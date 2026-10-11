use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::warn;

/// A checkpoint may only run after this much time has passed with no
/// write-relevant inotify event (Create/Modify/Remove/CLOSE_WRITE).
pub(crate) const QUIET_WINDOW_MS: u64 = 200;

/// Recursive workspace write watcher. Quiescence is a *time* window since the
/// last write-relevant event, never a latch that an event can clear: a
/// CLOSE_WRITE means data was just modified, and one writer closing must not
/// hide another writer's in-flight writes.
pub struct WorkspaceWatcher {
    last_activity_ms: Arc<AtomicU64>,
    /// Hold the watcher so its background thread stays alive; dropping the
    /// struct cancels the watch and unblocks the forwarding task.
    _watcher: RecommendedWatcher,
    workspace_path: PathBuf,
}

/// Milliseconds since the Unix epoch, saturating to 0 if the clock is behind.
pub(crate) fn activity_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Quiescent only after a full quiet window with no write-relevant event —
/// pure so the boundary is unit-testable.
pub(crate) fn quiescent_after(last_activity_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(last_activity_ms) >= QUIET_WINDOW_MS
}

/// Wait out (at most) one quiet window and report whether the workspace has
/// been write-quiescent throughout it. Shared by `WorkspaceWatcher` and the
/// daemon's per-workspace state check.
pub(crate) async fn wait_quiescent(last_activity_ms: &AtomicU64) -> bool {
    if quiescent_after(last_activity_ms.load(Ordering::Acquire), activity_now_ms()) {
        return true;
    }
    // Wait out the remainder of the quiet window, then re-check: an event
    // landing in the meantime refreshes the timestamp and fails the check.
    tokio::time::sleep(std::time::Duration::from_millis(QUIET_WINDOW_MS)).await;
    quiescent_after(last_activity_ms.load(Ordering::Acquire), activity_now_ms())
}

impl WorkspaceWatcher {
    /// Start watching a workspace directory (recursively) for write activity.
    /// Called after workspace init or during rebuild_from_disk.
    pub fn start(workspace_path: &Path) -> anyhow::Result<Self> {
        let last_activity_ms = Arc::new(AtomicU64::new(0));
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

        let activity = last_activity_ms.clone();
        let log_path = path.clone();
        tokio::spawn(async move {
            while let Some(res) = rx.recv().await {
                match res {
                    Ok(event) => match &event.kind {
                        // Every write-relevant event refreshes activity. In
                        // particular a write-close is activity (data was just
                        // modified), not the end of all activity: a single
                        // latch cleared by any close used to approve
                        // checkpoints while another writer was mid-write.
                        EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                            activity.fetch_max(activity_now_ms(), Ordering::Release);
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
            last_activity_ms,
            _watcher: watcher,
            workspace_path: path,
        })
    }

    /// Check if workspace is quiescent (no write-relevant event for a full
    /// quiet window). Returns true if safe to snapshot, false if writes are
    /// active.
    pub async fn check_quiescent(&self) -> bool {
        wait_quiescent(&self.last_activity_ms).await
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

    /// Get a clone of the last-activity timestamp for external quiescence
    /// checks.
    pub fn last_activity(&self) -> Arc<AtomicU64> {
        self.last_activity_ms.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiescent_after_requires_the_full_quiet_window() {
        assert!(quiescent_after(0, QUIET_WINDOW_MS));
        assert!(quiescent_after(1000, 1000 + QUIET_WINDOW_MS));
        // One millisecond short of the window is still active.
        assert!(!quiescent_after(1000, 1000 + QUIET_WINDOW_MS - 1));
        // A late clock (now before the event) must not wrap around.
        assert!(!quiescent_after(2000, 1000));
    }

    #[tokio::test]
    async fn start_valid_dir() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        assert_eq!(watcher.workspace_path(), dir.path());
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
        let activity = watcher.last_activity();
        // Simulate ongoing writes: a background task keeps refreshing the
        // last-activity timestamp.
        let activity_c = activity.clone();
        let handle = tokio::spawn(async move {
            loop {
                activity_c.fetch_max(activity_now_ms(), Ordering::Release);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        activity.fetch_max(activity_now_ms(), Ordering::Release);
        assert!(!watcher.check_quiescent().await);
        handle.abort();
    }

    #[tokio::test]
    async fn one_writers_close_does_not_clear_another_writers_activity() {
        // Writer A holds an open fd and writes a burst around t=250ms; writer
        // B writes and closes at ~t=50ms. A close must not make the workspace
        // look quiescent while A is still writing: the quiet-window model
        // keeps the checkpoint gated until A's events stop landing.
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let a_path = dir.path().join("a.log");
        let mut a = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&a_path)
            .unwrap();
        std::io::Write::write_all(&mut a, b"burst-0").unwrap();
        // Writer B: opens, writes, closes at ~t=50ms.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(dir.path().join("b.log"), b"x").unwrap();
        // Let the events land, then check while A is between bursts.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Writer A's next bursts.
        let a = Arc::new(a);
        let writer = tokio::spawn(async move {
            for delay in [150u64, 40, 100] {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                let mut file = a.clone();
                std::io::Write::write_all(&mut file, b"burst").unwrap();
            }
        });
        assert!(
            !watcher.check_quiescent().await,
            "a close from writer B must not approve a checkpoint while writer A is mid-burst"
        );
        writer.abort();
    }

    #[tokio::test]
    async fn stop_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        watcher.stop();
        assert_eq!(watcher.workspace_path(), dir.path());
    }
}
