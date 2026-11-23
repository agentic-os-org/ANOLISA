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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ws_ckpt_daemon::fs_watcher::{activity_now_ms, WorkspaceWatcher};

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

/// Current watermark of `activity`, after waiting until the activity clock
/// has moved past it: any event observed from now on stamps a strictly
/// newer value.
async fn baseline(activity: &AtomicU64) -> u64 {
    let baseline = activity.load(Ordering::Acquire);
    while activity_now_ms() <= baseline {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    baseline
}

/// Poll until `activity` moves past `baseline`, panicking after a deadline
/// that only reports an event that was never delivered.
async fn eventually_past(activity: &AtomicU64, baseline: u64, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while activity.load(Ordering::Acquire) <= baseline {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} (watermark={}, baseline={baseline})",
            activity.load(Ordering::Acquire)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Hold a file open and write to it, so no CLOSE_WRITE lands while the
/// watermark is being observed.
fn write_held(path: std::path::PathBuf) -> std::fs::File {
    let mut held = open_held(path);
    held.write_all(b"data").unwrap();
    held
}

/// Create and open a file for appending without writing. Opened before the
/// watch is armed, each later write through it produces exactly one
/// IN_MODIFY, so a test can tell which event moved the watermark.
fn open_held(path: std::path::PathBuf) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap()
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
    let a_activity = a.last_activity();
    let b_activity = b.last_activity();
    let a_before = baseline(&a_activity).await;
    let b_before = b_activity.load(Ordering::Acquire);

    let _held = write_held(dir_a.path().join("held.log"));

    eventually_past(
        &a_activity,
        a_before,
        "workspace A to observe its own writes",
    )
    .await;
    assert_eq!(
        b_activity.load(Ordering::Acquire),
        b_before,
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
    let inner_activity = inner_watcher.last_activity();
    let outer_activity = outer_watcher.last_activity();
    let inner_before = baseline(&inner_activity).await;
    let outer_before = baseline(&outer_activity).await;

    let _held = write_held(inner.join("held.log"));

    eventually_past(
        &inner_activity,
        inner_before,
        "the inner workspace to observe writes",
    )
    .await;
    eventually_past(
        &outer_activity,
        outer_before,
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
    let fresh_activity = fresh.last_activity();
    let fresh_before = baseline(&fresh_activity).await;

    let _held = write_held(dir.path().join("held.log"));

    eventually_past(
        &fresh_activity,
        fresh_before,
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
    // Opened before the watches are armed: each write below is exactly one
    // event, so an observed one cannot be a leftover from an earlier step.
    let mut held = open_held(inner.join("a.log"));
    let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
    let inner_watcher = WorkspaceWatcher::start(&inner).unwrap();
    let inner_activity = inner_watcher.last_activity();
    let outer_activity = outer_watcher.last_activity();

    // While both handles live, a write inside the nested root reaches both.
    let inner_before = baseline(&inner_activity).await;
    let outer_before = baseline(&outer_activity).await;
    held.write_all(b"data").unwrap();
    eventually_past(
        &inner_activity,
        inner_before,
        "the nested watcher to observe a write",
    )
    .await;
    eventually_past(
        &outer_activity,
        outer_before,
        "the outer watcher to observe the same write",
    )
    .await;

    drop(outer_watcher);

    // The nested watcher must observe NEW writes: its kernel descriptors
    // must have been re-armed when the overlapping outer root was released.
    let inner_before = baseline(&inner_activity).await;
    held.write_all(b"data").unwrap();
    eventually_past(
        &inner_activity,
        inner_before,
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
    let mut held = open_held(inner.join("a.log"));
    let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
    let inner_watcher = WorkspaceWatcher::start(&inner).unwrap();
    let inner_activity = inner_watcher.last_activity();
    let outer_activity = outer_watcher.last_activity();

    let inner_before = baseline(&inner_activity).await;
    let outer_before = baseline(&outer_activity).await;
    held.write_all(b"data").unwrap();
    eventually_past(
        &inner_activity,
        inner_before,
        "the nested watcher to observe a write",
    )
    .await;
    eventually_past(
        &outer_activity,
        outer_before,
        "the outer watcher to observe the same write",
    )
    .await;

    drop(inner_watcher);

    // A write inside the (now unwatched-for-itself) nested subtree must
    // still reach the outer workspace through its re-armed recursive walk.
    let outer_before = baseline(&outer_activity).await;
    held.write_all(b"data").unwrap();
    eventually_past(
        &outer_activity,
        outer_before,
        "the outer watcher to observe writes after the nested watcher dropped",
    )
    .await;
}
