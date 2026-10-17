//! The daemon's file watchers must share one inotify instance.
//!
//! `notify::recommended_watcher` opens one `inotify_init` per watcher, and
//! Linux caps instances per user (`fs.inotify.max_user_instances`, default
//! 128). One watcher per workspace made the ~129th registration fail while
//! the daemon only warned and kept going, silently dropping change
//! detection for most of a large deployment (#7233). These tests run in
//! their own process on purpose: the lib tests also arm watchers and share
//! a process, which would blur the `/proc/self/fd` instance count.

use std::ffi::OsStr;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ws_ckpt_daemon::fs_watcher::WorkspaceWatcher;

/// Count this process's open inotify instances.
fn inotify_instances() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd is available on Linux")
        .filter_map(|entry| {
            let target = std::fs::read_link(entry.ok()?.path()).ok()?;
            Some(target == OsStr::new("anon_inode:inotify"))
        })
        .filter(|is_inotify| *is_inotify)
        .count()
}

/// Poll `flag` until it becomes `want`, panicking after a deadline.
async fn eventually(flag: &AtomicBool, want: bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while flag.load(Ordering::Acquire) != want {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} (flag={})",
            flag.load(Ordering::Acquire)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Hold a file open and write to it, so no CLOSE_WRITE lands while the
/// write-activity flag is being observed.
fn write_held(path: std::path::PathBuf) -> std::fs::File {
    let mut held = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    held.write_all(b"data").unwrap();
    held
}

#[tokio::test]
async fn many_workspaces_share_one_inotify_instance() {
    let before = inotify_instances();
    let dirs: Vec<_> = (0..24).map(|_| tempfile::tempdir().unwrap()).collect();
    let watchers: Vec<_> = dirs
        .iter()
        .map(|dir| WorkspaceWatcher::start(dir.path()).unwrap())
        .collect();
    let during = inotify_instances();
    drop(watchers);
    drop(dirs);

    assert!(
        during - before <= 2,
        "24 workspaces must share one inotify instance instead of opening one each: \
         fs.inotify.max_user_instances defaults to 128 and the daemon silently \
         loses change detection past it (#7233) [{} -> {}]",
        before,
        during
    );
}

#[tokio::test]
async fn dropping_every_watcher_leaves_at_most_one_instance_behind() {
    let before = inotify_instances();
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let watchers: Vec<_> = dirs
        .iter()
        .map(|dir| WorkspaceWatcher::start(dir.path()).unwrap())
        .collect();
    drop(watchers);
    // Give the notify backend a beat to settle its unwatch bookkeeping.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = inotify_instances();
    drop(dirs);

    assert!(
        after <= before + 1,
        "dropped watchers must release their inotify instances [{} -> {}]",
        before,
        after
    );
}

#[tokio::test]
async fn events_route_to_the_owning_workspace() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = WorkspaceWatcher::start(dir_a.path()).unwrap();
    let b = WorkspaceWatcher::start(dir_b.path()).unwrap();

    let _held = write_held(dir_a.path().join("held.log"));

    let a_flag = a.is_writing_flag();
    let b_flag = b.is_writing_flag();
    eventually(&a_flag, true, "workspace A to observe its own writes").await;
    assert!(
        !b_flag.load(Ordering::Acquire),
        "workspace B must not observe A's writes"
    );
}

#[tokio::test]
async fn overlapping_roots_flag_every_containing_workspace() {
    // Parity with one-instance-per-workspace observers: an event inside an
    // overlapping root was seen by both watchers before, and must be again.
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
    let inner_watcher = WorkspaceWatcher::start(&inner).unwrap();

    let _held = write_held(inner.join("held.log"));

    let inner_flag = inner_watcher.is_writing_flag();
    let outer_flag = outer_watcher.is_writing_flag();
    eventually(&inner_flag, true, "the inner workspace to observe writes").await;
    eventually(
        &outer_flag,
        true,
        "the overlapping outer workspace to observe the same writes",
    )
    .await;
}

#[tokio::test]
async fn replacing_a_watcher_on_the_same_path_keeps_events_flowing() {
    // A re-arm registers the fresh watcher before the stale handle drops;
    // the survivor must keep receiving events for the shared root.
    let dir = tempfile::tempdir().unwrap();
    let stale = WorkspaceWatcher::start(dir.path()).unwrap();
    let fresh = WorkspaceWatcher::start(dir.path()).unwrap();
    drop(stale);

    let _held = write_held(dir.path().join("held.log"));

    let fresh_flag = fresh.is_writing_flag();
    eventually(
        &fresh_flag,
        true,
        "the surviving watcher on the same path to observe writes",
    )
    .await;
}

#[tokio::test]
async fn dropping_the_outer_watcher_keeps_the_nested_one_watching() {
    // #7592 review: notify 6.1.1 keys one descriptor per path and a
    // recursive unwatch(outer) removes EVERY descriptor whose path starts
    // with the outer root — including the nested workspace's. Dropping the
    // outer handle used to leave the nested route published (status:
    // healthy) while its kernel watch was gone, so a held write under the
    // nested root was never observed and guarded checkpoint/rollback could
    // accept false quiescence. The shared watcher must re-arm overlapping
    // survivors after the unwatch.
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("nested-ws");
    std::fs::create_dir(&inner).unwrap();
    let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
    let inner_watcher = WorkspaceWatcher::start(&inner).unwrap();

    // While both handles live, a write inside the nested root flags both.
    {
        let held = write_held(inner.join("a.log"));
        let inner_flag = inner_watcher.is_writing_flag();
        let outer_flag = outer_watcher.is_writing_flag();
        eventually(&inner_flag, true, "the nested watcher to observe a write").await;
        eventually(
            &outer_flag,
            true,
            "the outer watcher to observe the same write",
        )
        .await;
        drop(held); // CLOSE_WRITE clears both latches.
    }
    let inner_flag = inner_watcher.is_writing_flag();
    let outer_flag = outer_watcher.is_writing_flag();
    eventually(&inner_flag, false, "the nested latch to clear on close").await;
    eventually(&outer_flag, false, "the outer latch to clear on close").await;

    drop(outer_watcher);

    // The nested watcher must observe NEW writes: its kernel descriptors
    // must have been re-armed when the overlapping outer root was released.
    let _held = write_held(inner.join("b.log"));
    eventually(
        &inner_flag,
        true,
        "the nested watcher to observe writes after the outer watcher dropped",
    )
    .await;
}

#[tokio::test]
async fn dropping_the_nested_watcher_keeps_the_outer_one_watching() {
    // Mirror of the case above: unwatch(nested) removes the descriptors
    // under the nested root that the outer recursive walk installed, so
    // writes inside the released subtree would stop reaching the outer
    // workspace unless the outer root is re-armed.
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("nested-ws");
    std::fs::create_dir(&inner).unwrap();
    let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
    let inner_watcher = WorkspaceWatcher::start(&inner).unwrap();

    {
        let held = write_held(inner.join("a.log"));
        let inner_flag = inner_watcher.is_writing_flag();
        let outer_flag = outer_watcher.is_writing_flag();
        eventually(&inner_flag, true, "the nested watcher to observe a write").await;
        eventually(
            &outer_flag,
            true,
            "the outer watcher to observe the same write",
        )
        .await;
        drop(held);
    }
    let outer_flag = outer_watcher.is_writing_flag();
    eventually(&outer_flag, false, "the outer latch to clear on close").await;

    drop(inner_watcher);

    // A write inside the (now unwatched-for-itself) nested subtree must
    // still reach the outer workspace through its re-armed recursive walk.
    let _held = write_held(inner.join("b.log"));
    eventually(
        &outer_flag,
        true,
        "the outer watcher to observe writes after the nested watcher dropped",
    )
    .await;
}
