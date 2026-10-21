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
    //
    // A completed walk also makes the directory a prune candidate:
    // replacing a whole directory (e.g. snapshot restore, which arrives
    // as a single Create/Modify event for the directory) would otherwise
    // leave index entries of the *old* tree's children searchable
    // forever (issue #4634). Entries under a pruned directory that the
    // walk did not find are removed. If the walk hit an I/O error we
    // skip pruning so unreadable files are not mistaken for deleted ones.
    use walkdir::WalkDir;

    let mut expanded: HashSet<PathBuf> = HashSet::new();
    let mut prune_dirs: Vec<String> = Vec::new();
    let mut walked_files: HashSet<String> = HashSet::new();
    for path in pending_modify.iter() {
        if path.is_dir() {
            let mut walk_complete = true;
            let mut found_files: Vec<PathBuf> = Vec::new();
            for entry in WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !is_under_meta(mount, e.path()))
            {
                match entry {
                    Ok(e) => {
                        if e.file_type().is_file() {
                            found_files.push(e.path().to_path_buf());
                        }
                    }
                    Err(_) => walk_complete = false,
                }
            }
            for file_path in found_files {
                match relative(mount, &file_path) {
                    Some(rel) => {
                        walked_files.insert(rel);
                    }
                    // A walked file we cannot name relative to the mount
                    // would be missing from `walked_files` while its
                    // directory is still registered as a prune candidate —
                    // i.e. a live entry could be deleted. Treat the walk as
                    // incomplete so the prune is skipped entirely.
                    None => walk_complete = false,
                }
                expanded.insert(file_path);
            }
            if walk_complete {
                if let Some(rel_dir) = relative(mount, path) {
                    // The mount root is never a prune candidate — retiring
                    // entries at the root is `full_scan`'s job. An empty
                    // rel_dir would build the prefix "/" and match nothing.
                    if !rel_dir.is_empty() {
                        prune_dirs.push(rel_dir);
                    }
                }
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

    // Prune stale children of fully-walked directories (issue #4634).
    // The prefix match is literal Rust string comparison: `notes/`
    // prunes `notes/x.md` but never `notes2/x.md`, and directory names
    // containing `_` or `%` are not treated as SQL LIKE wildcards
    // (see #3797). Candidates are scoped to each subtree with a range
    // scan on the `path` index (`known_paths_under`) instead of reading
    // the whole index, and the deletes are batched into one transaction.
    if !prune_dirs.is_empty() {
        // A batch routinely contains a directory and its subdir (a subdir
        // create/remove also bumps the parent's link count, which notify
        // surfaces as Modify(Metadata)). Pruning the ancestor already
        // covers the whole subtree, so drop entries under a kept prefix.
        prune_dirs.sort();
        let mut kept: Vec<String> = Vec::new();
        for d in prune_dirs {
            if !kept
                .iter()
                .any(|k| d == *k || d.starts_with(&format!("{k}/")))
            {
                kept.push(d);
            }
        }
        let mut stale: Vec<String> = Vec::new();
        for d in &kept {
            let under = match store.known_paths_under(d) {
                Ok(v) => v,
                Err(e) => {
                    // `flush` runs on every debounce window; a transient
                    // SQLite error here must not kill the watcher thread.
                    // The upserts/removes above already landed, so skip
                    // this prune and let the next window retry.
                    tracing::warn!("index prune skipped: {e}");
                    return Ok(());
                }
            };
            // Also consider `d` itself: a file that was later replaced by
            // this directory is not covered by the `d/` range, but its row
            // is stale (Phase 2 skips it because it is no longer a file).
            for p in under.into_iter().chain(std::iter::once(d.clone())) {
                if walked_files.contains(&p) {
                    continue;
                }
                // A row can appear between the Phase-1 walk and this prune
                // (IndexHandle::reindex_file from memory_observe, #1462).
                // Never delete a path that still exists on disk as a file;
                // a path that exists only as a directory is a stale
                // file-replaced-by-dir row and must go.
                if crate::safe_fs::metadata(mount.root_fd.as_fd(), Path::new(&p))
                    .map(|m| m.is_file())
                    .unwrap_or(false)
                {
                    continue;
                }
                stale.push(p);
            }
        }
        if let Err(e) = store.remove_many(&stale) {
            tracing::warn!("index prune failed for {} paths: {e}", stale.len());
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A mount tree backed by a tempdir, with the `.anolisa` meta dir
    /// pre-created so `is_under_meta` filtering behaves like production.
    fn fixture() -> (tempfile::TempDir, MountPointLite) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonicalize root");
        fs::create_dir(root.join(".anolisa")).expect("create meta dir");
        let root_fd: OwnedFd = fs::File::open(&root).expect("open root dir fd").into();
        let mount = MountPointLite {
            meta_dir: root.join(".anolisa"),
            meta_dir_name: ".anolisa".to_string(),
            root: root.clone(),
            root_fd: Arc::new(root_fd),
        };
        (dir, mount)
    }

    fn new_store() -> Arc<Mutex<BM25Store>> {
        Arc::new(Mutex::new(
            BM25Store::open_in_memory().expect("in-memory store"),
        ))
    }

    fn index(store: &Arc<Mutex<BM25Store>>, rel: &str) {
        let mut s = store.lock().unwrap();
        s.upsert(rel, 0, 10, "stale body", None)
            .expect("seed upsert");
    }

    fn paths(store: &Arc<Mutex<BM25Store>>) -> Vec<String> {
        let mut p = store.lock().unwrap().known_paths().expect("known_paths");
        p.sort();
        p
    }

    fn run_flush(mount: &MountPointLite, store: &Arc<Mutex<BM25Store>>, modify: Vec<PathBuf>) {
        let mut pm: HashSet<PathBuf> = modify.into_iter().collect();
        let mut pr: HashSet<PathBuf> = HashSet::new();
        flush(mount, store, None, None, &mut pm, &mut pr).expect("flush");
    }

    #[test]
    fn flush_prunes_stale_children_of_replaced_directory() {
        // Issue #4634: a snapshot restore replaces `notes/` wholesale and
        // arrives as a single directory event. The old tree's `notes/b.md`
        // is gone from disk but was still searchable; a completed walk of
        // the directory must prune it.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes")).unwrap();
        fs::write(mount.root.join("notes/a.md"), "alpha").unwrap();

        let store = new_store();
        index(&store, "notes/a.md");
        index(&store, "notes/b.md");

        run_flush(&mount, &store, vec![mount.root.join("notes")]);

        assert_eq!(paths(&store), vec!["notes/a.md".to_string()]);
    }

    #[test]
    fn flush_prune_does_not_touch_sibling_with_shared_prefix() {
        // `notes/` must not prune `notes2/` — prefix match is on `notes/`
        // with the trailing separator.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes")).unwrap();
        fs::create_dir_all(mount.root.join("notes2")).unwrap();
        fs::write(mount.root.join("notes/new.md"), "new").unwrap();
        fs::write(mount.root.join("notes2/x.md"), "x").unwrap();

        let store = new_store();
        index(&store, "notes/old.md");
        index(&store, "notes2/x.md");

        run_flush(&mount, &store, vec![mount.root.join("notes")]);

        assert_eq!(
            paths(&store),
            vec!["notes/new.md".to_string(), "notes2/x.md".to_string()]
        );
    }

    #[test]
    fn flush_prune_prefix_is_literal_not_like_wildcard() {
        // #3797: SQL LIKE treats `_` as a single-char wildcard, so a naive
        // `LIKE 'a_b/%'` prune would also delete `axb/` entries. The Rust
        // starts_with match must be literal.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("a_b")).unwrap();
        fs::create_dir_all(mount.root.join("axb")).unwrap();
        fs::write(mount.root.join("a_b/new.md"), "new").unwrap();
        fs::write(mount.root.join("axb/keep.md"), "keep").unwrap();

        let store = new_store();
        index(&store, "a_b/old.md");
        index(&store, "axb/keep.md");

        run_flush(&mount, &store, vec![mount.root.join("a_b")]);

        assert_eq!(
            paths(&store),
            vec!["a_b/new.md".to_string(), "axb/keep.md".to_string()]
        );
    }

    #[test]
    fn flush_skips_prune_when_directory_walk_is_incomplete() {
        // An unreadable subdir makes the walk incomplete; pruning would
        // then delete entries that merely could not be stat'd. Root
        // ignores mode bits, so skip under uid 0.
        if unsafe { nix::libc::geteuid() } == 0 {
            return;
        }
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes/secret")).unwrap();
        fs::write(mount.root.join("notes/a.md"), "alpha").unwrap();
        fs::write(mount.root.join("notes/secret/b.md"), "beta").unwrap();

        let store = new_store();
        index(&store, "notes/a.md");
        index(&store, "notes/stale.md");

        let secret = mount.root.join("notes/secret");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_flush(&mount, &store, vec![mount.root.join("notes")]);
        }));
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o755)).unwrap();
        result.expect("flush with unreadable subdir");

        let p = paths(&store);
        assert!(
            p.contains(&"notes/stale.md".to_string()),
            "incomplete walk must not prune, got {p:?}"
        );
    }

    #[test]
    fn flush_file_only_event_does_not_prune() {
        // A plain file modify event must not trigger subtree pruning for
        // the directory that contains it.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes")).unwrap();
        fs::write(mount.root.join("notes/a.md"), "alpha").unwrap();
        fs::write(mount.root.join("notes/b.md"), "beta").unwrap();

        let store = new_store();
        index(&store, "notes/a.md");
        index(&store, "notes/b.md");

        run_flush(&mount, &store, vec![mount.root.join("notes/b.md")]);

        assert_eq!(
            paths(&store),
            vec!["notes/a.md".to_string(), "notes/b.md".to_string()]
        );
    }

    #[test]
    fn flush_prune_keeps_row_written_after_walk() {
        // #1462 race: a row written between the Phase-1 walk and the
        // Phase-3 prune (memory_observe -> IndexHandle::reindex_file) is
        // absent from `walked_files` while its file is on disk. The prune
        // must not delete it. Reproduce the "on disk but not walked" state
        // deterministically by pointing the meta-dir filter at the whole
        // root, so the walk yields no files yet still completes.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonicalize root");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("notes/late.md"), "late").unwrap();
        let root_fd: OwnedFd = fs::File::open(&root).expect("open root dir fd").into();
        let mount = MountPointLite {
            meta_dir: root.clone(),
            meta_dir_name: ".anolisa".to_string(),
            root,
            root_fd: Arc::new(root_fd),
        };

        let store = new_store();
        index(&store, "notes/late.md");
        index(&store, "notes/gone.md");

        run_flush(&mount, &store, vec![mount.root.join("notes")]);

        assert_eq!(paths(&store), vec!["notes/late.md".to_string()]);
    }

    #[test]
    fn flush_prune_removes_row_for_file_replaced_by_directory() {
        // A file `notes` that was later replaced by a directory is stale,
        // but the `notes/` range does not cover the exact path. It must be
        // removed without cascading into the live children the walk just
        // indexed.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes")).unwrap();
        fs::write(mount.root.join("notes/a.md"), "alpha").unwrap();

        let store = new_store();
        index(&store, "notes");
        index(&store, "notes/a.md");

        run_flush(&mount, &store, vec![mount.root.join("notes")]);

        assert_eq!(paths(&store), vec!["notes/a.md".to_string()]);
    }

    #[test]
    fn flush_prune_nested_dirs_in_one_batch_keeps_live_files() {
        // A subdir event also bumps the parent's link count, so one batch
        // can carry `notes` and `notes/observed`. Pruning the ancestor
        // covers the whole subtree: both stale rows go, both live rows stay.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes/observed")).unwrap();
        fs::write(mount.root.join("notes/a.md"), "alpha").unwrap();
        fs::write(mount.root.join("notes/observed/c.md"), "gamma").unwrap();

        let store = new_store();
        index(&store, "notes/a.md");
        index(&store, "notes/old.md");
        index(&store, "notes/observed/c.md");
        index(&store, "notes/observed/old.md");

        run_flush(
            &mount,
            &store,
            vec![mount.root.join("notes"), mount.root.join("notes/observed")],
        );

        assert_eq!(
            paths(&store),
            vec!["notes/a.md".to_string(), "notes/observed/c.md".to_string()]
        );
    }

    #[test]
    fn flush_prune_makes_stale_body_unsearchable() {
        // #4634's user-visible symptom is "still searchable": the prune
        // must cascade through files_fts (and files_vec), not just drop
        // the `files` row.
        let (_tmp, mount) = fixture();
        fs::create_dir_all(mount.root.join("notes")).unwrap();
        fs::write(mount.root.join("notes/a.md"), "alpha").unwrap();

        let store = new_store();
        {
            let mut s = store.lock().unwrap();
            s.upsert("notes/a.md", 0, 10, "alpha", None)
                .expect("seed a.md");
            s.upsert("notes/b.md", 0, 10, "zzprunetoken body", None)
                .expect("seed b.md");
        }

        run_flush(&mount, &store, vec![mount.root.join("notes")]);

        let hits = store
            .lock()
            .unwrap()
            .search("zzprunetoken", 5, false)
            .expect("search");
        assert!(
            hits.iter().all(|h| h.path != "notes/b.md"),
            "pruned entry must not come back from search: {hits:?}"
        );
    }
}
