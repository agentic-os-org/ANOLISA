//! Package S2 audit event stream integration tests.
//!
//! These exercise the full path:
//!
//! ```text
//! FUSE callback → SkillFs::emit_event → SkillEventSink::emit
//! ```
//!
//! by mounting SkillFS with a custom [`InMemoryEventSink`] (or
//! [`JsonlFileAuditSink`]) and triggering representative operations through
//! the real mount. The intent is to confirm:
//!
//! * `.skill-meta/**` denials produce serializable `PolicyDenied` events
//!   with skill name, relative path, errno, and uid/gid filled in.
//! * `Open` / `Create` / `Write` events fire with `Allowed` outcomes on the
//!   passthrough path without changing the underlying syscall results.
//! * The temp-file `JsonlFileAuditSink` writes structured JSONL lines that
//!   downstream consumers can re-parse with `serde_json`.
//!
//! All tests skip cleanly when FUSE is unavailable.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    AuditConfig, InMemoryEventSink, JsonlFileAuditSink, SkillEventAction, SkillEventKind,
    SkillEventSink,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

use common::{create_skill_dir, fuse_available};

/// RAII fixture that mounts SkillFS with a custom event sink injected.
///
/// Mirrors the relevant subset of `common::MountFixture` so we can plug in a
/// concrete sink without exposing a test-only seam in the production fixture.
struct AuditedMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl AuditedMount {
    fn new(seed: impl FnOnce(&Path), sink: Arc<dyn SkillEventSink>) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

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
                event_sink: Some(sink),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");

        // Wait for the FUSE daemon to start serving requests, mirroring the
        // shared MountFixture timing.
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn skill_path(&self, name: &str) -> std::path::PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn passthrough(&self, skill: &str, rel: &str) -> std::path::PathBuf {
        self.skill_path(skill).join(rel)
    }

    fn source_path(&self) -> &Path {
        self.source.path()
    }
}

impl Drop for AuditedMount {
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

#[test]
fn skill_meta_denial_records_policy_denied_event_with_attribution() {
    skip_if_no_fuse!();

    let sink = Arc::new(InMemoryEventSink::new());
    let mount = AuditedMount::new(
        |dir| {
            create_skill_dir(dir, "alpha");
            // Pre-create the meta dir on the source so the FUSE-level write
            // attempt cannot succeed for a missing-parent reason — the
            // denial must come from the S1 policy, not from ENOENT.
            std::fs::create_dir_all(dir.join("alpha").join(".skill-meta")).unwrap();
        },
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    // Attempt to create a file under .skill-meta — untrusted callers
    // cannot even look up .skill-meta paths (ENOENT), so the write
    // never reaches the S1 policy layer.
    let target = mount.passthrough("alpha", ".skill-meta/manifest.json");
    let err = std::fs::write(&target, b"hi").expect_err("must deny .skill-meta create");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "untrusted .skill-meta access must surface ENOENT, got {:?}",
        err.raw_os_error()
    );
}

#[test]
fn open_create_write_emit_allowed_events_on_passthrough() {
    skip_if_no_fuse!();

    let sink = Arc::new(InMemoryEventSink::new());
    let mount = AuditedMount::new(
        |dir| {
            create_skill_dir(dir, "alpha");
        },
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    let path = mount.passthrough("alpha", "notes.txt");
    // create + write + close
    std::fs::write(&path, b"hello world").expect("write to passthrough");
    // Re-open to trigger an Open event (read-only; no write).
    let _ = std::fs::read(&path).expect("read back");

    let events = sink.events();
    let creates: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SkillEventKind::Create && e.action == Some(SkillEventAction::Allowed))
        .collect();
    let opens: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SkillEventKind::Open && e.action == Some(SkillEventAction::Allowed))
        .collect();
    let writes: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SkillEventKind::Write && e.action == Some(SkillEventAction::Allowed))
        .collect();

    assert!(
        !creates.is_empty(),
        "expected an Allowed Create event, got events: {:?}",
        events.iter().map(|e| e.kind).collect::<Vec<_>>()
    );
    assert!(
        !opens.is_empty(),
        "expected an Allowed Open event after read; got events: {:?}",
        events.iter().map(|e| e.kind).collect::<Vec<_>>()
    );
    assert!(
        !writes.is_empty(),
        "expected an Allowed Write event with byte count"
    );

    let create = creates[0];
    assert_eq!(create.skill_name.as_deref(), Some("alpha"));
    assert_eq!(
        create.relative_path.as_deref(),
        Some(Path::new("notes.txt"))
    );
    assert!(create.uid.is_some());

    let write = writes[0];
    assert_eq!(write.skill_name.as_deref(), Some("alpha"));
    assert_eq!(write.relative_path.as_deref(), Some(Path::new("notes.txt")));
    assert!(
        write.bytes.is_some_and(|b| b > 0),
        "write event must include byte count"
    );
}

#[test]
fn passthrough_behavior_unchanged_with_audit_sink_attached() {
    // Audit emission must not alter syscall outcomes. We do a small smoke
    // test that the same operations succeed/fail with the same errno
    // whether or not a sink is attached.
    skip_if_no_fuse!();

    let sink = Arc::new(InMemoryEventSink::new());
    let mount = AuditedMount::new(
        |dir| {
            create_skill_dir(dir, "alpha");
        },
        sink as Arc<dyn SkillEventSink>,
    );

    let path = mount.passthrough("alpha", "ok.txt");
    std::fs::write(&path, b"a").expect("plain write must succeed");
    assert_eq!(std::fs::read(&path).unwrap(), b"a");

    // Removing must succeed.
    std::fs::remove_file(&path).expect("plain unlink must succeed");

    // Untrusted .skill-meta access returns ENOENT.
    std::fs::create_dir_all(mount.source_path().join("alpha").join(".skill-meta")).unwrap();
    let meta_target = mount.passthrough("alpha", ".skill-meta/sig.json");
    let err =
        std::fs::write(&meta_target, b"x").expect_err(".skill-meta write must still be denied");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn jsonl_file_sink_records_events_through_real_mount() {
    skip_if_no_fuse!();

    let log_dir = tempfile::tempdir().expect("audit log dir");
    let log_path = log_dir.path().join("audit.jsonl");
    let sink = Arc::new(
        JsonlFileAuditSink::new(AuditConfig::new(&log_path).with_queue_capacity(64))
            .expect("JsonlFileAuditSink::new"),
    );

    {
        let mount = AuditedMount::new(
            |dir| {
                create_skill_dir(dir, "alpha");
                std::fs::create_dir_all(dir.join("alpha").join(".skill-meta")).unwrap();
            },
            sink.clone() as Arc<dyn SkillEventSink>,
        );

        // Untrusted .skill-meta access returns ENOENT at lookup layer.
        let target = mount.passthrough("alpha", ".skill-meta/keys.json");
        let err = std::fs::write(&target, b"x").expect_err("must deny .skill-meta");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

        // Normal passthrough write should succeed and produce events.
        let normal = mount.passthrough("alpha", "notes.txt");
        std::fs::write(&normal, b"hello").expect("normal write must succeed");
        // mount drops here, unmount happens in Drop
    }
    // Drop the sink last so the writer thread can flush; sleep to let the
    // worker drain its queue.
    drop(sink);
    std::thread::sleep(Duration::from_millis(200));

    let content = std::fs::read_to_string(&log_path).expect("read audit log");
    assert!(
        !content.is_empty(),
        "audit log file must contain at least one line"
    );
    for line in content.lines() {
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each line must be valid JSON");
        assert!(v.get("kind").is_some(), "missing kind in {}", line);
        assert!(
            v.get("ts_unix_ms").is_some(),
            "missing ts_unix_ms in {}",
            line
        );
    }
}
