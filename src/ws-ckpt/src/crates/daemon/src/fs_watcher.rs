use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::warn;

/// Monotonic watcher generation counter: every `start` increments it, so
/// tests can observe re-arming (a fresh watcher has a strictly larger seq)
/// without waiting on inotify event delivery.
static WATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Recursive workspace write watcher. CLOSE_WRITE clears the flag so
/// checkpoint can skip the quiescence wait when all writers have closed.
pub struct WorkspaceWatcher {
    is_writing: Arc<AtomicBool>,
    /// Hold the watcher so its background thread stays alive; dropping the
    /// struct cancels the watch and unblocks the forwarding task.
    _watcher: RecommendedWatcher,
    workspace_path: PathBuf,
    /// (dev, ino) of the directory the inotify watch was armed on, captured
    /// at `start` by following the registration symlink. inotify watches
    /// inodes, not paths: after a rollback swaps the live subvolume behind
    /// the registration path, this is how we detect the watch went stale.
    target: (u64, u64),
    watch_seq: u64,
}

impl WorkspaceWatcher {
    /// Start watching a workspace directory (recursively) for write activity.
    /// Called after workspace init or during rebuild_from_disk.
    pub fn start(workspace_path: &Path) -> anyhow::Result<Self> {
        let is_writing = Arc::new(AtomicBool::new(false));
        let path = workspace_path.to_path_buf();

        // Forward notify events from the (sync) watcher callback into a tokio
        // task via an unbounded channel. The channel is closed automatically
        // when the watcher is dropped, which ends the forwarder.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();

        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                // Best-effort: if the receiver is gone we simply drop the event.
                let _ = tx.send(res);
            })
            .map_err(|e| anyhow::anyhow!("Failed to init notify watcher for {:?}: {}", path, e))?;

        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|e| anyhow::anyhow!("Failed to add recursive watch for {:?}: {}", path, e))?;

        // std::fs::metadata FOLLOWS symlinks, so this is the directory
        // inode inotify actually observes — the identity to compare against
        // after the backend swaps the live subvolume behind the path.
        let meta = std::fs::metadata(&path)
            .map_err(|e| anyhow::anyhow!("cannot resolve watch target for {:?}: {}", path, e))?;

        let writing = is_writing.clone();
        let log_path = path.clone();
        tokio::spawn(async move {
            while let Some(res) = rx.recv().await {
                match res {
                    Ok(event) => match &event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                            writing.store(true, Ordering::Release);
                        }
                        EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                            writing.store(false, Ordering::Release);
                        }
                        _ => {}
                    },
                    Err(e) => {
                        warn!("notify error for {:?}: {}", log_path, e);
                    }
                }
            }
        });

        Ok(Self {
            is_writing,
            _watcher: watcher,
            workspace_path: path,
            target: (meta.dev(), meta.ino()),
            watch_seq: WATCH_SEQ.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// The (dev, ino) this watch is armed on.
    pub fn target_identity(&self) -> (u64, u64) {
        self.target
    }

    /// Monotonic generation of this watcher (for observing re-arms).
    pub fn watch_seq(&self) -> u64 {
        self.watch_seq
    }

    /// Check if workspace is quiescent (no recent writes).
    /// Returns true if safe to snapshot, false if writes are active.
    pub async fn check_quiescent(&self) -> bool {
        if !self.is_writing.load(Ordering::Acquire) {
            return true;
        }
        // Wait 100ms quiet period
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // Reset and check again
        self.is_writing.store(false, Ordering::Release);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        !self.is_writing.load(Ordering::Acquire)
    }

    /// Stop watching. Called when workspace is deleted.
    ///
    /// With the `notify`-based implementation, resource cleanup happens
    /// automatically when this `WorkspaceWatcher` is dropped (the inner
    /// watcher's backend thread stops and the forwarder task exits). This
    /// method is retained for API compatibility and is a no-op.
    pub fn stop(&self) {}

    /// Get the watched workspace path.
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    /// Get a clone of the is_writing flag for external quiescence checks.
    pub fn is_writing_flag(&self) -> Arc<AtomicBool> {
        self.is_writing.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn start_valid_dir() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        assert_eq!(watcher.workspace_path(), dir.path());
    }

    #[tokio::test]
    async fn watcher_targets_the_resolved_registration_path() {
        // Registration layout: a symlink pointing at the live directory.
        // std::fs::metadata follows it, so the recorded target identity must
        // be the LIVE directory's (dev, ino) — the inode inotify observes.
        use std::os::unix::fs::MetadataExt;
        let base = tempfile::tempdir().unwrap();
        let live = base.path().join("live");
        std::fs::create_dir(&live).unwrap();
        let link = base.path().join("ws");
        std::os::unix::fs::symlink(&live, &link).unwrap();

        let watcher = WorkspaceWatcher::start(&link).unwrap();
        let live_meta = std::fs::metadata(&live).unwrap();
        assert_eq!(
            watcher.target_identity(),
            (live_meta.dev(), live_meta.ino()),
            "the watch must be pinned to the symlink target's inode"
        );
    }

    #[tokio::test]
    async fn quiescent_when_idle() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        assert!(watcher.check_quiescent().await);
    }

    #[tokio::test]
    async fn not_quiescent_when_writing() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let flag = watcher.is_writing_flag();
        // Simulate ongoing writes: a background task keeps setting the flag
        let flag_c = flag.clone();
        let handle = tokio::spawn(async move {
            loop {
                flag_c.store(true, Ordering::Release);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        flag.store(true, Ordering::Release);
        assert!(!watcher.check_quiescent().await);
        handle.abort();
    }

    #[tokio::test]
    async fn stop_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        watcher.stop();
        assert_eq!(watcher.workspace_path(), dir.path());
    }

    #[tokio::test]
    async fn each_start_gets_a_strictly_larger_seq() {
        let dir = tempfile::tempdir().unwrap();
        let a = WorkspaceWatcher::start(dir.path()).unwrap();
        let b = WorkspaceWatcher::start(dir.path()).unwrap();
        assert!(b.watch_seq() > a.watch_seq(), "seq must be monotonic");
    }
}
