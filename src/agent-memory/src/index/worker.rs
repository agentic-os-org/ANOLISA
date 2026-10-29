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
    /// Register the watcher before the synchronous initial full scan.
    /// Changes made during the scan or embedding backfill are queued for
    /// incremental indexing, just like writes made after startup returns.
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

        // Arm before scanning: embedding backfill may take multiple network
        // round trips, during which another client can change the shared store.
        let (event_tx, event_rx) = stdmpsc::channel::<notify::Result<notify::Event>>();
        let watcher = watch_mount(&mount, event_tx)?;

        // Preserve synchronous initial indexing. If it fails, the local
        // watcher is dropped without starting an orphan worker thread.
        full_scan(&mount, &store, embedding.as_deref(), rt_handle.as_ref())?;

        let (cancel_tx, cancel_rx) = stdmpsc::channel::<()>();
        let mount_clone = mount.clone();
        let store_clone = Arc::clone(&store);
        let emb_clone = embedding.clone();
        let rt_clone = rt_handle.clone();

        let handle = thread::Builder::new()
            .name("agentmem-index".into())
            .spawn(move || {
                let _watcher = watcher;
                if let Err(e) = run_watcher(
                    mount_clone,
                    store_clone,
                    emb_clone,
                    rt_clone,
                    cancel_rx,
                    event_rx,
                ) {
                    tracing::warn!("index watcher exited: {e}");
                }
            })
            .map_err(|e| MemoryError::Other(format!("spawn index thread: {e}")))?;

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

fn watch_mount(
    mount: &MountPointLite,
    event_tx: stdmpsc::Sender<notify::Result<notify::Event>>,
) -> Result<notify::RecommendedWatcher> {
    let mut watcher = recommended_watcher(move |res| {
        let _ = event_tx.send(res);
    })
    .map_err(|e| MemoryError::Other(format!("watcher init: {e}")))?;
    watcher
        .watch(&mount.root, RecursiveMode::Recursive)
        .map_err(|e| MemoryError::Other(format!("watch: {e}")))?;
    // Keep startup SQLite writes out of the event queue instead of discarding
    // them later in classify. IndexHandle::open already created this subtree.
    watcher
        .unwatch(&mount.meta_dir)
        .map_err(|e| MemoryError::Other(format!("unwatch meta: {e}")))?;
    Ok(watcher)
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
    event_rx: stdmpsc::Receiver<notify::Result<notify::Event>>,
) -> Result<()> {
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
    // subtree size. `full_scan` uses the same `(mtime, size)` fingerprint.
    // The mutex is held only for the lookups — never across the body reads
    // or the embedding calls below.
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
    let mut failed_text = HashSet::new();
    for (rel, mtime, size, body) in to_upsert {
        if let Err(e) = store.upsert(&rel, mtime, size, &body, agent_id.as_deref()) {
            tracing::warn!("index upsert failed for {rel}: {e}");
            failed_text.insert(rel);
        }
    }
    for (rel, vec) in to_upsert_vec {
        // A rolled-back text update must retain its old vector too; a failed
        // insert must not leave a standalone vector for unindexed content.
        if failed_text.contains(&rel) {
            continue;
        }
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

    let mut failed_text = HashSet::new();
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
            // Match the incremental fingerprint: timestamps alone can survive
            // a rewrite, for example when a restored file retains its mtime.
            // Same-mtime, same-size rewrites remain undetected.
            if let Some(known) = store.index_stat_for(&rel) {
                if known.mtime_ms == mtime && known.size == meta.len() {
                    seen.insert(rel.clone());
                    continue;
                }
            }
            if let Err(e) = store.upsert(&rel, mtime, meta.len(), &body, agent_id.as_deref()) {
                tracing::warn!("index full-scan upsert failed for {rel}: {e}");
                failed_text.insert(rel.clone());
            }
            seen.insert(rel);
        }

        // Each known path is a file row. A stale file may now be a directory
        // whose children were just indexed, so remove only the exact row.
        let known = store.known_paths()?;
        for p in known {
            if !seen.contains(&p) {
                if let Err(e) = store.remove_file(&p) {
                    tracing::warn!("index full-scan remove failed for {p}: {e}");
                }
            }
        }
    } // store lock released before the (potentially slow) embedding backfill

    // Backfill dense embeddings for files that are BM25-indexed but have no
    // vector — e.g. pre-existing files at startup, or files recovered after
    // an inotify overflow. Without this, vector/hybrid search would miss any
    // file that never received a modify event after the provider was enabled.
    backfill_vectors(mount, store, embedding, rt_handle, &failed_text)?;

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
    failed_text: &HashSet<String>,
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
        // The disk body may be newer than the row whose update just failed.
        // Leave that row without a vector until its text can be reconciled.
        if failed_text.contains(&rel) {
            continue;
        }
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
    use notify::event::{Flag, ModifyKind};
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

    #[test]
    fn watcher_excludes_startup_database_writes() {
        use notify::event::{AccessKind, AccessMode};

        let (_tmp, mount, _) = setup();
        let index_dir = mount.meta_dir.join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let store = Arc::new(Mutex::new(
            BM25Store::open(&index_dir.join("bm25.db"), 0.0, 0.0, false).unwrap(),
        ));
        let notes_dir = mount.root.join("notes");
        std::fs::create_dir(&notes_dir).unwrap();
        for i in 0..16 {
            std::fs::write(notes_dir.join(format!("{i}.md")), "cobaltzebra").unwrap();
        }
        let (event_tx, event_rx) = stdmpsc::channel();
        let _watcher = watch_mount(&mount, event_tx).unwrap();

        let wal_path = index_dir.join("bm25.db-wal");
        let wal_before = std::fs::metadata(&wal_path).unwrap().len();
        full_scan(&mount, &store, None, None).unwrap();
        assert_eq!(
            store
                .lock()
                .unwrap()
                .search("cobaltzebra", 20, false)
                .unwrap()
                .len(),
            16
        );
        assert!(std::fs::metadata(&wal_path).unwrap().len() > wal_before);

        let external = notes_dir.join("0.md");
        std::fs::write(&external, "heliotrope").unwrap();
        let sentinel = mount.root.join("sentinel.md");
        std::fs::write(&sentinel, "finished").unwrap();

        // inotify preserves order: the sentinel's close follows the WAL writes
        // and external modification, so absence is checked without a sleep.
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut external_seen = false;
        loop {
            let event = event_rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("missing sentinel event")
                .expect("watcher error");
            assert!(
                !event
                    .paths
                    .iter()
                    .any(|path| path.starts_with(&mount.meta_dir)),
                "internal state reached the event queue: {event:?}"
            );
            if matches!(event.kind, EventKind::Modify(_)) && event.paths.contains(&external) {
                external_seen = true;
            }
            if event.kind == EventKind::Access(AccessKind::Close(AccessMode::Write))
                && event.paths.contains(&sentinel)
            {
                break;
            }
        }
        assert!(external_seen, "external file modification was not watched");
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_text_write_preserves_other_vectors_in_same_flush() {
        let (_tmp, mount, _) = setup();
        let db = mount.meta_dir.join("batch.db");
        let store = Arc::new(Mutex::new(BM25Store::open(&db, 0.0, 0.0, false).unwrap()));
        for path in ["failed.md", "healthy.md"] {
            seed_file(&mount, &store, path);
            write(&mount.root, path, "heliotrope replacement");
        }
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_text_update BEFORE UPDATE ON files
             WHEN OLD.path = 'failed.md'
             BEGIN SELECT RAISE(ABORT, 'controlled text failure'); END;",
        )
        .unwrap();
        let embedding = CountingEmbedding::new();
        flush_rels(
            &mount,
            &store,
            Some(&embedding),
            &["failed.md", "healthy.md"],
        );
        let store = store.lock().unwrap();
        assert_eq!(embedding.calls(), 2);
        assert_eq!(
            store.search("cobaltzebra", 10, false).unwrap()[0].path,
            "failed.md"
        );
        assert_eq!(
            store.search("heliotrope", 10, false).unwrap()[0].path,
            "healthy.md"
        );
        let vectors = store.search_vec(&[1.0, 0.0], 10).unwrap();
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors.iter().find(|v| v.0 == "failed.md").unwrap().1, 1.0);
        assert_eq!(vectors.iter().find(|v| v.0 == "healthy.md").unwrap().1, 0.5);
    }

    #[test]
    fn full_scan_reopen_preserves_children_of_replaced_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = MountPoint::ensure(Namespace::user("tester").unwrap(), tmp.path()).unwrap();
        let path = mount.root.join("notes");
        fs::write(&path, "cobaltzebra old file").unwrap();
        let original = super::super::IndexHandle::open(&mount, None, 0.0, 0.0, false).unwrap();
        original
            .store
            .lock()
            .unwrap()
            .upsert_vec("notes", &[1.0, 0.0])
            .unwrap();
        assert_eq!(original.search("cobaltzebra", 10).unwrap().len(), 1);
        drop(original);

        // This replacement happens while the service is stopped, so reopening
        // must reconcile the persistent rows without relying on watcher events.
        fs::remove_file(&path).unwrap();
        write(&mount.root, "notes/child.md", "heliotrope new child");
        write(
            &mount.root,
            "notes/deep/grandchild.md",
            "heliotrope nested child",
        );
        let reopened = super::super::IndexHandle::open(&mount, None, 0.0, 0.0, false).unwrap();
        assert_eq!(reopened.search("heliotrope", 10).unwrap().len(), 2);
        assert!(reopened.search("cobaltzebra", 10).unwrap().is_empty());
        assert!(reopened.search_vec(&[1.0, 0.0], 10).unwrap().is_empty());
        assert!(
            reopened
                .store
                .lock()
                .unwrap()
                .index_stat_for("notes")
                .is_none()
        );
    }

    #[test]
    fn full_scan_keeps_existing_child_text_and_vector_when_pruning_ancestor() {
        let (_tmp, mount, store) = setup();
        seed_file(&mount, &store, "notes/child.md");
        // A tool may already have indexed a descendant before the next scan
        // has reconciled the previous file at the ancestor path.
        store
            .lock()
            .unwrap()
            .upsert("notes", 0, 1, "old ancestor", None)
            .unwrap();
        store
            .lock()
            .unwrap()
            .upsert_vec("notes", &[1.0, 0.0])
            .unwrap();
        full_scan(&mount, &store, None, None).unwrap();
        let store = store.lock().unwrap();
        assert!(store.index_stat_for("notes").is_none());
        assert_eq!(
            store.search("cobaltzebra", 10, false).unwrap()[0].path,
            "notes/child.md"
        );
        let vectors = store.search_vec(&[1.0, 0.0], 10).unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].0, "notes/child.md");
    }

    #[test]
    fn full_scan_still_prunes_all_files_in_deleted_directory() {
        let (_tmp, mount, store) = setup();
        for path in ["notes/a.md", "notes/deep/b.md", "notes-alike/keep.md"] {
            seed_file(&mount, &store, path);
        }
        fs::remove_dir_all(mount.root.join("notes")).unwrap();
        full_scan(&mount, &store, None, None).unwrap();
        let store = store.lock().unwrap();
        assert_eq!(
            store.known_paths().unwrap(),
            ["README.md", "notes-alike/keep.md"]
        );
        let hits = store.search("cobaltzebra", 10, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "notes-alike/keep.md");
        let vectors = store.search_vec(&[1.0, 0.0], 10).unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].0, "notes-alike/keep.md");
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
