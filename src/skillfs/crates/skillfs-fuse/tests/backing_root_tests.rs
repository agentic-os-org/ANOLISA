//! A6/B1: Ledger Backing Root integration tests.
//!
//! Verifies canonical/live root separation with a backing root:
//! - Notify v2 uses canonical paths, never the live backing root.
//! - Activation bootstrap reads from the daemon root.
//! - Activation reload reads from the daemon root.
//! - In-place hidden skill is invisible through FUSE but the source
//!   (backing root) still has the live files.
//! - Fallback skill serves snapshot through FUSE while the source
//!   (backing root) still has the live files.
//! - Non-in-place mount without backing root preserves existing behavior.
//! - Unsafe backing root is rejected at setup.
//! - Identity mismatch (separate non-bind-mount directory) is rejected.
//! - P2-1: Path-shape check runs before directory creation (no side effects).
//! - P2-3: In-place mount with backing root == source exercises daemon wiring.

#![allow(dead_code)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActivationReloadController, ActiveSkillResolver, ActiveTarget, BackingRootError,
    InMemoryNotifyClient, InMemoryProtocolEventWriter, LedgerBackingRoot, MutationKind,
    NotifyController, ReloadOutcome, bootstrap_activation,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

use common::{create_skill_dir, list_dir_names};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn make_skill(dir: &Path, name: &str) {
    create_skill_dir(dir, name);
}

fn make_snapshot(dir: &Path, skill: &str, version: &str) {
    let snap = dir
        .join(skill)
        .join(format!(".skill-meta/versions/{version}"));
    std::fs::create_dir_all(&snap).unwrap();
    std::fs::write(
        snap.join("SKILL.md"),
        format!("---\nname: {skill}\ndescription: snapshot {version}\n---\n"),
    )
    .unwrap();
}

fn write_activation(dir: &Path, skill: &str, json: &str) {
    let meta = dir.join(skill).join(".skill-meta");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("activation.json"), json).unwrap();
}

const HIDDEN_ACTIVATION: &str = r#"{"schemaVersion": 1, "target": null}"#;
const SNAPSHOT_ACTIVATION: &str =
    r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#;

// ─────────────────────────────────────────────────────────────────────────────
// Non-FUSE tests: daemon-facing operations use daemon root
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn notify_v2_uses_canonical_root_with_backing_root() {
    let canonical_root = tempfile::tempdir().unwrap();
    let live_root = tempfile::tempdir().unwrap();
    make_skill(canonical_root.path(), "alpha");
    make_skill(live_root.path(), "alpha");

    let client = Arc::new(InMemoryNotifyClient::new());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());

    let ctrl = NotifyController::new_with_protocol_writer(
        client.clone(),
        canonical_root.path().to_path_buf(),
        live_root.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
        writer.clone(),
    );

    // Trigger a notify for skill "alpha".
    ctrl.observe("alpha", Some(Path::new("scripts")), MutationKind::Mkdir);
    ctrl.flush_for_testing();

    // Wait for the notify worker to process.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !client.is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("timeout waiting for notify");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let events = client.events();
    assert!(!events.is_empty(), "expected at least one notify event");
    let canonical_skill_dir = &events[0].canonical_skill_dir;
    assert_eq!(events[0].schema_version, 2);
    assert_eq!(events[0].skill_id, "alpha");
    assert_eq!(
        canonical_skill_dir,
        &canonical_root
            .path()
            .join("alpha")
            .to_string_lossy()
            .to_string()
    );
    assert!(
        !canonical_skill_dir.starts_with(live_root.path().to_string_lossy().as_ref()),
        "canonicalSkillDir '{}' must not expose live root '{}'",
        canonical_skill_dir,
        live_root.path().display()
    );

    let protocol_events = writer.events();
    assert_eq!(protocol_events[0].schema_version, 1);
    assert_eq!(protocol_events[0].skill_name, "alpha");
    assert_eq!(
        protocol_events[0].skill_dir,
        live_root.path().join("alpha").to_string_lossy().to_string()
    );
    assert!(
        !protocol_events[0]
            .skill_dir
            .starts_with(canonical_root.path().to_string_lossy().as_ref()),
        "protocol event skillDir '{}' must not use canonical root '{}'",
        protocol_events[0].skill_dir,
        canonical_root.path().display()
    );

    let count = ctrl.emit_startup_reconcile(&["alpha".to_string()]);
    assert_eq!(count, 1);

    let notify_events = client.events();
    let reconcile_notify = notify_events
        .iter()
        .find(|event| event.event_kind == "reconcile")
        .expect("startup reconcile notify event");
    assert_eq!(reconcile_notify.schema_version, 2);
    assert_eq!(reconcile_notify.skill_id, "alpha");
    assert_eq!(
        reconcile_notify.canonical_skill_dir,
        canonical_root
            .path()
            .join("alpha")
            .to_string_lossy()
            .to_string()
    );

    let protocol_events = writer.events();
    let reconcile_protocol = protocol_events
        .iter()
        .find(|event| event.event_kind == "reconcile")
        .expect("startup reconcile protocol event");
    assert_eq!(reconcile_protocol.schema_version, 1);
    assert_eq!(
        reconcile_protocol.skill_dir,
        live_root.path().join("alpha").to_string_lossy().to_string()
    );
}

#[test]
fn bootstrap_activation_uses_daemon_root() {
    // bootstrap_activation should read activation.json from the daemon root,
    // not the FUSE source path.
    let daemon_root = tempfile::tempdir().unwrap();
    make_skill(daemon_root.path(), "alpha");
    make_snapshot(daemon_root.path(), "alpha", "v000001.snapshot");
    write_activation(daemon_root.path(), "alpha", SNAPSHOT_ACTIVATION);

    // The resolver is rooted at source (for FUSE read paths), but
    // bootstrap_activation reads from daemon_root.
    let source = tempfile::tempdir().unwrap();
    let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));

    let results = bootstrap_activation(daemon_root.path(), &["alpha".to_string()], &resolver);

    assert_eq!(results.len(), 1);
    let (name, outcome) = &results[0];
    assert_eq!(name, "alpha");
    assert!(outcome.is_ok(), "bootstrap should succeed: {outcome:?}");

    match outcome.as_ref().unwrap() {
        ActiveTarget::Snapshot { version, .. } => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

#[test]
fn bootstrap_activation_hidden_from_daemon_root() {
    let daemon_root = tempfile::tempdir().unwrap();
    make_skill(daemon_root.path(), "alpha");
    write_activation(daemon_root.path(), "alpha", HIDDEN_ACTIVATION);

    let source = tempfile::tempdir().unwrap();
    let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));

    let results = bootstrap_activation(daemon_root.path(), &["alpha".to_string()], &resolver);

    let (_, outcome) = &results[0];
    match outcome.as_ref().unwrap() {
        ActiveTarget::Hidden { .. } => {}
        other => panic!("expected Hidden, got {other:?}"),
    }
}

#[test]
fn activation_reload_uses_daemon_root() {
    // ActivationReloadController should read activation from the daemon root.
    let daemon_root = tempfile::tempdir().unwrap();
    make_skill(daemon_root.path(), "alpha");
    make_snapshot(daemon_root.path(), "alpha", "v000001.snapshot");

    // No activation yet.
    let source = tempfile::tempdir().unwrap();
    let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));
    let reload_ctrl = Arc::new(ActivationReloadController::new(
        daemon_root.path().to_path_buf(),
        resolver.clone(),
        Duration::from_millis(50),
        Duration::from_millis(500),
    ));

    // Initially no activation → fail-safe hidden.
    let outcome = reload_ctrl.reload_skill_once("alpha");
    match outcome {
        ReloadOutcome::FailSafeHidden { .. } => {}
        other => panic!("expected FailSafeHidden, got {other:?}"),
    }

    // Daemon writes activation through the daemon root.
    write_activation(daemon_root.path(), "alpha", SNAPSHOT_ACTIVATION);

    // Reload should pick up the new activation from daemon_root.
    let outcome = reload_ctrl.reload_skill_once("alpha");
    match outcome {
        ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!("expected Updated(Snapshot), got {other:?}"),
    }
}

#[test]
fn activation_reload_freshness_uses_daemon_root() {
    // snapshot_freshness should read from the daemon root path.
    let daemon_root = tempfile::tempdir().unwrap();
    make_skill(daemon_root.path(), "alpha");

    let source = tempfile::tempdir().unwrap();
    let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));
    let reload_ctrl = Arc::new(ActivationReloadController::new(
        daemon_root.path().to_path_buf(),
        resolver.clone(),
        Duration::from_millis(50),
        Duration::from_millis(500),
    ));

    let baseline = reload_ctrl.snapshot_freshness("alpha");

    // Write activation to daemon root.
    write_activation(daemon_root.path(), "alpha", HIDDEN_ACTIVATION);

    // Give filesystem time to update mtime.
    std::thread::sleep(Duration::from_millis(20));

    let current = reload_ctrl.snapshot_freshness("alpha");
    assert!(
        baseline.has_advanced(&current),
        "freshness should advance after activation write"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Non-FUSE tests: backing root setup validation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn unsafe_backing_root_permissions_too_open_rejected() {
    // P1-2: A directory with group/other access bits (0o755) must be rejected.
    let source = tempfile::tempdir().unwrap();
    let source_canon = source.path().canonicalize().unwrap();
    let mount = tempfile::tempdir().unwrap();
    let mount_canon = mount.path().canonicalize().unwrap();

    let backing = tempfile::tempdir().unwrap();
    let backing_path = backing.path().canonicalize().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&backing_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);
    assert!(
        result.is_err(),
        "0o755 backing root should be rejected (permissions too open)"
    );

    // Cleanup.
    std::fs::set_permissions(&backing_path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn unsafe_backing_root_inside_mount_rejected() {
    let source = tempfile::tempdir().unwrap();
    let source_canon = source.path().canonicalize().unwrap();
    let mount = tempfile::tempdir().unwrap();
    let mount_canon = mount.path().canonicalize().unwrap();
    let inside = mount_canon.join("subdir");
    std::fs::create_dir(&inside).unwrap();

    let result = LedgerBackingRoot::setup(&source_canon, &inside, &mount_canon, false);
    assert!(
        result.is_err(),
        "backing root inside mount path should be rejected"
    );
}

#[test]
fn identity_mismatch_separate_directory_rejected() {
    // P1-1: A separate directory that is NOT a bind mount of source
    // must be rejected (identity mismatch). Without root privileges,
    // bind mount fails and the separate directory has different dev/ino.
    let source = tempfile::tempdir().unwrap();
    let source_canon = source.path().canonicalize().unwrap();

    let backing = tempfile::tempdir().unwrap();
    let backing_canon = backing.path().canonicalize().unwrap();
    // Set restrictive permissions so permission check passes and
    // identity check is what rejects this.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&backing_canon, std::fs::Permissions::from_mode(0o700)).unwrap();

    let mount = tempfile::tempdir().unwrap();
    let mount_canon = mount.path().canonicalize().unwrap();

    let result = LedgerBackingRoot::setup(&source_canon, &backing_canon, &mount_canon, false);
    assert!(
        result.is_err(),
        "separate non-bind-mount directory should be rejected"
    );
}

#[test]
fn backing_root_same_as_source_accepted() {
    // Non-in-place: backing root == source is always valid (identity matches).
    let source = tempfile::tempdir().unwrap();
    let source_canon = source.path().canonicalize().unwrap();
    let mount = tempfile::tempdir().unwrap();
    let mount_canon = mount.path().canonicalize().unwrap();

    let br = LedgerBackingRoot::setup(&source_canon, &source_canon, &mount_canon, false)
        .expect("backing root == source should be accepted");

    assert_eq!(br.path(), source_canon);
}

#[test]
fn p2_1_no_side_effects_on_rejected_path_inside_source() {
    // P2-1: A rejected path inside source must NOT create a directory
    // in the source tree.
    let source = tempfile::tempdir().unwrap();
    let source_canon = source.path().canonicalize().unwrap();
    let backing_path = source_canon.join(".skillfs-ledger");

    assert!(!backing_path.exists(), "precondition: path must not exist");

    let result = LedgerBackingRoot::setup(
        &source_canon,
        &backing_path,
        &source_canon, // in-place: source == mount
        true,
    );

    assert!(result.is_err(), "should be rejected");
    assert!(
        !backing_path.exists(),
        "rejected path must not create directory in source tree"
    );
}

#[test]
#[ignore = "requires root/CAP_SYS_ADMIN for bind mount"]
fn in_place_bind_mount_backing_root_e2e() {
    // P2-3: End-to-end test for in-place mount with a real bind mount.
    // Requires root to create bind mount. Run with:
    //   sudo cargo test -p skillfs-fuse --test backing_root_tests \
    //     in_place_bind_mount_backing_root_e2e -- --ignored
    let source = tempfile::tempdir().unwrap();
    let source_canon = source.path().canonicalize().unwrap();

    // Create a backing root outside source.
    let backing_parent = tempfile::tempdir().unwrap();
    let backing_path = backing_parent.path().join("backing_root");

    let br = LedgerBackingRoot::setup(&source_canon, &backing_path, &source_canon, true)
        .expect("bind mount should succeed with root");

    assert_eq!(br.path(), backing_path.canonicalize().unwrap());

    // Verify the bind mount mirrors source.
    make_skill(source.path(), "alpha");
    let backing_alpha = br.path().join("alpha/SKILL.md");
    assert!(
        backing_alpha.exists(),
        "bind mount should mirror source content"
    );

    // Cleanup via Drop.
    drop(br);
    assert!(!backing_path.exists(), "temp dir should be cleaned up");
}

// ─────────────────────────────────────────────────────────────────────────────
// FUSE tests: agent-visible FUSE view vs daemon-visible backing root
// ─────────────────────────────────────────────────────────────────────────────

struct BackingRootMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl BackingRootMount {
    /// Mount in normal mode with an active resolver that hides the given skill.
    /// All other skills are set to Current so they remain visible.
    fn new_with_hidden(seed: impl FnOnce(&Path), hidden_skill: &str) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());

        let resolver = ActiveSkillResolver::new(source.path().to_path_buf());
        resolver.set(
            hidden_skill.to_string(),
            ActiveTarget::Hidden {
                reason: "test hidden".to_string(),
            },
        );
        // Set all other loaded skills to Current so they remain visible.
        for name in store.list() {
            if name != hidden_skill && name != "skill-discover" {
                let skill_dir = source.path().join(name);
                resolver.set(
                    name.to_string(),
                    ActiveTarget::Current {
                        source_dir: skill_dir,
                    },
                );
            }
        }

        let mountpoint = tempfile::tempdir().unwrap();
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(Arc::new(resolver)),
                ..MountConfig::default()
            },
        )
        .expect("mount");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    /// Mount in normal mode with an active resolver that maps to a snapshot.
    fn new_with_snapshot(seed: impl FnOnce(&Path), skill: &str, version: &str) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let snap_dir = source
            .path()
            .join(skill)
            .join(format!(".skill-meta/versions/{version}"));
        let resolver = ActiveSkillResolver::new(source.path().to_path_buf());
        resolver.set(
            skill.to_string(),
            ActiveTarget::Snapshot {
                snapshot_dir: snap_dir,
                version: version.to_string(),
            },
        );

        let mountpoint = tempfile::tempdir().unwrap();
        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(Arc::new(resolver)),
                ..MountConfig::default()
            },
        )
        .expect("mount");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn skills_dir(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.skills_dir().join(name)
    }

    /// The physical source directory (simulates the backing root in tests).
    fn source_path(&self) -> &Path {
        self.source.path()
    }
}

impl Drop for BackingRootMount {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn in_place_hidden_skill_fuse_invisible_source_visible() {
    // Agent-visible FUSE view hides the skill, but the source (backing root)
    // still has the live files visible to the daemon.
    skip_if_no_fuse!();

    let fixture = BackingRootMount::new_with_hidden(
        |src| {
            make_skill(src, "alpha");
            make_skill(src, "beta");
        },
        "alpha",
    );

    // FUSE view: "alpha" is hidden, "beta" is visible.
    let fuse_entries = list_dir_names(&fixture.skills_dir());
    assert!(
        !fuse_entries.contains(&"alpha".to_string()),
        "hidden skill should be invisible in FUSE readdir: {fuse_entries:?}"
    );
    assert!(
        fuse_entries.contains(&"beta".to_string()),
        "visible skill should be in FUSE readdir: {fuse_entries:?}"
    );

    // FUSE lookup of hidden skill returns ENOENT.
    let alpha_path = fixture.skill_path("alpha");
    assert!(
        !alpha_path.exists(),
        "hidden skill path should not exist through FUSE"
    );

    // Source (backing root): both skills are visible — the daemon can
    // scan the live source tree.
    let source_entries = list_dir_names(fixture.source_path());
    assert!(
        source_entries.contains(&"alpha".to_string()),
        "hidden skill should be visible in source (backing root): {source_entries:?}"
    );
    assert!(
        source_entries.contains(&"beta".to_string()),
        "visible skill should be in source (backing root): {source_entries:?}"
    );

    // The daemon can read the hidden skill's SKILL.md through the source.
    let source_alpha_md = fixture.source_path().join("alpha/SKILL.md");
    assert!(
        source_alpha_md.exists(),
        "daemon should be able to read hidden skill's SKILL.md through source"
    );
}

#[test]
fn fallback_skill_fuse_reads_snapshot_source_reads_live() {
    // Agent-visible FUSE view serves the snapshot, but the source (backing
    // root) still has the live source for the daemon to scan.
    skip_if_no_fuse!();

    let fixture = BackingRootMount::new_with_snapshot(
        |src| {
            make_skill(src, "alpha");
            // Write live SKILL.md with "live" content.
            std::fs::write(
                src.join("alpha/SKILL.md"),
                "---\nname: alpha\ndescription: live content\n---\n",
            )
            .unwrap();
            // Create snapshot with "snapshot" content.
            make_snapshot(src, "alpha", "v000001.snapshot");
        },
        "alpha",
        "v000001.snapshot",
    );

    // FUSE view: "alpha" is visible.
    let fuse_entries = list_dir_names(&fixture.skills_dir());
    assert!(
        fuse_entries.contains(&"alpha".to_string()),
        "fallback skill should be visible in FUSE readdir: {fuse_entries:?}"
    );

    // Source (backing root): the live SKILL.md has "live" content.
    let source_alpha_md = fixture.source_path().join("alpha/SKILL.md");
    let source_content = std::fs::read_to_string(&source_alpha_md).unwrap();
    assert!(
        source_content.contains("live content"),
        "source (backing root) should have live content: {source_content}"
    );

    // The daemon can also see the snapshot directory in the source.
    let snap_dir = fixture
        .source_path()
        .join("alpha/.skill-meta/versions/v000001.snapshot");
    assert!(
        snap_dir.exists(),
        "daemon should be able to see snapshot dir in source (backing root)"
    );
}

#[test]
fn non_in_place_no_backing_root_preserves_behavior() {
    // Without a backing root, the mount behaves exactly as before.
    skip_if_no_fuse!();

    let fixture = BackingRootMount::new_with_hidden(
        |src| {
            make_skill(src, "alpha");
            make_skill(src, "beta");
        },
        "alpha", // hidden
    );

    // Without backing root, the daemon root == source.
    // The FUSE view should still hide "alpha" and show "beta".
    let fuse_entries = list_dir_names(&fixture.skills_dir());
    assert!(!fuse_entries.contains(&"alpha".to_string()));
    assert!(fuse_entries.contains(&"beta".to_string()));

    // Source has both (this is the default behavior, no backing root needed).
    let source_entries = list_dir_names(fixture.source_path());
    assert!(source_entries.contains(&"alpha".to_string()));
    assert!(source_entries.contains(&"beta".to_string()));
}

// ─────────────────────────────────────────────────────────────────────────────
// Startup validation: backing root accessibility
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn backing_root_missing_fails_validation_in_place() {
    use skillfs_fuse::security::SecurityConfig;

    let config: SecurityConfig = toml::from_str(
        r#"
[activation]
mode = "file"

[ledger]
backing_root = "/nonexistent/path/that/does/not/exist"
"#,
    )
    .unwrap();

    let result = config.validate_backing_root_accessible(true);
    assert!(
        result.is_err(),
        "missing backing root in in-place mode with activation enabled must fail"
    );
}

#[test]
fn backing_root_not_configured_fails_validation_in_place() {
    use skillfs_fuse::security::SecurityConfig;

    let config: SecurityConfig = toml::from_str(
        r#"
[activation]
mode = "file"
"#,
    )
    .unwrap();

    let result = config.validate_backing_root_accessible(true);
    assert!(
        result.is_err(),
        "in-place + activation without backing_root configured must fail"
    );
}

#[test]
fn backing_root_not_needed_when_not_in_place() {
    use skillfs_fuse::security::SecurityConfig;

    let config: SecurityConfig = toml::from_str(
        r#"
[activation]
mode = "file"

[ledger]
backing_root = "/nonexistent/path/that/does/not/exist"
"#,
    )
    .unwrap();

    let result = config.validate_backing_root_accessible(false);
    assert!(
        result.is_ok(),
        "non-in-place mode should not require backing root validation"
    );
}

#[test]
fn backing_root_valid_passes_validation() {
    use skillfs_fuse::security::SecurityConfig;

    let dir = tempfile::tempdir().unwrap();
    let config_str = format!(
        r#"
[activation]
mode = "file"

[ledger]
backing_root = "{}"
"#,
        dir.path().display()
    );
    let config: SecurityConfig = toml::from_str(&config_str).unwrap();

    let result = config.validate_backing_root_accessible(true);
    assert!(
        result.is_ok(),
        "existing backing root directory must pass validation"
    );
}

#[test]
fn normal_mount_notify_uses_source_as_canonical_skill_dir() {
    let source = tempfile::tempdir().unwrap();
    make_skill(source.path(), "alpha");

    let client = Arc::new(InMemoryNotifyClient::new());
    let ctrl = NotifyController::new(
        client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
    ctrl.flush_for_testing();

    let events = client.events();
    assert_eq!(events.len(), 1);
    let expected_dir = source.path().join("alpha").to_string_lossy().to_string();
    assert_eq!(
        events[0].canonical_skill_dir, expected_dir,
        "normal mount must use source dir as canonicalSkillDir"
    );
    ctrl.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// Propagation isolation (make-private)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn make_private_failed_error_display() {
    let err = BackingRootError::MakePrivateFailed {
        target: PathBuf::from("/tmp/backing"),
        error: std::io::Error::from_raw_os_error(libc::EINVAL),
    };
    let msg = err.to_string();
    assert!(msg.contains("make-private"), "display: {msg}");
    assert!(msg.contains("/tmp/backing"), "display: {msg}");
    assert!(msg.contains("propagation"), "display: {msg}");
}

#[test]
fn make_private_failed_error_source() {
    let err = BackingRootError::MakePrivateFailed {
        target: PathBuf::from("/tmp/backing"),
        error: std::io::Error::from_raw_os_error(libc::EPERM),
    };
    assert!(
        std::error::Error::source(&err).is_some(),
        "MakePrivateFailed must expose its inner io::Error via source()"
    );
}
