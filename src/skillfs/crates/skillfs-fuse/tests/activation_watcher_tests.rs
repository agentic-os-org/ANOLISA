//! A5: Activation State Watcher integration tests.
//!
//! These tests verify the A5 acceptance criteria: SkillFS continuously
//! converges its in-memory `ActiveSkillResolver` to the daemon-owned
//! activation state, even when the update was not produced by the
//! current mount's notify/poll cycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use skillfs_fuse::security::{
    ActivationFreshness, ActivationReloadController, ActivationWatcher, ActiveSkillResolver,
    ActiveTarget, FailingNotifyClient, InMemoryNotifyClient, InMemoryProtocolEventWriter,
    MutationKind, NoopNotifyClient, NotifyController, ReloadOutcome,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup_skill_dir(dir: &Path, skill: &str) {
    std::fs::create_dir_all(dir.join(skill)).unwrap();
}

fn setup_skill_with_snapshot(dir: &Path, skill: &str, version: &str) {
    let skill_dir = dir.join(skill);
    let snap = skill_dir.join(format!(".skill-meta/versions/{version}.snapshot"));
    std::fs::create_dir_all(&snap).unwrap();
}

fn setup_skill_with_activation(dir: &Path, skill: &str, activation_json: &str) {
    let skill_dir = dir.join(skill);
    let meta = skill_dir.join(".skill-meta");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("activation.json"), activation_json).unwrap();
}

fn update_activation_fresh(dir: &Path, skill: &str, activation_json: &str) {
    let skill_dir = dir.join(skill);
    let before = ActivationFreshness::snapshot(&skill_dir);
    loop {
        std::thread::sleep(Duration::from_millis(15));
        setup_skill_with_activation(dir, skill, activation_json);
        let after = ActivationFreshness::snapshot(&skill_dir);
        if before.has_advanced(&after) {
            break;
        }
    }
}

fn make_reload_controller(
    dir: &Path,
    resolver: Arc<ActiveSkillResolver>,
) -> Arc<ActivationReloadController> {
    Arc::new(ActivationReloadController::new(
        dir,
        resolver,
        Duration::from_millis(50),
        Duration::from_millis(500),
    ))
}

// ---------------------------------------------------------------------------
// A5 acceptance criteria tests
// ---------------------------------------------------------------------------

/// If SkillFS starts hidden because activation is missing, and activation
/// is written later, the mounted view converges without remount.
#[test]
fn activation_missing_then_written_visible() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_dir(dir.path(), "alpha");

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = make_reload_controller(dir.path(), resolver.clone());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());
    let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), Duration::from_millis(100));
    watcher.register_skill("alpha");
    watcher.start();

    // Wait for the watcher to observe the missing activation.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        matches!(resolver.get("alpha"), Some(ActiveTarget::Hidden { .. })),
        "skill must be hidden when activation is missing"
    );

    // Daemon writes activation.
    setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
    update_activation_fresh(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    // Wait for the watcher to converge.
    std::thread::sleep(Duration::from_millis(400));
    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!("expected Snapshot after watcher convergence, got {other:?}"),
    }

    // Verify protocol events were emitted.
    assert!(
        writer.len() >= 2,
        "expected at least 2 protocol events (hidden + updated), got {}",
        writer.len()
    );

    watcher.shutdown();
}

/// If notify-triggered poll times out and activation is written after
/// the timeout, the watcher or periodic repair loop still refreshes.
#[test]
fn notify_poll_timeout_then_late_activation() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
    setup_skill_with_activation(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = Arc::new(ActivationReloadController::new(
        dir.path(),
        resolver.clone(),
        Duration::from_millis(30),
        Duration::from_millis(100), // very short timeout
    ));
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    // Bootstrap the resolver with the initial activation.
    reload_ctrl.reload_skill_once("alpha");
    assert!(matches!(
        resolver.get("alpha"),
        Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
    ));

    // Simulate notify poll: capture freshness, then poll will timeout
    // because we don't write activation yet.
    let baseline = reload_ctrl.snapshot_freshness("alpha");
    let outcome = reload_ctrl.poll_reload_skill("alpha", baseline);
    assert_eq!(outcome, ReloadOutcome::Timeout);

    // Now daemon writes v2 (after notify poll timed out).
    setup_skill_with_snapshot(dir.path(), "alpha", "v000002");
    update_activation_fresh(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
    );

    // Start watcher — it should detect the freshness advance.
    let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), Duration::from_millis(100));
    watcher.register_skill("alpha");
    watcher.start();

    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000002.snapshot");
        }
        other => panic!("expected v2 after watcher convergence, got {other:?}"),
    }

    watcher.shutdown();
}

/// If daemon reconcile or operator action updates activation without a
/// FUSE mutation, SkillFS eventually observes the update.
#[test]
fn daemon_reconcile_no_fuse_mutation_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
    setup_skill_with_activation(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = make_reload_controller(dir.path(), resolver.clone());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), Duration::from_millis(100));
    watcher.register_skill("alpha");
    watcher.start();

    // Wait for initial convergence.
    std::thread::sleep(Duration::from_millis(300));
    assert!(matches!(
        resolver.get("alpha"),
        Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
    ));

    // Daemon reconcile writes a new activation (no FUSE mutation involved).
    setup_skill_with_snapshot(dir.path(), "alpha", "v000002");
    update_activation_fresh(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
    );

    // Wait for watcher to pick it up.
    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000002.snapshot");
        }
        other => panic!("expected v2 after daemon reconcile (no FUSE mutation), got {other:?}"),
    }

    watcher.shutdown();
}

/// If the notify socket is unavailable and daemon repair happens later,
/// SkillFS eventually observes the repaired activation.
#[test]
fn notify_socket_fails_daemon_repairs() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_with_snapshot(dir.path(), "alpha", "v000001");

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = make_reload_controller(dir.path(), resolver.clone());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    // No activation yet — skill is hidden.
    let watcher = ActivationWatcher::new(
        reload_ctrl.clone(),
        writer.clone(),
        Duration::from_millis(100),
    );
    watcher.register_skill("alpha");

    // Simulate failed notify by using FailingNotifyClient (just for
    // context — the watcher is independent of notify).
    let _failing_client = Arc::new(FailingNotifyClient);

    watcher.start();
    std::thread::sleep(Duration::from_millis(300));

    // Skill should be hidden (no activation).
    assert!(
        matches!(resolver.get("alpha"), Some(ActiveTarget::Hidden { .. })),
        "skill must be hidden when no activation exists"
    );

    // Daemon repairs and writes activation.
    update_activation_fresh(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    // Watcher converges.
    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => {
            panic!("expected Snapshot after daemon repair (notify was unavailable), got {other:?}")
        }
    }

    watcher.shutdown();
}

/// Startup reconcile can lead to a current-mount resolver refresh once
/// daemon activation is written.
#[test]
fn startup_reconcile_then_daemon_writes_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_with_snapshot(dir.path(), "alpha", "v000001");

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = make_reload_controller(dir.path(), resolver.clone());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    // Simulate startup reconcile: notify sent, but no activation yet.
    let notify_client = Arc::new(InMemoryNotifyClient::new());
    let notify_ctrl = NotifyController::new_with_reload(
        notify_client.clone(),
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
        writer.clone(),
        reload_ctrl.clone(),
    );
    notify_ctrl.emit_startup_reconcile(&["alpha".to_string()]);

    // At this point, resolver has no entry for alpha (no activation).
    // The notify was sent but daemon hasn't written activation yet.

    // Start watcher with immediate check.
    let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), Duration::from_millis(100));
    watcher.register_skill("alpha");
    watcher.start();

    // Skill should be hidden initially.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        matches!(resolver.get("alpha"), Some(ActiveTarget::Hidden { .. })),
        "skill must be hidden before daemon writes activation"
    );

    // Daemon writes activation (simulating response to reconcile notify).
    update_activation_fresh(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    // Schedule immediate check (as main.rs would after reconcile).
    watcher.schedule_immediate_check(vec!["alpha".to_string()]);

    // Wait for convergence.
    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!("expected Snapshot after startup reconcile + daemon write, got {other:?}"),
    }

    notify_ctrl.shutdown();
    watcher.shutdown();
}

/// xattr-only update detected via dir ctime.
#[test]
fn xattr_only_update_observed() {
    fn user_xattr_supported(dir: &Path) -> bool {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_path = match CString::new(dir.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let c_name = match CString::new("user.skillfs.probe") {
            Ok(c) => c,
            Err(_) => return false,
        };
        let rc = unsafe {
            libc::lsetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                b"1".as_ptr() as *const libc::c_void,
                1,
                0,
            )
        };
        if rc != 0 {
            return false;
        }
        unsafe {
            libc::lremovexattr(c_path.as_ptr(), c_name.as_ptr());
        }
        true
    }

    fn set_activation_xattr(dir: &Path, value: &str) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(dir.as_os_str().as_bytes()).unwrap();
        let c_name = CString::new("user.agent_sec.skill_ledger.activation").unwrap();
        let rc = unsafe {
            libc::lsetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            )
        };
        assert_eq!(
            rc,
            0,
            "lsetxattr failed: {}",
            std::io::Error::last_os_error()
        );
    }

    fn xattr_capable_tempdir() -> Option<tempfile::TempDir> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(env_path) = std::env::var("SKILLFS_XATTR_TEST_ROOT") {
            if !env_path.is_empty() {
                candidates.push(PathBuf::from(env_path));
            }
        }
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_dir.ancestors() {
            if ancestor.join("Cargo.lock").exists() {
                candidates.push(ancestor.join("target").join("xattr-tests"));
                break;
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let mut path = PathBuf::from(home);
            path.push(".cache");
            path.push("skillfs-xattr-tests");
            candidates.push(path);
        }
        for cand in candidates {
            if std::fs::create_dir_all(&cand).is_err() {
                continue;
            }
            let td = match tempfile::Builder::new()
                .prefix("a5-integ-xattr-")
                .tempdir_in(&cand)
            {
                Ok(d) => d,
                Err(_) => continue,
            };
            if user_xattr_supported(td.path()) {
                return Some(td);
            }
        }
        None
    }

    let td = match xattr_capable_tempdir() {
        Some(d) => d,
        None => {
            eprintln!("SKIP: no xattr-capable filesystem for A5 xattr integration test");
            return;
        }
    };
    let dir = td.path();

    setup_skill_with_snapshot(dir, "alpha", "v000001");
    setup_skill_with_snapshot(dir, "alpha", "v000002");

    let skill_dir = dir.join("alpha");
    std::fs::create_dir_all(skill_dir.join(".skill-meta")).unwrap();
    set_activation_xattr(
        &skill_dir,
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    let resolver = Arc::new(ActiveSkillResolver::new(dir));
    let reload_ctrl = make_reload_controller(dir, resolver.clone());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), Duration::from_millis(100));
    watcher.register_skill("alpha");
    watcher.start();

    // Wait for initial convergence to v1.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
        ),
        "initial convergence should resolve v1, got {:?}",
        resolver.get("alpha")
    );

    // Update xattr only (no activation.json), wait for ctime advance.
    let baseline = ActivationFreshness::snapshot(&skill_dir);
    loop {
        std::thread::sleep(Duration::from_millis(15));
        set_activation_xattr(
            &skill_dir,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
        );
        let current = ActivationFreshness::snapshot(&skill_dir);
        if baseline.has_advanced(&current) {
            break;
        }
    }

    // Wait for watcher to pick up the xattr-only change.
    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000002.snapshot");
        }
        other => panic!("expected v2 after xattr-only update, got {other:?}"),
    }

    watcher.shutdown();
}

/// New skill triggers notify, poll times out, daemon writes activation
/// later, watcher converges — WITHOUT manual watcher registration.
/// This is the P1 gap fix: the skill is auto-registered via
/// NotifyController -> WatcherRegistrar on poll timeout.
#[test]
fn new_skill_notify_poll_timeout_then_late_activation_converges() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_with_snapshot(dir.path(), "new-skill", "v000001");
    // No activation.json yet — daemon hasn't processed the notify.

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = Arc::new(ActivationReloadController::new(
        dir.path(),
        resolver.clone(),
        Duration::from_millis(30),
        Duration::from_millis(100), // very short timeout to force timeout
    ));
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    // Build the watcher BEFORE the notify controller (matching main.rs order).
    let watcher = Arc::new(ActivationWatcher::new(
        reload_ctrl.clone(),
        writer.clone(),
        Duration::from_millis(100),
    ));
    // NOTE: we do NOT call watcher.register_skill("new-skill") here.
    // The skill must be auto-registered via the WatcherRegistrar trait.

    // Build notify controller with reload (poll-after-notify enabled).
    let notify_client = Arc::new(NoopNotifyClient);
    let notify_ctrl = NotifyController::new_with_reload(
        notify_client,
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
        writer.clone(),
        reload_ctrl.clone(),
    );

    // Inject the watcher registrar into the notify controller.
    notify_ctrl.set_watcher_registrar(watcher.clone());

    // Simulate a FUSE mutation on the new skill — this triggers
    // notify -> poll_reload_skill -> timeout (no activation.json).
    // On timeout, the notify controller auto-registers "new-skill"
    // with the watcher via WatcherRegistrar.
    notify_ctrl.observe(
        "new-skill",
        Some(Path::new("SKILL.md")),
        MutationKind::Write,
    );
    notify_ctrl.flush_for_testing();

    // At this point, poll_reload_skill timed out. The watcher should
    // now be tracking "new-skill" via auto-registration.
    // Start the watcher.
    watcher.start();

    // Wait briefly to let the watcher do its first tick — it should
    // observe "new-skill" as fail-safe hidden (no activation).
    std::thread::sleep(Duration::from_millis(300));

    // Now daemon writes activation (late, after poll timeout).
    update_activation_fresh(
        dir.path(),
        "new-skill",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    // Wait for the watcher to converge.
    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("new-skill") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!(
            "expected Snapshot after watcher convergence (auto-registered via notify timeout), \
             got {other:?}"
        ),
    }

    notify_ctrl.shutdown();
    watcher.shutdown();
}

/// Notify socket failure also auto-registers skill with the watcher,
/// so daemon repair can be observed later.
#[test]
fn notify_send_failure_auto_registers_for_convergence() {
    let dir = tempfile::tempdir().unwrap();
    setup_skill_with_snapshot(dir.path(), "alpha", "v000001");

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload_ctrl = make_reload_controller(dir.path(), resolver.clone());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    let watcher = Arc::new(ActivationWatcher::new(
        reload_ctrl.clone(),
        writer.clone(),
        Duration::from_millis(100),
    ));
    // NOT registered manually.

    // Failing client — daemon unreachable.
    let failing_client = Arc::new(FailingNotifyClient);
    let notify_ctrl = NotifyController::new_with_protocol_writer(
        failing_client,
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
        writer.clone(),
    );
    notify_ctrl.set_watcher_registrar(watcher.clone());

    // FUSE mutation on "alpha" — notify will fail, triggering
    // auto-registration with the watcher.
    notify_ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
    notify_ctrl.flush_for_testing();

    // Start watcher — it should now track "alpha".
    watcher.start();

    // Daemon repairs and writes activation.
    update_activation_fresh(
        dir.path(),
        "alpha",
        r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
    );

    // Wait for convergence.
    std::thread::sleep(Duration::from_millis(400));

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!(
            "expected Snapshot after watcher convergence (auto-registered via notify failure), \
             got {other:?}"
        ),
    }

    notify_ctrl.shutdown();
    watcher.shutdown();
}
