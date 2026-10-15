//! Integration tests: hidden-gate rejections emit audit events.
//!
//! #5183's xattr gate refuses a hidden skill with `ENOENT` and a
//! `Rejected` audit record tagged `class=hidden_skill`. The other
//! mutating callbacks (`rename`, `create`, `unlink`, `rmdir`, `mkdir`,
//! `symlink`, `link`) refuse the same requests with `ENOENT` but emit
//! NOTHING — an audit consumer sees the xattr probes against a hidden
//! skill while the rename/create probes against the very same skill
//! leave no trace. The self-integration audit reproduced both rejections
//! on a real mount with a recording sink: zero `Rejected` op-events.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, InMemoryEventSink, LedgerResolveResult, SkillEventAction, SkillEventKind,
    SkillEventSink,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use common::create_skill_dir;

fn current_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "pass",
            "decision": "current",
            "currentVersion": "v000001",
            "trustedVersion": "v000001"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("current json")
}

fn hidden_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "deny",
            "decision": "hidden",
            "reason": "no trusted version available"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

/// Normal-mode mount with a flip-capable resolver AND a recording event
/// sink, so a test can assert the audit record of a hidden rejection.
struct AuditedFlipMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    resolver: Arc<ActiveSkillResolver>,
    sink: Arc<InMemoryEventSink>,
    _handle: MountHandle,
}

impl AuditedFlipMount {
    fn new_current(skill: &str) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));
        create_skill_dir(source.path(), skill);
        std::fs::write(source.path().join(skill).join("notes.txt"), b"notes\n")
            .expect("seed notes.txt");
        std::fs::create_dir_all(source.path().join(skill).join("subdir")).expect("seed subdir");
        resolver
            .set_from_resolve(&current_result(skill))
            .expect("seed current");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        let sink = Arc::new(InMemoryEventSink::new());
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(resolver.clone()),
                event_sink: Some(sink.clone() as Arc<dyn SkillEventSink>),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            resolver,
            sink,
            _handle: handle,
        }
    }

    fn flip_hidden(&self, skill: &str) {
        self.resolver
            .set_from_resolve(&hidden_result(skill))
            .expect("flip to hidden");
    }

    fn skill(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn source_skill(&self, name: &str) -> PathBuf {
        self.source.path().join(name)
    }

    /// Exactly-one assertion: a single `Rejected`/`ENOENT` record of the
    /// given kind for the skill (and relative path, when given), carrying
    /// the stable `class=hidden_skill` label — not zero (the pre-fix
    /// silence) and not two (double-emit).
    fn assert_one_hidden_rejection(
        &self,
        kind: SkillEventKind,
        skill: &str,
        relative: Option<&str>,
    ) {
        let events = self.sink.events();
        let rejections: Vec<_> = events
            .iter()
            .filter(|e| e.kind == kind && e.action == Some(SkillEventAction::Rejected))
            .filter(|e| e.skill_name.as_deref() == Some(skill))
            .filter(|e| match (relative, e.relative_path.as_deref()) {
                (Some(want), Some(got)) => got == std::path::Path::new(want),
                (None, _) => true,
                (Some(_), None) => false,
            })
            .collect();
        assert_eq!(
            rejections.len(),
            1,
            "exactly one {kind:?}/Rejected record must mention {skill}:{relative:?} (audit-silence or double-emit otherwise), got {:?}",
            rejections
        );
        let ev = rejections[0];
        assert_eq!(ev.errno, Some(libc::ENOENT), "hidden rejection errno");
        let detail = ev.detail.as_deref().unwrap_or_default();
        assert!(
            detail.contains("class=hidden_skill"),
            "the rejection detail must carry the stable class label, got {detail:?}"
        );
    }
}

/// Warm the kernel dentries while the skill resolves `current` (see
/// hidden_write_gate_tests for the stale-dentry rationale).
fn warm_dentries(fx: &AuditedFlipMount, skill: &str) {
    let md = fx.skill(skill).join("SKILL.md");
    let body = std::fs::read_to_string(&md).expect("warm read of manifest");
    assert!(body.contains(skill), "manifest body sanity: {body:?}");
    std::fs::metadata(fx.skill(skill)).expect("warm stat of skill dir");
    std::fs::metadata(fx.skill(skill).join("notes.txt")).expect("warm stat of notes.txt");
    std::fs::metadata(fx.skill(skill).join("subdir")).expect("warm stat of subdir");
}

/// A gated `rename` rejection against a hidden skill must emit exactly
/// one `Rename`/`Rejected`/`ENOENT` op-event tagged `class=hidden_skill`
/// — the same convention #5183's xattr gate established.
#[test]
fn hidden_gate_rename_rejection_must_emit_audit_event() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_gate_rename_rejection_must_emit_audit_event: FUSE not available");
        return;
    }

    let fx = AuditedFlipMount::new_current("demo");
    warm_dentries(&fx, "demo");
    fx.flip_hidden("demo");

    let err = std::fs::rename(
        fx.skill("demo").join("notes.txt"),
        fx.skill("demo").join("gone.txt"),
    )
    .expect_err("rename inside a hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden rename must stay ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("demo").join("notes.txt").is_file(),
        "the hidden source must survive the rejected rename"
    );

    fx.assert_one_hidden_rejection(SkillEventKind::Rename, "demo", Some("notes.txt"));
}

/// A gated `create` rejection against a hidden skill must emit exactly
/// one `Create`/`Rejected`/`ENOENT` op-event tagged `class=hidden_skill`.
#[test]
fn hidden_gate_create_rejection_must_emit_audit_event() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_gate_create_rejection_must_emit_audit_event: FUSE not available");
        return;
    }

    let fx = AuditedFlipMount::new_current("demo");
    warm_dentries(&fx, "demo");
    fx.flip_hidden("demo");

    let err = std::fs::write(fx.skill("demo").join("never-seen.txt"), b"payload\n")
        .expect_err("create inside a hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden create must stay ENOENT, got {err:?}"
    );
    assert!(
        !fx.source_skill("demo").join("never-seen.txt").exists(),
        "no physical file may appear inside the hidden skill"
    );

    fx.assert_one_hidden_rejection(SkillEventKind::Create, "demo", Some("never-seen.txt"));
}

/// The remaining mutating callbacks (`unlink`, `rmdir`, `mkdir`,
/// `symlink`, `link`) reject hidden-skill mutations through the same
/// silent arms — each must emit exactly one attributed, class-tagged
/// `Rejected` record. Each operation runs against a fresh mount with
/// freshly warmed dentries: the kernel's 1s entry timeout is shorter
/// than a five-op sequence, and a stale parent dentry fails the path
/// resolution at lookup (ENOENT, no daemon dispatch) — a different
/// silent path than the one under test.
#[test]
fn hidden_gate_other_mutators_emit_classed_rejections() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_gate_other_mutators_emit_classed_rejections: FUSE not available");
        return;
    }

    // unlink: Delete/Rejected/ENOENT.
    {
        let fx = AuditedFlipMount::new_current("demo");
        warm_dentries(&fx, "demo");
        fx.flip_hidden("demo");
        let err = std::fs::remove_file(fx.skill("demo").join("notes.txt"))
            .expect_err("unlink inside a hidden skill must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        fx.assert_one_hidden_rejection(SkillEventKind::Delete, "demo", Some("notes.txt"));
    }

    // mkdir: Create/Rejected/ENOENT.
    {
        let fx = AuditedFlipMount::new_current("demo");
        warm_dentries(&fx, "demo");
        fx.flip_hidden("demo");
        let err = std::fs::create_dir(fx.skill("demo").join("new-dir"))
            .expect_err("mkdir inside a hidden skill must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        fx.assert_one_hidden_rejection(SkillEventKind::Create, "demo", Some("new-dir"));
    }

    // rmdir: Delete/Rejected/ENOENT.
    {
        let fx = AuditedFlipMount::new_current("demo");
        warm_dentries(&fx, "demo");
        fx.flip_hidden("demo");
        let err = std::fs::remove_dir(fx.skill("demo").join("subdir"))
            .expect_err("rmdir inside a hidden skill must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        fx.assert_one_hidden_rejection(SkillEventKind::Delete, "demo", Some("subdir"));
    }

    // symlink: SymlinkAttempt/Rejected/ENOENT.
    {
        let fx = AuditedFlipMount::new_current("demo");
        warm_dentries(&fx, "demo");
        fx.flip_hidden("demo");
        let err = std::os::unix::fs::symlink("notes.txt", fx.skill("demo").join("new-link"))
            .expect_err("symlink into a hidden skill must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        fx.assert_one_hidden_rejection(SkillEventKind::SymlinkAttempt, "demo", Some("new-link"));
    }

    // link: HardlinkAttempt/Rejected/ENOENT.
    {
        let fx = AuditedFlipMount::new_current("demo");
        warm_dentries(&fx, "demo");
        fx.flip_hidden("demo");
        let err = std::fs::hard_link(
            fx.skill("demo").join("notes.txt"),
            fx.skill("demo").join("hard.txt"),
        )
        .expect_err("hardlink into a hidden skill must be rejected");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        fx.assert_one_hidden_rejection(SkillEventKind::HardlinkAttempt, "demo", Some("hard.txt"));
    }
}
