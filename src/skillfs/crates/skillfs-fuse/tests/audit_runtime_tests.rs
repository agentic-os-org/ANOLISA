//! Package S2.1 audit runtime wiring integration tests.
//!
//! These exercise the runtime opt-in path that the CLI/operator uses to turn
//! the JSONL audit sink on:
//!
//! ```text
//! AuditRuntimeConfig { path: Some(...), queue_capacity }
//!     .build_sink()? -> Option<Arc<dyn SkillEventSink>>
//!         -> mount_with_security / mount_background_with_security
//! ```
//!
//! The unit-level shape of [`AuditRuntimeConfig`] (default disables audit,
//! invalid path returns `Err`, capacity normalization) is pinned in
//! `security::audit::tests`. The tests in this file focus on the end-to-end
//! contract:
//!
//! * a default runtime config does not produce any audit log file at mount
//!   time and keeps the FUSE filesystem on its default `NoopEventSink`;
//! * an enabled runtime config produces a JSONL log with at least one
//!   representative event after a real FUSE syscall;
//! * an invalid explicit audit path is rejected before the FUSE mount
//!   starts.
//!
//! All tests skip cleanly when FUSE is unavailable.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{AuditRuntimeConfig, SkillEventSink};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

use common::{create_skill_dir, fuse_available};

/// Mount SkillFS with whatever sink the runtime config produces.
///
/// Mirrors what the CLI does in `cmd_mount` after parsing flags: build the
/// `AuditRuntimeConfig` from user input, ask it for a sink, and pass that
/// sink through to the security-aware mount entry point. Returns `Err` if
/// the runtime config asked for audit logging but the sink could not be
/// constructed — the caller is then expected to refuse the mount.
struct RuntimeMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl RuntimeMount {
    fn new(seed: impl FnOnce(&Path), runtime: &AuditRuntimeConfig) -> std::io::Result<Self> {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        // Surface sink-build errors to the caller before any FUSE mount
        // attempt happens, exactly mirroring the CLI's startup-error policy.
        let sink: Option<Arc<dyn SkillEventSink>> = runtime.build_sink()?;

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                event_sink: sink,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");

        std::thread::sleep(Duration::from_millis(300));

        Ok(Self {
            source,
            mountpoint,
            handle: Some(handle),
        })
    }

    fn passthrough(&self, skill: &str, rel: &str) -> std::path::PathBuf {
        self.mountpoint.path().join("skills").join(skill).join(rel)
    }

    fn source_path(&self) -> &Path {
        self.source.path()
    }
}

impl Drop for RuntimeMount {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        let mp = self.mountpoint.path().to_path_buf();
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

/// Default runtime config (no `--audit-log` flag) must not enable audit
/// logging. The mount succeeds without an audit log file being created
/// anywhere, and no sink is passed to the FUSE filesystem (so the default
/// `NoopEventSink` is preserved).
#[test]
fn default_runtime_config_does_not_enable_audit_logging() {
    skip_if_no_fuse!();

    // Pick a path inside a temp directory and confirm it stays untouched
    // after a real mount + passthrough activity. The runtime config does
    // not point at it, so the file must not appear.
    let probe_dir = tempfile::tempdir().expect("probe dir");
    let probe_path = probe_dir.path().join("audit.jsonl");
    assert!(!probe_path.exists(), "precondition: probe path is empty");

    let runtime = AuditRuntimeConfig::default();
    assert!(!runtime.is_enabled());

    let mount = RuntimeMount::new(|dir| create_skill_dir(dir, "alpha"), &runtime)
        .expect("default runtime config must mount cleanly");

    // Trigger ordinary passthrough activity that *would* fire events if the
    // sink were attached — `Open`/`Create`/`Write` are exactly what S2 added
    // emission for. Audit must remain off so this leaves no JSONL artifact.
    let p = mount.passthrough("alpha", "notes.txt");
    std::fs::write(&p, b"hello").expect("plain write");
    let _ = std::fs::read(&p).expect("plain read");

    assert!(
        !probe_path.exists(),
        "default runtime config must not create any audit log file"
    );
}

/// Enabling audit through the runtime config records a representative event
/// to the on-disk JSONL log via a real FUSE mount.
///
/// We deliberately trigger an S1 `.skill-meta` denial because that path also
/// exercises the policy → audit pipeline that S2's runtime wiring is meant
/// to expose to operators.
#[test]
fn explicit_audit_path_creates_jsonl_log_and_records_event() {
    skip_if_no_fuse!();

    let log_dir = tempfile::tempdir().expect("audit log dir");
    let log_path = log_dir.path().join("audit.jsonl");
    let runtime = AuditRuntimeConfig::enabled(&log_path);
    assert!(runtime.is_enabled());

    {
        let mount = RuntimeMount::new(
            |dir| {
                create_skill_dir(dir, "alpha");
                std::fs::create_dir_all(dir.join("alpha").join(".skill-meta")).unwrap();
            },
            &runtime,
        )
        .expect("enabled runtime config must mount cleanly");

        // Untrusted .skill-meta access returns ENOENT.
        let target = mount.passthrough("alpha", ".skill-meta/manifest.json");
        let err = std::fs::write(&target, b"x").expect_err("must deny .skill-meta");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

        // Produce a passthrough write to confirm allowed events flow
        // through the audit sink.
        let _ = std::fs::write(mount.source_path().join("touchstone"), b"x");
        let path = mount.passthrough("alpha", "notes.txt");
        std::fs::write(&path, b"hi").expect("plain passthrough write");

        // RuntimeMount drops here; FUSE unmounts before we read the log.
    }

    // Wait briefly for the audit writer thread to flush its queue.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut content = String::new();
    loop {
        content.clear();
        if let Ok(c) = std::fs::read_to_string(&log_path) {
            if !c.is_empty() {
                content = c;
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "audit log was not produced through runtime helper at {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut saw_event = false;
    for line in content.lines() {
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each audit line must be valid JSON");
        // Stable required fields.
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

/// An invalid audit path must surface as a startup error from
/// `AuditRuntimeConfig::build_sink` so the CLI can refuse to mount. We model
/// the CLI flow directly: try to build the sink, observe `Err`, never reach
/// `mount_background_with_security`.
#[test]
fn invalid_explicit_audit_path_returns_startup_error_before_mounting() {
    // FUSE availability does not matter — this test exits before the mount
    // path is ever entered, exactly like the CLI startup-error path.

    let bogus = std::path::PathBuf::from("/nonexistent/skillfs-runtime/audit.jsonl");
    let runtime = AuditRuntimeConfig::enabled(&bogus);

    let result = runtime.build_sink();
    assert!(
        result.is_err(),
        "explicit but unwritable audit path must return Err so the runtime refuses to mount"
    );

    // Defense in depth: confirm the `RuntimeMount` helper, which mirrors the
    // CLI's wiring, also surfaces the error before any mount happens. We do
    // not invoke FUSE here; the constructor returns at the `build_sink` step.
    let probe = RuntimeMount::new(|_dir| {}, &runtime);
    assert!(
        probe.is_err(),
        "RuntimeMount must propagate sink-build errors before mounting"
    );
}

/// Zero queue capacity (or missing capacity) must be treated as
/// `DEFAULT_AUDIT_QUEUE_CAPACITY`. We assert the helper's effective capacity
/// directly and confirm a sink built with capacity=0 still services events
/// through a real mount.
#[test]
fn zero_queue_capacity_uses_default_capacity_through_runtime_helper() {
    use skillfs_fuse::security::DEFAULT_AUDIT_QUEUE_CAPACITY;

    // Pure config-level normalization.
    let cfg_zero = AuditRuntimeConfig::default();
    assert_eq!(cfg_zero.queue_capacity, 0);
    assert_eq!(
        cfg_zero.effective_queue_capacity(),
        DEFAULT_AUDIT_QUEUE_CAPACITY
    );

    let cfg_explicit_zero = AuditRuntimeConfig::default().with_queue_capacity(0);
    assert_eq!(
        cfg_explicit_zero.effective_queue_capacity(),
        DEFAULT_AUDIT_QUEUE_CAPACITY
    );

    // End-to-end: build the sink via the runtime helper with capacity=0,
    // mount, fire an event, and confirm the JSONL log records at least one
    // line. A truly 0-capacity bounded channel would always reject in
    // try_send and the file would stay empty; the appearance of any line
    // proves the normalization happened.
    if !fuse_available() {
        eprintln!("SKIP zero_queue_capacity end-to-end: FUSE unavailable");
        return;
    }

    let log_dir = tempfile::tempdir().expect("audit log dir");
    let log_path = log_dir.path().join("audit-zero.jsonl");
    let runtime = AuditRuntimeConfig::enabled(&log_path).with_queue_capacity(0);

    {
        let mount = RuntimeMount::new(
            |dir| {
                create_skill_dir(dir, "alpha");
                std::fs::create_dir_all(dir.join("alpha").join(".skill-meta")).unwrap();
            },
            &runtime,
        )
        .expect("zero-capacity runtime config must mount cleanly");

        // Untrusted .skill-meta access returns ENOENT.
        let target = mount.passthrough("alpha", ".skill-meta/manifest.json");
        let err = std::fs::write(&target, b"x").expect_err("must deny .skill-meta");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        // Trigger a passthrough event to confirm audit plumbing works.
        let _ = std::fs::write(mount.source_path().join("touchstone"), b"x");
        let path = mount.passthrough("alpha", "notes.txt");
        std::fs::write(&path, b"hi").expect("plain passthrough write");
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(c) = std::fs::read_to_string(&log_path) {
            if !c.is_empty() {
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "zero-capacity runtime config produced no audit log at {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
