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
const RESCAN_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Debounced paths and recovery state.
/// `rescan_retry = Some(t)` means a full scan is owed at `t`; ordinary flushes
/// stay suppressed so stale removals cannot erase partially rebuilt rows.
#[derive(Default)]
struct PendingEvents {
    modify: HashSet<PathBuf>,
    remove: HashSet<PathBuf>,
    rescan_retry: Option<Instant>,
    // Consecutive failed scans; reset only after successful recovery.
    rescan_attempts: u32,
}

impl PendingEvents {
    // Schedule and return the chosen delay, doubling up to a 64-second cap.
    fn schedule_rescan_retry(&mut self, now: Instant) -> Duration {
        let delay = RESCAN_RETRY_DELAY * (1 << self.rescan_attempts.min(6));
        self.rescan_attempts = self.rescan_attempts.saturating_add(1);
        self.rescan_retry = Some(now + delay);
        delay
    }
}

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
    let mut pending = PendingEvents::default();

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
                Ok(event) => handle_event(
                    &mount,
                    &store,
                    embedding.as_deref(),
                    rt_handle.as_ref(),
                    event,
                    &mut pending,
                ),
                Err(stdmpsc::RecvTimeoutError::Timeout) => break,
                Err(stdmpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }

        // Flush. The previous implementation also did a `full_scan` on
        // every flush to paper over notify missing newly-created subdir
        // children — but that walked the entire tree on every event,
        // which is O(N) per change. Per-directory rescan in `flush` for
        // events touching a directory is O(depth) and much cheaper.
        flush_pending(
            &mount,
            &store,
            embedding.as_deref(),
            rt_handle.as_ref(),
            &mut pending,
            Instant::now(),
        )?;
    }

    Ok(())
}

fn handle_event(
    mount: &MountPointLite,
    store: &Arc<Mutex<BM25Store>>,
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
    event: notify::Result<notify::Event>,
    pending: &mut PendingEvents,
) {
    match event {
        // notify reports a kernel queue overflow as a successful, pathless
        // Rescan event, so error handling and path classification cannot catch it.
        Ok(ev) if ev.need_rescan() => {
            tracing::warn!(root = %mount.root.display(), info = ?ev.info(), "watcher events lost; full rescan required");
        }
        Err(e) if is_overflow(&e) => {
            tracing::warn!(root = %mount.root.display(), error = %e, "watcher overflow detected; full rescan required");
        }
        Ok(ev) => {
            classify(mount, ev, &mut pending.modify, &mut pending.remove);
            return;
        }
        Err(e) => {
            tracing::warn!("watcher error: {e}");
            return;
        }
    }
    // Repeated overflow notifications must not bypass a failed scan's delay.
    if pending.rescan_retry.is_none() {
        recover(mount, store, embedding, rt_handle, pending);
    }
}

fn recover(
    mount: &MountPointLite,
    store: &Arc<Mutex<BM25Store>>,
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
    pending: &mut PendingEvents,
) {
    match full_scan(mount, store, embedding, rt_handle) {
        Ok(()) => {
            tracing::info!(
                root = %mount.root.display(),
                pending_modify = pending.modify.len(),
                pending_remove = pending.remove.len(),
                "index recovery completed; discarding superseded events"
            );
            pending.modify.clear();
            pending.remove.clear();
            pending.rescan_retry = None;
            pending.rescan_attempts = 0;
        }
        Err(e) => {
            let retry_delay = pending.schedule_rescan_retry(Instant::now());
            tracing::warn!(
                root = %mount.root.display(),
                error = %e,
                pending_modify = pending.modify.len(),
                pending_remove = pending.remove.len(),
                retry_delay_secs = retry_delay.as_secs(),
                "index recovery failed; retaining pending events until retry"
            );
        }
    }
}

fn flush_pending(
    mount: &MountPointLite,
    store: &Arc<Mutex<BM25Store>>,
    embedding: Option<&dyn EmbeddingProvider>,
    rt_handle: Option<&tokio::runtime::Handle>,
    pending: &mut PendingEvents,
    now: Instant,
) -> Result<()> {
    // Indexing errors must stay non-fatal here so the watcher keeps processing events.
    if let Some(retry_at) = pending.rescan_retry {
        if now >= retry_at {
            recover(mount, store, embedding, rt_handle, pending);
        }
    } else if !pending.modify.is_empty() || !pending.remove.is_empty() {
        flush(
            mount,
            store,
            embedding,
            rt_handle,
            &mut pending.modify,
            &mut pending.remove,
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns::Namespace;
    use notify::event::{Flag, ModifyKind};

    fn setup() -> (tempfile::TempDir, MountPointLite, Arc<Mutex<BM25Store>>) {
        let tmp = tempfile::tempdir().unwrap();
        let mount = MountPoint::ensure(Namespace::user("tester").unwrap(), tmp.path()).unwrap();
        let store = Arc::new(Mutex::new(BM25Store::open_in_memory().unwrap()));
        (tmp, mount.clone_lite(), store)
    }

    #[test]
    fn pathless_rescan_recovers_missed_changes_and_discards_pending_events() {
        let (_tmp, mount, store) = setup();
        {
            let mut store = store.lock().unwrap();
            store
                .upsert("deleted.md", 0, 20, "cobaltzebra deleted", None)
                .unwrap();
            store.upsert_vec("deleted.md", &[1.0, 0.0]).unwrap();
            store
                .upsert("changed.md", 0, 20, "cobaltzebra obsolete", None)
                .unwrap();
        }
        std::fs::write(mount.root.join("changed.md"), "heliotrope updated").unwrap();
        std::fs::write(mount.root.join("created.md"), "heliotrope created").unwrap();
        // A delete/recreate may straddle overflow; replaying the stale remove
        // after rebuilding would erase the current file from the fresh index.
        let mut pending = PendingEvents {
            modify: HashSet::from([mount.root.join("deleted.md")]),
            remove: HashSet::from([mount.root.join("created.md")]),
            ..Default::default()
        };
        let event = notify::Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert!(event.paths.is_empty());

        handle_event(&mount, &store, None, None, Ok(event), &mut pending);

        assert!(pending.modify.is_empty());
        assert!(pending.remove.is_empty());
        assert!(pending.rescan_retry.is_none());
        assert_eq!(pending.rescan_attempts, 0);
        let store = store.lock().unwrap();
        assert!(store.search("cobaltzebra", 10, false).unwrap().is_empty());
        assert!(store.search_vec(&[1.0, 0.0], 10).unwrap().is_empty());
        let mut paths: Vec<_> = store
            .search("heliotrope", 10, false)
            .unwrap()
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        paths.sort();
        assert_eq!(paths, ["changed.md", "created.md"]);
    }

    struct TestEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for TestEmbedding {
        async fn embed(&self, _text: &str) -> Result<crate::embedding::Embedding> {
            Ok(crate::embedding::Embedding {
                vector: vec![1.0, 0.0],
            })
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    #[test]
    fn rescan_retry_backoff_doubles_caps_and_saturates() {
        let mut pending = PendingEvents::default();
        let mut now = Instant::now();
        for (attempt, seconds) in [1, 2, 4, 8, 16, 32, 64, 64, 64].into_iter().enumerate() {
            let delay = Duration::from_secs(seconds);
            assert_eq!(pending.schedule_rescan_retry(now), delay);
            assert_eq!(pending.rescan_retry, Some(now + delay));
            assert_eq!(pending.rescan_attempts, attempt as u32 + 1);
            now += delay;
        }

        pending.rescan_attempts = u32::MAX;
        let delay = Duration::from_secs(64);
        assert_eq!(pending.schedule_rescan_retry(now), delay);
        assert_eq!(pending.rescan_retry, Some(now + delay));
        assert_eq!(pending.rescan_attempts, u32::MAX);
    }

    #[test]
    fn repeated_overflow_preserves_capped_retry_until_success() {
        let (_tmp, mount, store) = setup();
        let missed = mount.root.join("missed.md");
        std::fs::write(&missed, "heliotrope missed").unwrap();
        let mut pending = PendingEvents {
            rescan_attempts: 6,
            ..Default::default()
        };
        let now = Instant::now();
        pending.schedule_rescan_retry(now);
        let retry_at = now + Duration::from_secs(64);

        for event in [
            Ok(notify::Event::new(EventKind::Other).set_flag(Flag::Rescan)),
            Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch)),
            Err(notify::Error::io(std::io::Error::from_raw_os_error(
                nix::libc::EOVERFLOW,
            ))),
        ] {
            handle_event(&mount, &store, None, None, event, &mut pending);
            assert_eq!(pending.rescan_retry, Some(retry_at));
            assert_eq!(pending.rescan_attempts, 7);
            assert_eq!(store.lock().unwrap().count().unwrap(), 0);
        }

        flush_pending(
            &mount,
            &store,
            None,
            None,
            &mut pending,
            retry_at - Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(pending.rescan_retry, Some(retry_at));
        assert_eq!(pending.rescan_attempts, 7);
        assert_eq!(store.lock().unwrap().count().unwrap(), 0);

        // A due retry runs even with no buffered paths or new events.
        flush_pending(&mount, &store, None, None, &mut pending, retry_at).unwrap();
        assert!(pending.rescan_retry.is_none());
        assert_eq!(pending.rescan_attempts, 0);
        assert!(store.lock().unwrap().mtime_for("missed.md").is_some());
        assert_eq!(
            pending.schedule_rescan_retry(retry_at),
            Duration::from_secs(1)
        );
        assert_eq!(
            pending.rescan_retry,
            Some(retry_at + Duration::from_secs(1))
        );
    }

    #[test]
    fn failed_recovery_retains_events_and_retries_without_another_event() {
        for event in [
            Ok(notify::Event::new(EventKind::Other).set_flag(Flag::Rescan)),
            Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch)),
            Err(notify::Error::io(std::io::Error::from_raw_os_error(
                nix::libc::EOVERFLOW,
            ))),
        ] {
            let (_tmp, mount, _) = setup();
            let db_path = mount.meta_dir.join("recovery.db");
            let store = Arc::new(Mutex::new(
                BM25Store::open(&db_path, 0.01, 0.3, true).unwrap(),
            ));
            let conn = rusqlite::Connection::open(db_path).unwrap();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let embedding: Option<&dyn EmbeddingProvider> = Some(&TestEmbedding);
            let rt_handle = Some(runtime.handle());
            store
                .lock()
                .unwrap()
                .upsert("deleted.md", 0, 20, "cobaltzebra deleted", None)
                .unwrap();
            let recreated = mount.root.join("recreated.md");
            std::fs::write(&recreated, "heliotrope recreated").unwrap();
            let mut pending = PendingEvents {
                modify: HashSet::from([mount.root.join("deleted.md")]),
                remove: HashSet::from([recreated.clone()]),
                ..Default::default()
            };
            // Fail the vector query only after the scan has written BM25 rows.
            // A second connection exercises a real SQLite error without mocks.
            conn.execute_batch("ALTER TABLE files_vec RENAME TO unavailable_vec")
                .unwrap();
            // Refresh the store connection's schema cache before the failing query.
            assert_eq!(store.lock().unwrap().count().unwrap(), 1);
            assert!(store.lock().unwrap().paths_without_vec().is_err());

            handle_event(&mount, &store, embedding, rt_handle, event, &mut pending);

            let retry_at = pending.rescan_retry.expect("failed scan must retry");
            assert_eq!(pending.rescan_attempts, 1);
            assert!(store.lock().unwrap().mtime_for("recreated.md").is_some());
            assert_eq!(
                pending.modify,
                HashSet::from([mount.root.join("deleted.md")])
            );
            assert_eq!(pending.remove, HashSet::from([recreated.clone()]));

            // Ordinary events still get buffered while recovery is pending.
            let during_retry = mount.root.join("during-retry.md");
            std::fs::write(&during_retry, "heliotrope during retry").unwrap();
            handle_event(
                &mount,
                &store,
                embedding,
                rt_handle,
                Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
                    .add_path(during_retry.clone())),
                &mut pending,
            );
            assert!(pending.modify.contains(&during_retry));
            assert!(store.lock().unwrap().mtime_for("during-retry.md").is_none());

            // Another overflow must not bypass the delay while SQLite is broken.
            handle_event(
                &mount,
                &store,
                embedding,
                rt_handle,
                Ok(notify::Event::new(EventKind::Other).set_flag(Flag::Rescan)),
                &mut pending,
            );
            assert_eq!(pending.rescan_retry, Some(retry_at));
            assert_eq!(pending.rescan_attempts, 1);
            conn.execute_batch("ALTER TABLE unavailable_vec RENAME TO files_vec")
                .unwrap();

            // Replaying the old removal now would erase a partially rebuilt row.
            flush_pending(
                &mount,
                &store,
                embedding,
                rt_handle,
                &mut pending,
                retry_at - Duration::from_millis(1),
            )
            .unwrap();
            assert_eq!(pending.rescan_retry, Some(retry_at));
            assert_eq!(pending.remove, HashSet::from([recreated]));
            assert!(store.lock().unwrap().mtime_for("recreated.md").is_some());

            // Advance only the retry deadline, with no new filesystem event.
            flush_pending(&mount, &store, embedding, rt_handle, &mut pending, retry_at).unwrap();
            assert!(pending.rescan_retry.is_none());
            assert_eq!(pending.rescan_attempts, 0);
            assert!(pending.modify.is_empty());
            assert!(pending.remove.is_empty());
            {
                let store = store.lock().unwrap();
                assert!(store.mtime_for("deleted.md").is_none());
                assert!(store.mtime_for("recreated.md").is_some());
                assert!(store.mtime_for("during-retry.md").is_some());
                assert!(store.paths_without_vec().unwrap().is_empty());
                // MountPoint::ensure also seeds README.md.
                assert_eq!(store.search_vec(&[1.0, 0.0], 10).unwrap().len(), 3);
            }

            // Successful recovery restores ordinary debounce-only operation.
            let later = mount.root.join("later.md");
            std::fs::write(&later, "heliotrope later").unwrap();
            std::fs::write(mount.root.join("unannounced.md"), "unrelated").unwrap();
            handle_event(
                &mount,
                &store,
                embedding,
                rt_handle,
                Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(later)),
                &mut pending,
            );
            assert!(store.lock().unwrap().mtime_for("later.md").is_none());
            flush_pending(
                &mount,
                &store,
                embedding,
                rt_handle,
                &mut pending,
                Instant::now(),
            )
            .unwrap();
            let store = store.lock().unwrap();
            assert!(store.mtime_for("later.md").is_some());
            assert!(store.mtime_for("unannounced.md").is_none());
        }
    }

    #[test]
    fn failed_recovery_with_empty_buffers_retries() {
        let (_tmp, mount, _) = setup();
        let db_path = mount.meta_dir.join("recovery.db");
        let store = Arc::new(Mutex::new(
            BM25Store::open(&db_path, 0.01, 0.3, true).unwrap(),
        ));
        let conn = rusqlite::Connection::open(db_path).unwrap();
        std::fs::write(mount.root.join("missed.md"), "heliotrope missed").unwrap();
        conn.execute_batch("ALTER TABLE files RENAME TO unavailable_files")
            .unwrap();
        assert!(store.lock().unwrap().count().is_err());
        assert!(store.lock().unwrap().known_paths().is_err());
        let mut pending = PendingEvents::default();

        handle_event(
            &mount,
            &store,
            None,
            None,
            Ok(notify::Event::new(EventKind::Other).set_flag(Flag::Rescan)),
            &mut pending,
        );
        let retry_at = pending.rescan_retry.expect("failed scan must retry");
        assert_eq!(pending.rescan_attempts, 1);
        assert!(pending.modify.is_empty());
        assert!(pending.remove.is_empty());

        // A failed retry remains non-fatal and still requires another scan.
        let before_retry = Instant::now();
        flush_pending(&mount, &store, None, None, &mut pending, retry_at).unwrap();
        let after_retry = Instant::now();
        let retry_at = pending
            .rescan_retry
            .expect("failed retry must stay pending");
        assert_eq!(pending.rescan_attempts, 2);
        assert!(
            retry_at >= before_retry + Duration::from_secs(2),
            "the second consecutive recovery failure must delay its retry by two seconds"
        );
        assert!(retry_at <= after_retry + Duration::from_secs(2));
        conn.execute_batch("ALTER TABLE unavailable_files RENAME TO files")
            .unwrap();
        flush_pending(&mount, &store, None, None, &mut pending, retry_at).unwrap();
        assert!(pending.rescan_retry.is_none());
        assert_eq!(pending.rescan_attempts, 0);
        assert_eq!(
            store.lock().unwrap().known_paths().unwrap(),
            ["README.md", "missed.md"]
        );
    }

    #[test]
    fn ordinary_modify_remains_debounced_without_a_full_scan() {
        let (_tmp, mount, store) = setup();
        std::fs::write(mount.root.join("changed.md"), "heliotrope updated").unwrap();
        std::fs::write(mount.root.join("unannounced.md"), "heliotrope unrelated").unwrap();
        let changed = mount.root.join("changed.md");
        let mut pending = PendingEvents::default();
        let event =
            notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(changed.clone());

        handle_event(&mount, &store, None, None, Ok(event), &mut pending);

        assert_eq!(pending.modify, HashSet::from([changed]));
        assert!(pending.remove.is_empty());
        assert!(pending.rescan_retry.is_none());
        assert_eq!(store.lock().unwrap().count().unwrap(), 0);
        flush_pending(&mount, &store, None, None, &mut pending, Instant::now()).unwrap();
        assert_eq!(store.lock().unwrap().known_paths().unwrap(), ["changed.md"]);
    }
}
