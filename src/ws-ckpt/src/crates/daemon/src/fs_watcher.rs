use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::warn;

/// Monotonic watcher generation counter: every `start` increments it, so
/// tests can observe re-arming (a fresh watcher has a strictly larger seq)
/// without waiting on inotify event delivery.
static WATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    /// (dev, ino) of the directory the inotify watch was armed on, captured
    /// at `start` by following the registration symlink. inotify watches
    /// inodes, not paths: after a rollback swaps the live subvolume behind
    /// the registration path, this is how we detect the watch went stale.
    target: (u64, u64),
    watch_seq: u64,
}

/// Milliseconds on a monotonic clock anchored at first use in this process.
///
/// Wall-clock steps break a `fetch_max` watermark in both directions: a
/// backwards step (chrony `makestep`, container time sync, VM restore) pins
/// the watermark in the future, so later events can never raise it and every
/// check reports a spurious conflict until the wall clock catches up; a
/// forwards step larger than the window instantly declares an active writer
/// quiescent — the torn-snapshot window this guard exists to close. The
/// quiet-window math only ever compares two readings of the same clock, so a
/// monotonic anchor is sufficient and immune to steps.
pub(crate) fn activity_now_ms() -> u64 {
    static ANCHOR: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ANCHOR
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
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
        // Born "just active": every creation path (first init, daemon
        // rebuild, re-arm after rollback) must observe a full quiet window
        // before its first quiescence report, because writes landing before
        // the watch arms are invisible to inotify — the rollback window
        // between the subvolume swap and the re-arm is exactly that shape.
        let last_activity_ms = Arc::new(AtomicU64::new(activity_now_ms()));
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
        let activity = watcher.last_activity();
        // Simulate ongoing writes: a background task keeps the watermark
        // microseconds-fresh via busy-refresh + yield (no timer wake to be
        // starved — under tarpaulin's ~9x ptrace instrumentation a 10 ms
        // sleep can oversleep the 200 ms quiet window on a CPU-quota'd
        // runner, making a truthful quiescent verdict look like a failure).
        let activity_c = activity.clone();
        let handle = tokio::spawn(async move {
            loop {
                activity_c.fetch_max(activity_now_ms(), Ordering::Release);
                tokio::task::yield_now().await;
            }
        });
        activity.fetch_max(activity_now_ms(), Ordering::Release);
        // Bounded retry: the only residual failure mode after busy-refresh
        // is a full-thread freeze longer than the window, during which the
        // writer genuinely is not writing (a Safe verdict does not violate
        // the guard contract). A truly broken guard returns Safe on every
        // attempt and still fails.
        let mut quiescent_seen = true;
        for _ in 0..3 {
            if !watcher.check_quiescent().await {
                quiescent_seen = false;
                break;
            }
        }
        assert!(
            !quiescent_seen,
            "the guard must not report quiescent while the refresher keeps the watermark fresh"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn one_writers_close_does_not_clear_another_writers_activity() {
        // Writer A holds an open fd and writes continuously for the whole
        // test; writer B writes and closes once mid-way. A close must not
        // make the workspace look quiescent while A is still writing: the
        // quiet-window model keeps the checkpoint gated until A's events
        // stop landing. A writes every ~10 ms — the cadence
        // `not_quiescent_when_writing` uses — so the assertion cannot pass
        // by scheduling luck: a quiescent report would require a full 200 ms
        // window in which none of A's writes landed.
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let a_path = dir.path().join("a.log");
        {
            let mut a = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&a_path)
                .unwrap();
            std::io::Write::write_all(&mut a, b"burst-0").unwrap();
        }
        let a_path = Arc::new(a_path);
        let writer = tokio::spawn(async move {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(a_path.as_ref())
                .unwrap();
            loop {
                std::io::Write::write_all(&mut file, b"burst").unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        // Let a few of A's writes land, then writer B: opens, writes, closes.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        std::fs::write(dir.path().join("b.log"), b"x").unwrap();
        let mut quiescent_seen = true;
        for _ in 0..3 {
            if !watcher.check_quiescent().await {
                quiescent_seen = false;
                break;
            }
        }
        assert!(
            !quiescent_seen,
            "a close from writer B must not approve a checkpoint while writer A keeps writing"
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

    #[tokio::test]
    async fn each_start_gets_a_strictly_larger_seq() {
        let dir = tempfile::tempdir().unwrap();
        let a = WorkspaceWatcher::start(dir.path()).unwrap();
        let b = WorkspaceWatcher::start(dir.path()).unwrap();
        assert!(b.watch_seq() > a.watch_seq(), "seq must be monotonic");
    }
}
