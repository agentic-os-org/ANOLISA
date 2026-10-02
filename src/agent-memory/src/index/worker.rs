use std::collections::HashSet;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc as stdmpsc};
use std::thread;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher, recommended_watcher};

use crate::embedding::EmbeddingProvider;
use crate::error::{MemoryError, Result};
use crate::ns::MountPoint;

use super::extractor::{extract_text, is_indexable};
use super::store::BM25Store;

const DEBOUNCE_MS: u64 = 200;

/// Background indexer: full-scan on start, then incremental updates driven
/// by `notify` filesystem events. Runs on a dedicated `std::thread` (the
/// notify channel is sync, so we don't pay the price of a tokio bridge).
pub struct IndexWorker {
    handle: Option<thread::JoinHandle<()>>,
    /// Sender to signal shutdown; the watcher thread polls for it.
    cancel_tx: stdmpsc::Sender<()>,
}

impl IndexWorker {
    /// Synchronously perform the initial full-scan AND wait for the
    /// inotify/FSEvents watcher to be registered before returning. By the
    /// time this completes, every existing file is indexed and any
    /// subsequent write to the mount tree will be picked up by the watcher.
    pub fn spawn(
        mount: MountPointLite,
        store: Arc<Mutex<BM25Store>>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<Self> {
        // Capture the tokio runtime handle from the spawning thread (which
        // runs inside the server's runtime) so the worker's dedicated
        // std::thread can drive async embedding calls via `handle.block_on`.
        // `Handle::try_current()` returns None on a plain std::thread with no
        // runtime context — which is exactly why the worker previously never
        // produced any vectors. Capturing here (on the runtime thread) is the
        // fix; the worker thread itself has no current runtime.
        let rt_handle = tokio::runtime::Handle::try_current().ok();

        // 1. Sync full scan so the caller can safely read svc.index.count()
        full_scan(&mount, &store, embedding.as_deref(), rt_handle.as_ref())?;

        // 2. Spawn watcher thread. Use a oneshot mpsc to know when the
        //    watcher has been wired up.
        let (cancel_tx, cancel_rx) = stdmpsc::channel::<()>();
        let (ready_tx, ready_rx) = stdmpsc::sync_channel::<Result<()>>(1);
        let mount_clone = mount.clone();
        let store_clone = Arc::clone(&store);
        let emb_clone = embedding.clone();
        let rt_clone = rt_handle.clone();

        let handle = thread::Builder::new()
            .name("agentmem-index".into())
            .spawn(move || {
                if let Err(e) = run_watcher(
                    mount_clone,
                    store_clone,
                    emb_clone,
                    rt_clone,
                    cancel_rx,
                    ready_tx,
                ) {
                    tracing::warn!("index watcher exited: {e}");
                }
            })
            .map_err(|e| MemoryError::Other(format!("spawn index thread: {e}")))?;

        // 3. Block until watcher is registered (or fails); reasonable timeout.
        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                tracing::warn!("watcher took >3s to become ready; continuing without sync barrier");
            }
        }

        Ok(Self {
            handle: Some(handle),
            cancel_tx,
        })
    }

    pub fn shutdown_blocking(mut self) {
        let _ = self.cancel_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Lightweight clone of MountPoint that the worker can own across threads.
#[derive(Clone)]
pub struct MountPointLite {
    pub root: PathBuf,
    pub meta_dir: PathBuf,
    pub meta_dir_name: String,
    /// Arc-wrapped root fd so MountPointLite is Clone without OwnedFd
    /// (OwnedFd is not Clone; Arc makes the fd shareable across threads).
    pub root_fd: Arc<OwnedFd>,
}

impl MountPoint {
    pub fn clone_lite(&self) -> MountPointLite {
        // Canonicalize so watcher event paths (which arrive in canonical form
        // match what strip_prefix expects (defensive against symlinked
        // tmpfs roots).
        let root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let meta_dir = self
            .meta_dir
            .canonicalize()
            .unwrap_or_else(|_| self.meta_dir.clone());
        MountPointLite {
            root,
            meta_dir,
            meta_dir_name: self.meta_dir_name().to_string(),
            root_fd: Arc::new(self.root_fd.try_clone().expect("root_fd dup")),
        }
    }
}

fn run_watcher(
    mount: MountPointLite,
    store: Arc<Mutex<BM25Store>>,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
    rt_handle: Option<tokio::runtime::Handle>,
    cancel_rx: stdmpsc::Receiver<()>,
    ready_tx: stdmpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let (event_tx, event_rx) = stdmpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match recommended_watcher(move |res| {
        let _ = event_tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            let err = MemoryError::Other(format!("watcher init: {e}"));
            let _ = ready_tx.send(Err(MemoryError::Other(err.to_string())));
            return Err(err);
        }
    };

    if let Err(e) = watcher.watch(&mount.root, RecursiveMode::Recursive) {
        let err = MemoryError::Other(format!("watch: {e}"));
        let _ = ready_tx.send(Err(MemoryError::Other(err.to_string())));
        return Err(err);
    }

    // Watcher is now armed; signal readiness to the spawner.
    let _ = ready_tx.send(Ok(()));

    // Debounce buffer: track unique paths touched since last flush
    let mut pending_modify: HashSet<PathBuf> = HashSet::new();
    let mut pending_remove: HashSet<PathBuf> = HashSet::new();

    loop {
        // Cancellation check (non-blocking)
        if cancel_rx.try_recv().is_ok() {
            break;
        }

        // Pump events for up to DEBOUNCE_MS
        let deadline = Instant::now() + Duration::from_millis(DEBOUNCE_MS);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let timeout = deadline - now;
            match event_rx.recv_timeout(timeout) {
                Ok(Ok(ev)) => {
                    classify(&mount, ev, &mut pending_modify, &mut pending_remove);
                }
                Ok(Err(e)) => {
                    if is_overflow(&e) {
                        tracing::warn!("inotify overflow detected; triggering full rescan");
                        full_scan(&mount, &store, embedding.as_deref(), rt_handle.as_ref())?;
                        pending_modify.clear();
                        pending_remove.clear();
                    } else {
                        tracing::warn!("watcher error: {e}");
                    }
                }
                Err(stdmpsc::RecvTimeoutError::Timeout) => break,
                Err(stdmpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }

        // Flush. The previous implementation also did a `full_scan` on
        // every flush to paper over notify missing newly-created subdir
        // children — but that walked the entire tree on every event,
        // which is O(N) per change. Per-directory rescan in `flush` for
        // events touching a directory is O(depth) and much cheaper.
        if !pending_modify.is_empty() || !pending_remove.is_empty() {
            flush(
                &mount,
                &store,
                embedding.as_deref(),
                rt_handle.as_ref(),
                &mut pending_modify,
                &mut pending_remove,
            )?;
        }
    }

    Ok(())
}

fn classify(
    mount: &MountPointLite,
    ev: notify::Event,
    pending_modify: &mut HashSet<PathBuf>,
    pending_remove: &mut HashSet<PathBuf>,
) {
    let kind = ev.kind;
    for path in ev.paths {
        // Skip events inside .anolisa/
        if is_under_meta(mount, &path) {
            continue;
        }
        match kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                pending_modify.insert(path);
            }
            EventKind::Remove(_) => {
                pending_remove.insert(path);
            }
            _ => {}
        }
    }
}

fn flush(
    mount: &MountPointLite,
    store: &Arc<Mutex<BM25Store>>,
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
    pending_modify: &mut HashSet<PathBuf>,
    pending_remove: &mut HashSet<PathBuf>,
) -> Result<()> {
    // Phase 1 (lock-free): turn directory events into a recursive walk
    // so we don't miss freshly-created files inside nested subdirs.
    // Linux inotify can miss Create events for files inside a
    // newly-created subdir whose watch hasn been wired up yet; a
    // max_depth(1) sweep only catches direct children, not deeper
    // nesting (e.g. notes/observed/<ulid>.md under notes/). Walking
    // the full subtree is still O(new files) because only directories
    // that received events are expanded, not the entire mount tree.
    let mut expanded: HashSet<PathBuf> = HashSet::new();
    for path in pending_modify.iter() {
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !is_under_meta(mount, e.path()))
                .flatten()
                .filter(|e| e.file_type().is_file())
            {
                expanded.insert(entry.path().to_path_buf());
            }
        }
    }
    pending_modify.extend(expanded);

    // Phase 2 (lock-free): I/O — stat + extract text. Walking the FS and
    // re-reading file bodies used to happen inside the store lock, which
    // blocked every concurrent `search` for the duration of the flush.
    // Collecting tuples here lets the lock-holding phase below be just a
    // batched DB write.
    let mut to_remove: Vec<String> = pending_remove
        .drain()
        .filter_map(|p| relative(mount, &p))
        .collect();
    let mut to_upsert: Vec<(String, i64, u64, String)> = Vec::new();
    let mut to_upsert_vec: Vec<(String, Vec<f32>)> = Vec::new();

    // If we have an embedding provider, use the tokio handle captured at
    // spawn time (the worker runs on a dedicated std::thread that has no
    // current runtime of its own). `handle.block_on` from a non-runtime
    // thread is the supported way to drive async work from sync context.
    // When no handle is available (e.g. tests without a runtime), embedding
    // is skipped — vector search then degrades to BM25, matching the
    // no-provider fallback.

    for path in pending_modify.drain() {
        let rel = match relative(mount, &path) {
            Some(r) => r,
            None => continue,
        };
        let rel_path = Path::new(&rel);
        let meta = match crate::safe_fs::metadata(mount.root_fd.as_fd(), rel_path) {
            Ok(m) => m,
            Err(_) => {
                // File may have been deleted between event and flush.
                to_remove.push(rel);
                continue;
            }
        };
        if !meta.is_file() {
            continue;
        }
        if !is_indexable(rel_path, meta.len()) {
            continue;
        }
        let body = match extract_text(mount.root_fd.as_fd(), rel_path) {
            Some(b) => b,
            None => continue,
        };
        let mtime = super::store::mtime_ms_of(&meta);
        to_upsert.push((rel.clone(), mtime, meta.len(), body.clone()));

        // Compute embedding for this file if provider is available.
        if let Some(v) = embed_sync(embedding, rt_handle, &body) {
            to_upsert_vec.push((rel, v));
        }
    }

    // Phase 3 (short locked window): batched DB writes. We hold the
    // mutex only here, so concurrent `search` callers see at most one
    // small batch of upserts/removes per debounce window instead of the
    // entire walk+extract pass.
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    let agent_id = std::env::var("MCP_CLIENT_NAME").ok();
    for rel in to_remove {
        if let Err(e) = store.remove(&rel) {
            tracing::warn!("index remove failed for {rel}: {e}");
        }
    }
    for (rel, mtime, size, body) in to_upsert {
        if let Err(e) = store.upsert(&rel, mtime, size, &body, agent_id.as_deref()) {
            tracing::warn!("index upsert failed for {rel}: {e}");
        }
    }
    for (rel, vec) in to_upsert_vec {
        if let Err(e) = store.upsert_vec(&rel, &vec) {
            tracing::warn!("index vector upsert failed for {rel}: {e}");
        }
    }

    Ok(())
}

fn full_scan(
    mount: &MountPointLite,
    store: &Arc<Mutex<BM25Store>>,
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
) -> Result<()> {
    use walkdir::WalkDir;

    {
        let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen: HashSet<String> = HashSet::new();
        let agent_id = std::env::var("MCP_CLIENT_NAME").ok();

        for entry in WalkDir::new(&mount.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_under_meta(mount, e.path()))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = match relative(mount, path) {
                Some(r) => r,
                None => continue,
            };
            let rel_path = Path::new(&rel);
            let meta = match crate::safe_fs::metadata(mount.root_fd.as_fd(), rel_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            if !is_indexable(rel_path, meta.len()) {
                continue;
            }
            let body = match extract_text(mount.root_fd.as_fd(), rel_path) {
                Some(b) => b,
                None => continue,
            };
            let mtime = super::store::mtime_ms_of(&meta);
            // Skip if already indexed with same mtime
            if let Some(known) = store.mtime_for(&rel) {
                if known == mtime {
                    seen.insert(rel.clone());
                    continue;
                }
            }
            if let Err(e) = store.upsert(&rel, mtime, meta.len(), &body, agent_id.as_deref()) {
                tracing::warn!("index full-scan upsert failed for {rel}: {e}");
            }
            seen.insert(rel);
        }

        // Remove entries no longer on disk
        let known = store.known_paths()?;
        for p in known {
            if !seen.contains(&p) {
                if let Err(e) = store.remove(&p) {
                    tracing::warn!("index full-scan remove failed for {p}: {e}");
                }
            }
        }
    } // store lock released before the (potentially slow) embedding backfill

    // Backfill dense embeddings for files that are BM25-indexed but have no
    // vector — e.g. pre-existing files at startup, or files recovered after
    // an inotify overflow. Without this, vector/hybrid search would miss any
    // file that never received a modify event after the provider was enabled.
    backfill_vectors(mount, store, embedding, rt_handle)?;

    Ok(())
}

/// Compute and store embeddings for every indexed file that lacks one.
/// The embedding HTTP call happens outside the store mutex so concurrent
/// searches are not blocked; only the brief path-query and final vec-write
/// hold the lock. No-op when no provider is configured.
fn backfill_vectors(
    mount: &MountPointLite,
    store: &Arc<Mutex<BM25Store>>,
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
) -> Result<()> {
    if embedding.is_none() {
        return Ok(());
    }
    // Phase 1 (locked, brief): which indexed paths lack a vector?
    let missing: Vec<String> = {
        let store = store.lock().unwrap_or_else(|e| e.into_inner());
        store.paths_without_vec()?
    };
    if missing.is_empty() {
        return Ok(());
    }

    // Phase 2 (lock-free): read body + embed. Holding the mutex across an
    // embedding HTTP call would block every concurrent search.
    let mut to_upsert: Vec<(String, Vec<f32>)> = Vec::new();
    for rel in missing {
        let rel_path = Path::new(&rel);
        let body = match extract_text(mount.root_fd.as_fd(), rel_path) {
            Some(b) => b,
            None => continue,
        };
        if let Some(v) = embed_sync(embedding, rt_handle, &body) {
            to_upsert.push((rel, v));
        }
    }

    // Phase 3 (locked): write vectors.
    if !to_upsert.is_empty() {
        let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
        for (rel, v) in to_upsert {
            if let Err(e) = store.upsert_vec(&rel, &v) {
                tracing::warn!("index full-scan vec upsert failed for {rel}: {e}");
            }
        }
    }
    Ok(())
}

/// Drive an async embedding call from sync context, picking the blocking
/// strategy for the current thread.
///
/// On a tokio runtime thread (the startup full_scan, called from within
/// the server's `block_on`), `block_in_place` makes a nested
/// `Handle::current().block_on` legal instead of panicking with
/// "Cannot start a runtime from within a runtime". On the index worker's
/// plain std::thread, drive via the runtime handle captured at spawn time
/// (`block_on` from a non-runtime thread).
///
/// Returns `None` when no provider is configured or no runtime is available
/// (tests outside a runtime); vector search then degrades to BM25, matching
/// the no-provider fallback.
fn embed_sync(
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
    text: &str,
) -> Option<Vec<f32>> {
    let emb = embedding?;
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(emb.embed(text)))
            .ok()
            .map(|e| e.vector)
    } else if let Some(handle) = rt_handle {
        handle.block_on(emb.embed(text)).ok().map(|e| e.vector)
    } else {
        None
    }
}

fn is_under_meta(mount: &MountPointLite, path: &Path) -> bool {
    // notify event paths may not match `mount.meta_dir` byte-for-byte
    // (bind mounts, /var → /private/var on macOS, symlinked tmpfs).
    // Canonicalize before comparing so .anolisa/ events don't leak into
    // the index just because of a path-form mismatch.
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canon.starts_with(&mount.meta_dir) || crate::safe_fs::is_under_git(&canon, &mount.root)
}

fn relative(mount: &MountPointLite, path: &Path) -> Option<String> {
    path.strip_prefix(&mount.root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Detect inotify/FSEvents overflow errors that indicate the kernel
/// dropped events and the index is stale. Requires a full rescan to
/// recover synchronization.
fn is_overflow(e: &notify::Error) -> bool {
    match &e.kind {
        notify::ErrorKind::Io(io_err) => {
            // Linux inotify returns ENOSPC when the max user watches
            // limit is hit, and the kernel logs "inotify: overflow".
            matches!(
                io_err.raw_os_error(),
                Some(nix::libc::ENOSPC) | Some(nix::libc::EOVERFLOW)
            )
        }
        notify::ErrorKind::MaxFilesWatch => true,
        _ => false,
    }
}
