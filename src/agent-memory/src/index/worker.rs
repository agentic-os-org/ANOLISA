use std::collections::{HashMap, HashSet};
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
use super::store::{BM25Store, IndexStat};

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

/// Whether `rel` — or a directory containing it — is queued for removal in
/// the same flush. `BM25Store::remove` deletes the exact path *and* every row
/// under `rel/`, so a queued removal of an ancestor directory invalidates
/// `rel` just as one for `rel` itself does.
fn removal_queued(queued_remove: &HashSet<String>, rel: &str) -> bool {
    !queued_remove.is_empty()
        && Path::new(rel).ancestors().any(|a| {
            let seg = a.as_os_str();
            !seg.is_empty() && queued_remove.contains(seg.to_str().unwrap_or(""))
        })
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

    // Phase 2 (lock-free): I/O — stat only. Walking the FS and re-reading
    // file bodies used to happen inside the store lock, which blocked every
    // concurrent `search` for the duration of the flush. Collecting tuples
    // here lets the lock-holding phase below stay a batched DB write.
    let mut to_remove: Vec<String> = pending_remove
        .drain()
        .filter_map(|p| relative(mount, &p))
        .collect();
    let mut to_remove_files: Vec<String> = Vec::new();
    let mut to_upsert: Vec<(String, i64, u64, String)> = Vec::new();
    let mut to_upsert_vec: Vec<(String, Vec<f32>)> = Vec::new();
    // The removals this same window queued. `classify` files an unlink under
    // `pending_remove` and a rename back onto the same path under
    // `pending_modify`, so a file deleted and restored inside one debounce
    // window appears in both — and phase 3 executes the removal *after* the
    // phase 2b snapshot was taken. A dedup guard that trusts that snapshot
    // would skip the upsert while the row is still deleted, leaving a file
    // that is readable on disk invisible to search until the next event or a
    // rescan. A queued removal therefore invalidates the snapshot; see
    // `removal_queued`. Empty for the events this dedup exists for (an
    // attribute change queues nothing for removal), where the check
    // short-circuits and costs nothing. Owned strings, because phase 2a still
    // appends paths that vanished before this flush to `to_remove`.
    let queued_remove: HashSet<String> = to_remove.iter().cloned().collect();

    // Phase 2a (lock-free): stat every pending path and keep the indexable
    // files together with the `(mtime, size)` the index would record.
    let mut statted: Vec<(String, i64, u64)> = Vec::new();
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
            to_remove_files.push(rel);
            continue;
        }
        statted.push((rel, super::store::mtime_ms_of(&meta), meta.len()));
    }

    // Phase 2b (one short locked window): read back what the index already
    // holds for those paths. Phase 1 expands a directory event into every
    // file under it, and a pure attribute change on the directory (`chmod
    // notes`, `touch notes`) therefore re-presents a whole subtree whose
    // content did not move by a single byte. Without this snapshot each of
    // those files was re-read *and* re-sent to the embedding provider, so a
    // `chmod -R` over a large tree burned embedding quota linearly in the
    // subtree size. `full_scan` already dedups on mtime; this brings the
    // incremental path in line with it. The mutex is held only for the
    // lookups — never across the body reads or the embedding calls below.
    let known: HashMap<String, IndexStat> = {
        let store = store.lock().unwrap_or_else(|e| e.into_inner());
        statted
            .iter()
            .filter_map(|(rel, _, _)| store.index_stat_for(rel).map(|st| (rel.clone(), st)))
            .collect()
    };

    // Phase 2c (lock-free): read bodies and embed only what still needs it.
    // If we have an embedding provider, use the tokio handle captured at
    // spawn time (the worker runs on a dedicated std::thread that has no
    // current runtime of its own). `handle.block_on` from a non-runtime
    // thread is the supported way to drive async work from sync context.
    // When no handle is available (e.g. tests without a runtime), embedding
    // is skipped — vector search then degrades to BM25, matching the
    // no-provider fallback.
    for (rel, mtime, size) in statted {
        let known = known.get(&rel);
        // The BM25 row is current when both mtime and size match. Size is
        // part of the comparison so a rewrite landing inside the mtime
        // granularity (coarse on some filesystems) is still picked up.
        // A removal queued for the same path in this window overrides that:
        // phase 3 is about to delete the very row the snapshot describes, so
        // the snapshot cannot vouch for anything. The file is re-read and
        // re-indexed, restoring the remove-then-reinsert this flush did before
        // the dedup landed.
        let row_current = !removal_queued(&queued_remove, &rel)
            && known.is_some_and(|k| k.mtime_ms == mtime && k.size == size);
        // A current row with no vector means an earlier embed failed, or the
        // provider was configured after the file was indexed. Heal that here
        // instead of deferring it to the next `full_scan` backfill — i.e. a
        // restart. With no provider configured there is nothing to heal.
        let vec_missing = embedding.is_some() && !known.is_some_and(|k| k.has_vector);
        if row_current && !vec_missing {
            continue;
        }
        let body = match crate::safe_fs::read_to_string(mount.root_fd.as_fd(), Path::new(&rel)) {
            Ok(b) => b,
            Err(MemoryError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                // A former text file may now contain binary data. Evict its
                // old content, while retaining entries on transient I/O errors.
                to_remove_files.push(rel);
                continue;
            }
            Err(e) => {
                tracing::warn!("index read failed for {rel}: {e}");
                continue;
            }
        };
        if !row_current {
            to_upsert.push((rel.clone(), mtime, size, body.clone()));
        }

        // Compute embedding for this file if provider is available: changed
        // content needs a fresh vector, and a current row that never got one
        // needs its first.
        if !row_current || vec_missing {
            if let Some(v) = embed_sync(embedding, rt_handle, &body) {
                to_upsert_vec.push((rel, v));
            }
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
    for rel in to_remove_files {
        match store.remove_file(&rel) {
            Ok(true) => tracing::info!("index entry evicted for {rel}: no longer indexable"),
            Ok(false) => {}
            Err(e) => tracing::warn!("index file removal failed for {rel}: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::Embedding;
    use crate::index::store::mtime_ms_of;
    use crate::ns::Namespace;
    use async_trait::async_trait;
    use std::fs::{self, FileTimes};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    /// Budget for waiting on one watcher flush. Mirrors the 4s the Tier B
    /// integration tests allow: notify on Linux often delivers Create/Modify
    /// back-to-back and the 200ms debounce can need a few cycles to drain.
    const WATCHER_BUDGET_MS: u64 = 4000;

    // Keep sibling eviction tests from registering the shared tracing callsite
    // without the capture subscriber: callsite interest is process-global even
    // though the subscriber installed below is thread-local.
    static EVICTION_LOG_LOCK: Mutex<()> = Mutex::new(());

    fn eviction_log_guard() -> std::sync::MutexGuard<'static, ()> {
        EVICTION_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn setup() -> (tempfile::TempDir, MountPointLite, Arc<Mutex<BM25Store>>) {
        let tmp = tempfile::tempdir().unwrap();
        let mount = MountPoint::ensure(Namespace::user("tester").unwrap(), tmp.path()).unwrap();
        let store = Arc::new(Mutex::new(BM25Store::open_in_memory().unwrap()));
        (tmp, mount.clone_lite(), store)
    }

    fn flush_path(mount: &MountPointLite, store: &Arc<Mutex<BM25Store>>, path: &str) {
        let mut modified = HashSet::from([mount.root.join(path)]);
        flush(mount, store, None, None, &mut modified, &mut HashSet::new()).unwrap();
    }

    fn seed_file(mount: &MountPointLite, store: &Arc<Mutex<BM25Store>>, path: &str) {
        std::fs::create_dir_all(mount.root.join(path).parent().unwrap()).unwrap();
        std::fs::write(mount.root.join(path), "cobaltzebra original content").unwrap();
        flush_path(mount, store, path);
        store.lock().unwrap().upsert_vec(path, &[1.0, 0.0]).unwrap();
    }

    fn assert_evicted_and_recoverable(
        mount: &MountPointLite,
        store: &Arc<Mutex<BM25Store>>,
        changed: &str,
        sibling: &str,
    ) {
        {
            let store = store.lock().unwrap();
            assert_eq!(store.known_paths().unwrap(), [sibling]);
            let hits = store.search("cobaltzebra", 10, false).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, sibling);
            let vectors = store.search_vec(&[1.0, 0.0], 10).unwrap();
            assert_eq!(vectors.len(), 1);
            assert_eq!(vectors[0].0, sibling);
        }

        std::fs::write(mount.root.join(changed), "heliotrope replacement").unwrap();
        flush_path(mount, store, changed);
        let store = store.lock().unwrap();
        let hits = store.search("heliotrope", 10, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, changed);
        assert_eq!(store.paths_without_vec().unwrap(), [changed]);
    }

    /// Embedding provider that only counts calls. The point of these tests is
    /// *whether* a file was re-embedded, so a call counter is the observable
    /// that matters — a re-embed is a paid HTTP round trip in production.
    struct CountingEmbedding {
        calls: AtomicUsize,
    }

    impl CountingEmbedding {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingProvider for CountingEmbedding {
        async fn embed(&self, _text: &str) -> Result<Embedding> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Embedding {
                vector: vec![0.5, 0.25],
            })
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, body).expect("write");
    }

    /// One flush pass over `rels`, mirroring what the watcher thread does
    /// after a debounce window. Paths are relative to the mount root; pass a
    /// directory to reproduce the phase-1 subtree expansion.
    fn flush_rels(
        mount: &MountPointLite,
        store: &Arc<Mutex<BM25Store>>,
        embedding: Option<&dyn EmbeddingProvider>,
        rels: &[&str],
    ) {
        let mut pending_modify: HashSet<PathBuf> =
            rels.iter().map(|r| mount.root.join(r)).collect();
        let mut pending_remove: HashSet<PathBuf> = HashSet::new();
        flush(
            mount,
            store,
            embedding,
            None,
            &mut pending_modify,
            &mut pending_remove,
        )
        .expect("flush");
    }

    fn stat(store: &Arc<Mutex<BM25Store>>, rel: &str) -> IndexStat {
        store
            .lock()
            .expect("lock")
            .index_stat_for(rel)
            .unwrap_or_else(|| panic!("{rel} should be indexed"))
    }

    #[test]
    fn flush_logs_only_actual_evictions() {
        let _eviction_lock = eviction_log_guard();
        struct LogWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for LogWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let logs = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&logs);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || LogWriter(Arc::clone(&output)))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let (_tmp, mount, store) = setup();
        seed_file(&mount, &store, "changed.md");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(mount.root.join("changed.md"))
            .unwrap();
        file.set_len(super::super::extractor::MAX_INDEXABLE_BYTES + 1)
            .unwrap();
        std::fs::write(mount.root.join("never-indexed.bin"), [0xff]).unwrap();

        flush_path(&mount, &store, "changed.md");
        flush_path(&mount, &store, "changed.md");
        flush_path(&mount, &store, "never-indexed.bin");

        let output = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("index entry evicted").count(), 1, "{output}");
        assert!(
            output.contains("changed.md: no longer indexable"),
            "{output}"
        );
        assert!(!output.contains("never-indexed.bin"), "{output}");
    }

    #[test]
    fn flush_evicts_oversized_text_and_vector() {
        let _eviction_lock = eviction_log_guard();
        let (_tmp, mount, store) = setup();
        seed_file(&mount, &store, "changed.md");
        seed_file(&mount, &store, "sibling.md");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(mount.root.join("changed.md"))
            .unwrap();
        file.set_len(super::super::extractor::MAX_INDEXABLE_BYTES + 1)
            .unwrap();

        flush_path(&mount, &store, "changed.md");

        assert_evicted_and_recoverable(&mount, &store, "changed.md", "sibling.md");
    }

    #[test]
    fn flush_evicts_non_utf8_text_and_vector() {
        let _eviction_lock = eviction_log_guard();
        let (_tmp, mount, store) = setup();
        seed_file(&mount, &store, "changed.md");
        seed_file(&mount, &store, "sibling.md");
        std::fs::write(mount.root.join("changed.md"), [0xff, 0xfe]).unwrap();

        flush_path(&mount, &store, "changed.md");

        assert_evicted_and_recoverable(&mount, &store, "changed.md", "sibling.md");
    }

    #[test]
    fn flush_ineligible_removal_matches_only_the_exact_file() {
        let _eviction_lock = eviction_log_guard();
        for (changed, sibling) in [
            ("report%.md", "report-2026.md/other.md"),
            ("report_.md", "report1.md/other.md"),
            ("report.md", "REPORT.md/other.md"),
        ] {
            let (_tmp, mount, store) = setup();
            seed_file(&mount, &store, changed);
            seed_file(&mount, &store, sibling);
            std::fs::write(mount.root.join(changed), [0xff, 0xfe]).unwrap();

            flush_path(&mount, &store, changed);

            assert_evicted_and_recoverable(&mount, &store, changed, sibling);
        }
    }

    #[test]
    fn flush_preserves_index_on_transient_read_error() {
        // Root can bypass file permissions, so this fixture requires an
        // ordinary user to exercise the read-error branch.
        if nix::unistd::Uid::effective().is_root() {
            eprintln!("skipped permission-error fixture: running as root");
            return;
        }
        let (_tmp, mount, store) = setup();
        seed_file(&mount, &store, "changed.md");
        let path = mount.root.join("changed.md");
        // Rewrite with a different length so the flush cannot skip this file on
        // the stored `(mtime, size)`: the fixture is about the read-error
        // branch, which the unchanged-file dedup would otherwise never reach.
        std::fs::write(&path, "cobaltzebra rewritten content").unwrap();
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let read_result =
            crate::safe_fs::read_to_string(mount.root_fd.as_fd(), Path::new("changed.md"));
        let mut modified = HashSet::from([path.clone()]);
        let flush_result = flush(
            &mount,
            &store,
            None,
            None,
            &mut modified,
            &mut HashSet::new(),
        );
        // Restore before assertions so a failure cannot leave the fixture unreadable.
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(read_result.is_err());
        flush_result.unwrap();

        let store = store.lock().unwrap();
        assert_eq!(store.search("cobaltzebra", 10, false).unwrap().len(), 1);
        assert_eq!(store.search_vec(&[1.0, 0.0], 10).unwrap().len(), 1);
    }

    /// A pure attribute change on a directory (`chmod notes`, `touch notes`)
    /// is expanded by phase 1 into every file below it. None of those files
    /// changed, so the flush must neither re-read nor re-embed them — that
    /// write amplification scaled with subtree size and burned embedding
    /// quota on bytes that never moved.
    #[tokio::test(flavor = "multi_thread")]
    async fn directory_attribute_event_leaves_an_unchanged_subtree_alone() {
        let (_tmp, mount, store) = setup();
        let root = mount.root.clone();
        write(&root, "notes/deep/a.md", "rust ownership");
        write(&root, "notes/b.md", "python gc");
        let emb = CountingEmbedding::new();

        flush_rels(
            &mount,
            &store,
            Some(&emb),
            &["notes/deep/a.md", "notes/b.md"],
        );
        assert_eq!(emb.calls(), 2, "both files index once");
        assert!(stat(&store, "notes/deep/a.md").has_vector);

        // The directory itself receives the event; phase 1 walks the subtree.
        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(
            emb.calls(),
            2,
            "unchanged files under the directory must not be re-embedded"
        );
    }

    /// The dedup has to stay a *dedup*: a file whose content really changed
    /// must still be re-read, re-indexed and re-embedded, and only that file.
    #[tokio::test(flavor = "multi_thread")]
    async fn one_changed_file_in_a_subtree_is_the_only_one_reindexed() {
        let (_tmp, mount, store) = setup();
        let root = mount.root.clone();
        write(&root, "notes/a.md", "rust ownership");
        write(&root, "notes/deep/b.md", "python gc");
        let emb = CountingEmbedding::new();

        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(emb.calls(), 2);

        write(
            &root,
            "notes/a.md",
            "rust ownership plus lifetimes, much longer",
        );
        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(emb.calls(), 3, "only the changed file is re-embedded");

        let hits = store
            .lock()
            .expect("lock")
            .search("lifetimes", 5, true)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "notes/a.md");
    }

    /// A row whose `(mtime, size)` matches but which has no vector means an
    /// earlier embed failed, or the provider was configured after the file
    /// was indexed. Matching metadata must not become an excuse to leave it
    /// un-embedded — otherwise the watcher would stop healing the gap and
    /// only the next `full_scan` backfill (i.e. a restart) would close it.
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_vector_is_reembedded_even_though_the_row_matches() {
        let (_tmp, mount, store) = setup();
        let root = mount.root.clone();
        write(&root, "notes/a.md", "rust ownership");

        // Indexed with no provider configured at all.
        flush_rels(&mount, &store, None, &["notes/a.md"]);
        assert!(!stat(&store, "notes/a.md").has_vector);

        let emb = CountingEmbedding::new();
        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(emb.calls(), 1, "the vector gap is healed");
        assert!(stat(&store, "notes/a.md").has_vector);

        // With the vector in place the same directory event is free again.
        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(emb.calls(), 1);
    }

    /// A brand-new file inside an expanded subtree has no row at all, so the
    /// snapshot lookup misses and it must be indexed on this very flush —
    /// catching newly-created files in nested dirs is why phase 1 walks the
    /// subtree in the first place.
    #[tokio::test(flavor = "multi_thread")]
    async fn new_file_in_an_expanded_subtree_is_indexed() {
        let (_tmp, mount, store) = setup();
        let root = mount.root.clone();
        write(&root, "notes/a.md", "rust ownership");
        let emb = CountingEmbedding::new();
        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(emb.calls(), 1);

        write(&root, "notes/observed/new.md", "borrow checker notes");
        flush_rels(&mount, &store, Some(&emb), &["notes"]);
        assert_eq!(emb.calls(), 2);
        let st = stat(&store, "notes/observed/new.md");
        assert!(st.has_vector);
        assert!(st.size > 0);
    }

    /// Stash `body` at `outside` — which must *not* be under the mount, or the
    /// watcher would see it — stamped with the mtime the index recorded, so
    /// that renaming it back onto the indexed path reproduces "same path, same
    /// `(mtime, size)`": exactly what the phase-2c dedup short-circuits on.
    fn stage_restore_copy(outside: &Path, body: &str, st: &IndexStat) {
        fs::write(outside, body).expect("stage copy");
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_millis(st.mtime_ms.max(0) as u64);
        let copy = fs::OpenOptions::new()
            .write(true)
            .open(outside)
            .expect("open copy");
        copy.set_times(FileTimes::new().set_modified(mtime))
            .expect("stamp copy with the indexed mtime");
        let meta = fs::metadata(outside).expect("stat copy");
        // Guard the fixture itself: were the stamp lost, the flush would
        // re-index for the ordinary "metadata moved" reason and the test would
        // stop covering the branch it names.
        assert_eq!(mtime_ms_of(&meta), st.mtime_ms, "copy mtime");
        assert_eq!(meta.len(), st.size, "copy size");
    }

    /// Block until the watcher has flushed at least once since `barrier` was
    /// written, i.e. until its row exists. Phase 3 removes before it upserts,
    /// so a flush that published the barrier has already applied everything
    /// queued ahead of it — no fixed multiple of the debounce window has to be
    /// guessed at.
    fn wait_for_flush_barrier(store: &Arc<Mutex<BM25Store>>, barrier: &str) -> bool {
        let deadline = Instant::now() + Duration::from_millis(WATCHER_BUDGET_MS);
        while Instant::now() < deadline {
            if store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index_stat_for(barrier)
                .is_some()
            {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// `classify` files an unlink under `pending_remove` and a rename back
    /// onto the same path under `pending_modify`, so one debounce window can
    /// queue a removal *and* a modification for a file that is still there.
    /// Phase 3 executes the removal after phase 2b took its snapshot, so the
    /// dedup must not trust that snapshot for such a path: skipping the upsert
    /// while the row is deleted anyway leaves a file that is readable on disk
    /// invisible to search until the next event or a rescan. Before the dedup
    /// landed, phase 3 removed and then reinserted it.
    #[test]
    fn queued_removal_of_a_restored_file_does_not_drop_its_row() {
        let (tmp, mount, store) = setup();
        let root = mount.root.clone();
        write(&root, "notes/a.md", "cobaltzebra original content");
        flush_rels(&mount, &store, None, &["notes/a.md"]);
        let indexed = stat(&store, "notes/a.md");

        let staged = tmp.path().join("restore.md");
        stage_restore_copy(&staged, "cobaltzebra original content", &indexed);
        fs::remove_file(root.join("notes/a.md")).expect("unlink");
        fs::rename(&staged, root.join("notes/a.md")).expect("restore");

        // Replay what the watcher collected for those two events.
        let mut pending_modify = HashSet::from([root.join("notes/a.md")]);
        let mut pending_remove = HashSet::from([root.join("notes/a.md")]);
        flush(
            &mount,
            &store,
            None,
            None,
            &mut pending_modify,
            &mut pending_remove,
        )
        .expect("flush");

        let hits = store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .search("cobaltzebra", 10, false)
            .expect("search");
        assert_eq!(hits.len(), 1, "the restored file must still be searchable");
        assert_eq!(hits[0].path, "notes/a.md");
        assert_eq!(stat(&store, "notes/a.md").mtime_ms, indexed.mtime_ms);
    }

    /// The same regression end to end through the real watcher with embedding
    /// disabled, which is how it was reported: index a file, keep a copy
    /// outside the mount that preserves its mtime, unlink the indexed file,
    /// rename the copy straight back. Both events land in one 200 ms debounce
    /// window. Afterwards the file must be readable *and* still return its
    /// keyword hit.
    #[test]
    fn watcher_keeps_a_file_deleted_and_restored_in_one_debounce_window() {
        let (tmp, mount, store) = setup();
        let root = mount.root.clone();
        write(&root, "notes/a.md", "cobaltzebra original content");

        // No embedding provider, as in the report: nothing but the BM25 row is
        // at stake, so there is no vector-side healing to mask the loss.
        let worker = IndexWorker::spawn(mount.clone(), Arc::clone(&store), None).expect("spawn");
        let indexed = stat(&store, "notes/a.md");
        let staged = tmp.path().join("restore.md");
        stage_restore_copy(&staged, "cobaltzebra original content", &indexed);

        // Back-to-back, so both events fall inside one debounce window.
        fs::remove_file(root.join("notes/a.md")).expect("unlink");
        fs::rename(&staged, root.join("notes/a.md")).expect("restore");

        write(&root, "notes/barrier.md", "quartz barrier marker");
        assert!(
            wait_for_flush_barrier(&store, "notes/barrier.md"),
            "watcher never flushed the barrier file"
        );
        worker.shutdown_blocking();

        assert_eq!(
            fs::read_to_string(root.join("notes/a.md")).expect("restored file is readable"),
            "cobaltzebra original content"
        );
        let hits = store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .search("cobaltzebra", 10, false)
            .expect("search");
        assert_eq!(hits.len(), 1, "the restored file must still be searchable");
        assert_eq!(hits[0].path, "notes/a.md");
    }
}
