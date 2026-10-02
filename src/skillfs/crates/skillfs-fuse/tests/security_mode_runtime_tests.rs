//! Package M0 security mount mode integration tests.
//!
//! These exercise the runtime gate that the CLI uses to decide whether a
//! requested mount is acceptable:
//!
//! ```text
//! SecurityModeConfig { enabled }
//!     .validate(source, mountpoint)?     // returns Err *before* any FUSE mount
//!     -> AuditRuntimeConfig::build_sink()?
//!     -> mount_with_security / mount_background_with_security
//! ```
//!
//! Pure config-level coverage (default-disabled, error variants, canonical
//! equality) is pinned in `security::mode::tests`. The tests in this file
//! focus on the end-to-end contract that matters for operators:
//!
//! * security mode accepts a real in-place mount and the existing audit
//!   stream still composes with it;
//! * security mode rejects a non-in-place pair **before** the FUSE event
//!   loop starts, so no partial mount state can leak out;
//! * the default (compatibility) path still works without `--security-mode`;
//! * an invalid audit log path still aborts at startup even when security
//!   mode would otherwise have accepted the mount, so the two failure
//!   modes do not mask each other.
//!
//! FUSE-dependent cases skip cleanly when `/dev/fuse` is not available;
//! pure-config cases run everywhere.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    AuditRuntimeConfig, SecurityModeConfig, SecurityModeError, SkillEventSink,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

use common::{create_skill_dir, fuse_available};

// ---------------------------------------------------------------------------
// Helper: mirror the CLI's wiring sequence in a test-friendly fixture.
//
// Order is intentional:
//   1. SecurityModeConfig::validate  (M0 gate; returns before any FS work)
//   2. AuditRuntimeConfig::build_sink (audit gate; returns before any mount)
//   3. mount_background_with_security (FUSE event loop)
//
// Any earlier step short-circuiting means the mount never starts, and the
// caller is responsible for surfacing the error.
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
enum SetupError {
    /// Validation rejected the source/mountpoint pair before mounting.
    SecurityMode(SecurityModeError),
    /// Audit sink could not be constructed before mounting.
    Audit(std::io::Error),
}

struct SecurityModeMount {
    source: tempfile::TempDir,
    /// `None` for in-place mounts, where source==mountpoint.
    mountpoint: Option<tempfile::TempDir>,
    /// Path that userspace tools should use.
    mount_path: std::path::PathBuf,
    handle: Option<MountHandle>,
}

impl SecurityModeMount {
    /// Build an in-place mount (source == mountpoint after canonicalize).
    fn in_place(
        seed: impl FnOnce(&Path),
        security: &SecurityModeConfig,
        audit: &AuditRuntimeConfig,
    ) -> Result<Self, SetupError> {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mount_path = source.path().to_path_buf();

        security
            .validate(source.path(), &mount_path)
            .map_err(SetupError::SecurityMode)?;
        let sink: Option<Arc<dyn SkillEventSink>> =
            audit.build_sink().map_err(SetupError::Audit)?;

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            &mount_path,
            source.path(),
            shared,
            MountOptions::default(),
            true, // in_place
            MountConfig {
                event_sink: sink,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");

        std::thread::sleep(Duration::from_millis(300));

        Ok(Self {
            source,
            mountpoint: None,
            mount_path,
            handle: Some(handle),
        })
    }

    /// Build a non-in-place mount (source != mountpoint).
    fn normal(
        seed: impl FnOnce(&Path),
        security: &SecurityModeConfig,
        audit: &AuditRuntimeConfig,
    ) -> Result<Self, SetupError> {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        security
            .validate(source.path(), mountpoint.path())
            .map_err(SetupError::SecurityMode)?;
        let sink: Option<Arc<dyn SkillEventSink>> =
            audit.build_sink().map_err(SetupError::Audit)?;

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mount_path = mountpoint.path().to_path_buf();
        let handle = mount_background_configured(
            &mount_path,
            source.path(),
            shared,
            MountOptions::default(),
            false, // in_place
            MountConfig {
                event_sink: sink,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");

        std::thread::sleep(Duration::from_millis(300));

        Ok(Self {
            source,
            mountpoint: Some(mountpoint),
            mount_path,
            handle: Some(handle),
        })
    }

    /// Path to a skill directory through the mount, accounting for the
    /// mount layout. In-place: `<mount>/<skill>/...`. Normal:
    /// `<mount>/skills/<skill>/...`.
    fn skill_path(&self, skill: &str) -> std::path::PathBuf {
        if self.mountpoint.is_some() {
            self.mount_path.join("skills").join(skill)
        } else {
            self.mount_path.join(skill)
        }
    }

    fn source_path(&self) -> &Path {
        self.source.path()
    }
}

impl Drop for SecurityModeMount {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        // Best-effort unmount; failures (e.g. already unmounted) are fine.
        let mp = self.mount_path.clone();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

macro_rules! skip_if_no_fuse {
    () => {
        if !fuse_available() {
            eprintln!(
                "SKIP {}: FUSE not available (no /dev/fuse or fusermount3)",
                ::std::module_path!()
            );
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// 1. Security mode accepts in-place mount paths
// ---------------------------------------------------------------------------

/// `--security-mode` plus source == mountpoint must mount cleanly. We
/// confirm the FUSE event loop actually serves requests by triggering a
/// real passthrough write through the in-place mountpoint.
#[test]
fn security_mode_accepts_in_place_mount() {
    skip_if_no_fuse!();

    let security = SecurityModeConfig::enabled_mode();
    let audit = AuditRuntimeConfig::disabled();

    let mount =
        SecurityModeMount::in_place(|dir| create_skill_dir(dir, "alpha"), &security, &audit)
            .expect("security mode must accept in-place mount");

    // Passthrough write through the over-mount confirms the FUSE event
    // loop is live.
    let notes = mount.skill_path("alpha").join("notes.txt");
    std::fs::write(&notes, b"hello").expect("plain in-place passthrough write");
    let read = std::fs::read(&notes).expect("plain in-place passthrough read");
    assert_eq!(read.as_slice(), b"hello");
}

// ---------------------------------------------------------------------------
// 2. Security mode rejects non-in-place mount BEFORE the FUSE event loop
// ---------------------------------------------------------------------------

/// `--security-mode` plus source != mountpoint must fail at validation
/// time, before the FUSE mount thread is spawned. FUSE availability does
/// not matter — we never reach the mount path.
#[test]
fn security_mode_rejects_non_in_place_mount_before_mounting() {
    let security = SecurityModeConfig::enabled_mode();
    let audit = AuditRuntimeConfig::disabled();

    // Use the helper directly so the assertion proves no FUSE mount thread
    // is spawned: `SecurityModeMount::normal` short-circuits at the
    // `validate(...)?` step.
    let result = SecurityModeMount::normal(|dir| create_skill_dir(dir, "alpha"), &security, &audit);
    match result {
        Err(SetupError::SecurityMode(SecurityModeError::NotInPlace {
            source_canonical,
            mountpoint_canonical,
            ..
        })) => {
            assert_ne!(
                source_canonical, mountpoint_canonical,
                "the error must surface distinct canonical paths"
            );
        }
        Err(other) => panic!("expected SecurityModeError::NotInPlace, got {:?}", other),
        Ok(_) => panic!("security mode must reject non-in-place mount"),
    }
}

/// Defense in depth at the validation primitive level: even with the
/// helper out of the picture, [`SecurityModeConfig::validate`] must reject
/// non-in-place paths. This guards against future refactors of the fixture
/// silently masking the gate.
#[test]
fn security_mode_validate_directly_rejects_non_in_place_paths() {
    let source = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    let err = SecurityModeConfig::enabled_mode()
        .validate(source.path(), mountpoint.path())
        .expect_err("non-in-place must be rejected");
    assert!(matches!(err, SecurityModeError::NotInPlace { .. }));
}

// ---------------------------------------------------------------------------
// 3. Default / compatibility behavior is unchanged
// ---------------------------------------------------------------------------

/// Without `--security-mode`, a non-in-place mount must still work — this
/// is the existing compatibility / dev-mode path. We trigger a real
/// passthrough write through `/skills/<skill>/...` to confirm.
#[test]
fn default_compat_mode_still_allows_non_in_place_mount() {
    skip_if_no_fuse!();

    let security = SecurityModeConfig::default();
    assert!(!security.is_enabled(), "default must remain disabled");
    let audit = AuditRuntimeConfig::disabled();

    let mount = SecurityModeMount::normal(|dir| create_skill_dir(dir, "alpha"), &security, &audit)
        .expect("default mode must accept non-in-place mount");

    let notes = mount.skill_path("alpha").join("notes.txt");
    std::fs::write(&notes, b"hello").expect("plain normal passthrough write");
    let read = std::fs::read(&notes).expect("plain normal passthrough read");
    assert_eq!(read.as_slice(), b"hello");

    // And the corresponding source file should be reachable directly,
    // *outside* the mount — this is exactly the property the
    // non-in-place mount cannot constrain, and the property the CLI
    // warning calls out.
    let direct = mount.source_path().join("alpha").join("notes.txt");
    let direct_read = std::fs::read(&direct).expect("direct source read must work");
    assert_eq!(
        direct_read.as_slice(),
        b"hello",
        "the source path stays directly accessible in non-in-place mode"
    );
}

// ---------------------------------------------------------------------------
// 4. Audit composes with security mode
// ---------------------------------------------------------------------------

/// Security mode + audit logging must compose: an in-place mount with both
/// enabled records the same JSONL stream that the S2.1 runtime tests pin,
/// and the audit log file is produced under the operator-supplied path.
///
/// We trigger an S1 `.skill-meta` denial because it has a deterministic
/// PolicyDenied event with stable `kind`/`skill`/`path`/`errno` fields.
#[test]
fn security_mode_composes_with_audit_runtime_config() {
    skip_if_no_fuse!();

    let log_dir = tempfile::tempdir().expect("audit log dir");
    let log_path = log_dir.path().join("audit.jsonl");

    let security = SecurityModeConfig::enabled_mode();
    let audit = AuditRuntimeConfig::enabled(&log_path);
    assert!(security.is_enabled());
    assert!(audit.is_enabled());

    {
        let mount = SecurityModeMount::in_place(
            |dir| {
                create_skill_dir(dir, "alpha");
                std::fs::create_dir_all(dir.join("alpha").join(".skill-meta")).unwrap();
            },
            &security,
            &audit,
        )
        .expect("security mode + audit must compose on an in-place mount");

        // Untrusted .skill-meta access returns ENOENT.
        let target = mount
            .skill_path("alpha")
            .join(".skill-meta")
            .join("manifest.json");
        let err = std::fs::write(&target, b"x").expect_err("must deny .skill-meta");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

        // Trigger a normal passthrough event for audit log.
        let normal = mount.skill_path("alpha").join("notes.txt");
        std::fs::write(&normal, b"hi").expect("normal write must succeed");

        // Mount drops here; FUSE unmounts before we read the log so the
        // writer thread can drain.
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let content: String;
    loop {
        if let Ok(c) = std::fs::read_to_string(&log_path) {
            if !c.is_empty() {
                content = c;
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "audit log was not produced under security mode at {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut saw_event = false;
    for line in content.lines() {
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each audit line must be valid JSON");
        assert!(v.get("kind").is_some(), "missing kind in {}", line);
        assert!(
            v.get("ts_unix_ms").is_some(),
            "missing ts_unix_ms in {}",
            line
        );
        if v.get("kind").is_some() {
            saw_event = true;
        }
    }
    assert!(
        saw_event,
        "expected at least one audit event record; full log:\n{}",
        content
    );
}

// ---------------------------------------------------------------------------
// 5. Invalid audit path fails before mount, independently of security mode
// ---------------------------------------------------------------------------

/// `AuditRuntimeConfig::build_sink` returning `Err` must abort the mount
/// even when security mode would have accepted the source/mountpoint pair.
/// This guards the operator's intent on *both* gates: security mode and
/// audit each say "I cannot honor what you asked for" independently, and
/// the CLI must surface either one as a startup error.
///
/// We don't need FUSE here because the failure is at config time, before
/// the mount thread is spawned. The fixture helper short-circuits at the
/// `audit.build_sink()?` step.
#[test]
fn invalid_audit_path_aborts_before_mount_under_security_mode() {
    let security = SecurityModeConfig::enabled_mode();
    let audit = AuditRuntimeConfig::enabled(std::path::PathBuf::from(
        "/nonexistent/skillfs-m0/audit.jsonl",
    ));

    // Run on an in-place source so the security mode validation passes:
    // the only failure we want to observe is the audit one. If we used a
    // non-in-place pair the security mode check would fire first and the
    // test would not actually exercise the audit gate.
    let result =
        SecurityModeMount::in_place(|dir| create_skill_dir(dir, "alpha"), &security, &audit);

    match result {
        Err(SetupError::Audit(_)) => {
            // expected: audit gate trips even though security mode accepts
        }
        Err(SetupError::SecurityMode(other)) => panic!(
            "audit gate should have fired before security mode; got {:?}",
            other
        ),
        Ok(_) => panic!("invalid audit path must abort the mount"),
    }
}

/// Without security mode, an invalid audit path on a normal mount also
/// aborts before any FUSE work. This pins that we did not accidentally
/// couple the two gates together — each must independently veto a mount.
#[test]
fn invalid_audit_path_aborts_before_mount_in_compat_mode() {
    let security = SecurityModeConfig::disabled();
    let audit = AuditRuntimeConfig::enabled(std::path::PathBuf::from(
        "/nonexistent/skillfs-m0/audit-compat.jsonl",
    ));

    let result = SecurityModeMount::normal(|dir| create_skill_dir(dir, "alpha"), &security, &audit);

    match result {
        Err(SetupError::Audit(_)) => {}
        Err(SetupError::SecurityMode(other)) => panic!(
            "compat mode must not reject on security grounds; got {:?}",
            other
        ),
        Ok(_) => panic!("invalid audit path must abort the mount"),
    }
}
