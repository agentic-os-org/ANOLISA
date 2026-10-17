//! A failed recursive registration must not leak the watch descriptors
//! notify installed before the failure (#7592 review, P1).
//!
//! notify 6.1.1's inotify backend walks the tree and installs one watch
//! descriptor per directory; when a later directory fails (ENOSPC /
//! `MaxFilesWatch` under `fs.inotify.max_user_watches` pressure) it returns
//! without rolling back the earlier additions. This file pins the daemon's
//! own rollback: after a failed start, a fresh registration must still
//! succeed — the partial descriptors were released, not left pinning the
//! per-user budget until daemon restart.
//!
//! The scenario needs a watch-budget exhaustion, so the test temporarily
//! lowers `fs.inotify.max_user_watches` (system-wide, restored on exit via
//! a guard). It runs as its own cargo target on purpose: everything else
//! that arms inotify watches in the test suite runs in other processes,
//! one target at a time, so nothing else registers watches while the limit
//! is low. Skips itself when it cannot control the sysctl (non-root, or a
//! limit already too low to make the scenario fail deterministically).

use std::fs;
use std::path::PathBuf;

use ws_ckpt_daemon::fs_watcher::WorkspaceWatcher;

const WATCH_LIMIT_PATH: &str = "/proc/sys/fs/inotify/max_user_watches";

/// Watch descriptors currently armed by every process on the system.
/// The per-user budget this test shapes counts exactly these, so the
/// temporary limit must leave room for all of them plus the test's
/// headroom. Best effort: unreadable entries are skipped.
fn system_inotify_watch_count() -> u64 {
    let mut total = 0u64;
    let Ok(procs) = fs::read_dir("/proc") else {
        return 0;
    };
    for proc in procs.flatten() {
        let pid = proc.file_name();
        let Some(pid) = pid.to_str() else { continue };
        if !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = fs::read_dir(PathBuf::from("/proc").join(pid).join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            if target.to_string_lossy() != "anon_inode:inotify" {
                continue;
            }
            let fdinfo = PathBuf::from("/proc")
                .join(pid)
                .join("fdinfo")
                .join(fd.file_name());
            let Ok(text) = fs::read_to_string(&fdinfo) else {
                continue;
            };
            total += text
                .lines()
                .filter(|line| line.starts_with("inotify wd:"))
                .count() as u64;
        }
    }
    total
}

/// Restore `fs.inotify.max_user_watches` however the test exits.
struct WatchLimitGuard {
    original: String,
}

impl Drop for WatchLimitGuard {
    fn drop(&mut self) {
        let _ = fs::write(WATCH_LIMIT_PATH, &self.original);
    }
}

/// Serializes the sysctl-shaping tests in this target: cargo runs test
/// functions in parallel threads, and two concurrent writers of
/// `max_user_watches` (each measuring a different ambient count) would
/// corrupt each other's budget. Async-aware like the diff-tmp shim lock:
/// the nested-survivor scenario below awaits its quiescence probe while
/// holding it.
static SHAPING_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Lower `max_user_watches` to `ambient + HEADROOM` under the shaping lock.
/// Returns `None` (after restoring nothing) when the scenario cannot be
/// shaped on this host.
fn shape_watch_limit(headroom: u64) -> Option<WatchLimitGuard> {
    let Ok(original) = fs::read_to_string(WATCH_LIMIT_PATH) else {
        eprintln!("skipping: cannot read {WATCH_LIMIT_PATH}");
        return None;
    };
    let original_value: u64 = match original.trim().parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("skipping: cannot parse current watch limit {original:?}");
            return None;
        }
    };
    let ambient = system_inotify_watch_count();
    let limit = ambient + headroom;
    if limit >= original_value {
        eprintln!(
            "skipping: current limit {original_value} is already too low to shape \
             (ambient {ambient} + headroom {headroom})"
        );
        return None;
    }
    if fs::write(WATCH_LIMIT_PATH, limit.to_string()).is_err() {
        eprintln!("skipping: cannot write {WATCH_LIMIT_PATH} (not root?)");
        return None;
    }
    Some(WatchLimitGuard {
        original: original.clone(),
    })
}

/// Deadline-poll `flag` until it becomes `want`, panicking after 3s.
fn eventually_sync(flag: &std::sync::atomic::AtomicBool, want: bool, what: &str) {
    use std::sync::atomic::Ordering;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while flag.load(Ordering::Acquire) != want {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what} (flag={})",
            flag.load(Ordering::Acquire)
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[tokio::test]
async fn failed_registration_releases_its_partial_watch_descriptors() {
    let _shaping = SHAPING_LOCK.lock().await;
    let Some(_guard) = shape_watch_limit(24) else {
        return;
    };
    const HEADROOM: u64 = 24;

    // The big tree needs far more watch descriptors than the headroom
    // allows, so its recursive registration must fail partway through
    // notify's directory walk — after installing a partial set.
    let big = tempfile::tempdir().unwrap();
    for i in 0..(HEADROOM * 4) {
        fs::create_dir(big.path().join(format!("sub-{i:03}"))).unwrap();
    }
    let failure = match WorkspaceWatcher::start(big.path()) {
        Err(e) => e,
        Ok(_) => panic!("registration must fail with the watch budget exhausted"),
    };
    eprintln!("planned failure: {failure:#}");

    // The regression: with the failed registration's partial descriptors
    // leaked, the per-user budget stays exhausted and this fresh, tiny
    // registration fails too. After a correct rollback it must succeed.
    let small = tempfile::tempdir().unwrap();
    fs::create_dir(small.path().join("one")).unwrap();
    let watcher = WorkspaceWatcher::start(small.path()).expect(
        "a fresh registration must succeed after a failed one released its partial descriptors",
    );
    drop(watcher);
}

#[tokio::test]
async fn failed_outer_registration_keeps_the_nested_watcher_observing() {
    // #7592 second review: the rollback unwatch after a failed recursive
    // registration prefix-removes the descriptors of an ALREADY REGISTERED
    // nested workspace (notify 6.1.1 `remove_watch` drops every path
    // starting with the released root). The nested route and handle stay
    // behind, so it used to look healthy while its kernel coverage was
    // gone — writes under it invisible, quiescence falsely Safe. The
    // rollback must re-arm every overlapping survivor it disturbs, exactly
    // like the ordinary release path does.
    let _shaping = SHAPING_LOCK.lock().await;
    let Some(_guard) = shape_watch_limit(24) else {
        return;
    };
    const HEADROOM: u64 = 24;

    let big = tempfile::tempdir().unwrap();
    let nested = big.path().join("nested-ws");
    fs::create_dir(&nested).unwrap();
    // Register the nested workspace FIRST: it must survive the outer
    // registration's failure.
    let nested_watcher = WorkspaceWatcher::start(&nested)
        .expect("the nested registration fits inside the shaped headroom");

    // The big tree needs far more watch descriptors than the headroom
    // allows, so its recursive registration must fail partway through
    // notify's directory walk — after installing a partial set.
    for i in 0..(HEADROOM * 4) {
        fs::create_dir(big.path().join(format!("sub-{i:03}"))).unwrap();
    }
    let failure = match WorkspaceWatcher::start(big.path()) {
        Err(e) => e,
        Ok(_) => panic!("outer registration must fail with the watch budget exhausted"),
    };
    eprintln!("planned failure: {failure:#}");

    // The pre-unwatch survivor latch sets the nested flag `true`; only a
    // LIVE close observation can clear it again. A write that closes must
    // therefore end with the flag `false` — proving real event coverage,
    // not just a published route.
    {
        use std::io::Write as _;
        let mut held = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(nested.join("a.log"))
            .unwrap();
        held.write_all(b"data").unwrap();
        drop(held);
    }
    let nested_flag = nested_watcher.is_writing_flag();
    eventually_sync(
        &nested_flag,
        false,
        "the nested watcher to observe the write close after the failed outer registration",
    );

    // And an active writer must not look quiescent through whatever
    // coverage survived the rollback.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let stop = std::sync::Arc::clone(&stop);
        let path = nested.join("writer.log");
        std::thread::spawn(move || {
            use std::io::Write as _;
            let mut held = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let _ = held.write_all(b"data");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        })
    };
    // Wait for the writer's first write to be OBSERVED before probing
    // quiescence: `check_quiescent` takes a not-writing shortcut to Safe,
    // and losing the race against the first event delivery would turn the
    // probe into a flake.
    eventually_sync(
        &nested_flag,
        true,
        "the nested watcher to observe the active writer",
    );
    let quiescent = nested_watcher.check_quiescent().await;
    stop.store(true, std::sync::atomic::Ordering::Release);
    writer.join().unwrap();
    assert!(
        !quiescent,
        "a workspace whose re-arm survived a failed overlapping registration must still \
         observe its writer (false quiescence is the guarded-checkpoint hazard)"
    );
}

#[tokio::test]
async fn failed_survivor_rearm_releases_its_partial_watch_descriptors() {
    // #7592 fourth review (P2): a survivor's recursive re-arm that fails
    // mid-walk leaves notify's partially installed descriptors armed
    // unless the failure branch releases them. Under watch-budget
    // pressure the leaked set pins the per-user budget: later survivors
    // and heals register against a budget occupied by the incomplete
    // registration, prolonging the monitoring outage. The release must
    // also keep the descriptors other overlapping survivors still rely
    // on (an ancestor whose walk covers the failed root, a sibling
    // outside it) watching.
    //
    // Layout: parent ⊃ outer ⊃ { nested-a (the survivor whose re-arm
    // fails), nested-b (a sibling that must keep observing) }. Every
    // root is registered first, then the per-user budget is frozen at
    // exactly what is armed; nested-a's tree then grows beyond what the
    // frozen budget can re-arm, so its recursive re-arm after the
    // release of `outer` fails partway through notify's walk.
    let _shaping = SHAPING_LOCK.lock().await;
    let parent = tempfile::tempdir().unwrap();
    let outer = parent.path().join("outer");
    let nested_a = outer.join("nested-a");
    let nested_b = outer.join("nested-b");
    fs::create_dir_all(&nested_a).unwrap();
    fs::create_dir_all(&nested_b).unwrap();
    for i in 0..8 {
        fs::create_dir(nested_a.join(format!("old-{i:02}"))).unwrap();
    }
    fs::create_dir(nested_b.join("data")).unwrap();
    let _parent_watcher = WorkspaceWatcher::start(parent.path()).unwrap();
    let outer_watcher = WorkspaceWatcher::start(&outer).unwrap();
    let a_watcher = WorkspaceWatcher::start(&nested_a).unwrap();
    let b_watcher = WorkspaceWatcher::start(&nested_b).unwrap();

    // Freeze the budget at exactly what is armed now (headroom zero): no
    // new descriptor fits, so the directories created below stay
    // unwatched (notify's auto-add on Create fails silently against the
    // frozen limit).
    let Some(_guard) = shape_watch_limit(0) else {
        return;
    };

    // Grow nested-a's tree beyond what the frozen budget can re-arm:
    // after the release strips the subtree, the re-arm walk needs more
    // descriptors than the strip freed, and fails partway through.
    for i in 0..8 {
        fs::create_dir(nested_a.join(format!("new-{i:02}"))).unwrap();
    }

    drop(outer_watcher); // survivor re-arms; nested-a's must fail partway

    // The failed survivor is degraded (fail closed) ...
    assert!(
        a_watcher.is_degraded(),
        "the survivor whose re-arm failed must be reported degraded"
    );

    // ... and its partial descriptors were RELEASED, not left pinning
    // the per-user budget: a fresh registration must not have to
    // register against a budget occupied by the incomplete one.
    let small = tempfile::tempdir().unwrap();
    fs::create_dir(small.path().join("one")).unwrap();
    let fresh = WorkspaceWatcher::start(small.path()).expect(
        "the failed survivor re-arm must release its partial descriptors; a fresh \
         registration must not register against a budget they still occupy",
    );
    drop(fresh);

    // The sibling survivor keeps real coverage through the failed
    // re-arm's cleanup: a write that closes must be observed.
    {
        use std::io::Write as _;
        let mut held = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(nested_b.join("data").join("a.log"))
            .unwrap();
        held.write_all(b"data").unwrap();
        drop(held);
    }
    let b_flag = b_watcher.is_writing_flag();
    eventually_sync(
        &b_flag,
        false,
        "the sibling survivor to observe the write close after the failed re-arm cleanup",
    );
    assert!(
        !b_watcher.is_degraded(),
        "the sibling survivor's coverage must survive the failed re-arm's cleanup"
    );
}
