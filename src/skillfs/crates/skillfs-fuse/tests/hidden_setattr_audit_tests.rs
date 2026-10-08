//! Integration tests: hidden-skill `setattr` rejections emit audit events.
//!
//! #5183 (d7a3af609) established the hidden-gate audit convention on the
//! xattr mutators: a hidden skill's metadata mutation is refused with
//! `ENOENT` and leaves a `Rejected` record tagged `hidden_skill`, so an
//! audit consumer can see the probe. `setattr` enforces the same hidden
//! gate (`chmod`/`chown`/`truncate`/`utimens` on a ledger-hidden skill
//! answer `ENOENT`) but emits NOTHING at either reject branch — the
//! `SkillDir` metadata arm and the shared I4/H3 manifest/passthrough
//! arm. An audit consumer watching a hidden skill sees the xattr probes
//! while the very same skill's chmod/truncate probes leave no trace.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, InMemoryEventSink, LedgerResolveResult, SkillEvent, SkillEventAction,
    SkillEventKind, SkillEventSink,
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

/// Normal-mode mount with a recording event sink whose resolver stays
/// reachable so a test can flip a skill to `hidden` after the dentries
/// are warm — the stale-dentry race the hidden gate closes.
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
        std::fs::write(source.path().join(skill).join("target.txt"), b"target\n")
            .expect("seed target.txt");
        resolver
            .set_from_resolve(&current_result(skill))
            .expect("seed current");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        let sink = Arc::new(InMemoryEventSink::new());
        let sink_dyn: Arc<dyn SkillEventSink> = sink.clone();
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(resolver.clone()),
                event_sink: Some(sink_dyn),
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

    /// The `Rejected` metadata events tagged `class=hidden_skill` recorded
    /// so far — the setattr audit trail these tests demand.
    fn hidden_reject_events(&self) -> Vec<SkillEvent> {
        self.sink
            .events()
            .into_iter()
            .filter(|e| e.kind == SkillEventKind::Metadata)
            .filter(|e| e.action == Some(SkillEventAction::Rejected))
            .filter(|e| e.errno == Some(libc::ENOENT))
            .filter(|e| {
                e.detail
                    .as_deref()
                    .is_some_and(|d| d.contains("class=hidden_skill"))
            })
            .collect()
    }
}

/// Warm the kernel dentries for the skill dir, manifest, and payload
/// while the skill still resolves `current`.
fn warm_dentries(fx: &AuditedFlipMount, skill: &str) {
    let md = fx.skill(skill).join("SKILL.md");
    let body = std::fs::read_to_string(&md).expect("warm read of manifest");
    assert!(body.contains(skill), "manifest body sanity: {body:?}");
    std::fs::metadata(fx.skill(skill)).expect("warm stat of skill dir");
    std::fs::metadata(fx.skill(skill).join("target.txt")).expect("warm stat of target.txt");
}

/// chmod a hidden skill's manifest: refused with `ENOENT` AND audited
/// with exactly one `class=hidden_skill` `Rejected` metadata event.
#[test]
fn hidden_manifest_chmod_is_audited() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_manifest_chmod_is_audited: FUSE not available");
        return;
    }
    let fx = AuditedFlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let err = std::fs::set_permissions(
        fx.skill("hidden-skill").join("SKILL.md"),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect_err("chmod of hidden manifest must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden manifest chmod must be ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("hidden-skill").join("SKILL.md").is_file(),
        "physical manifest must survive the rejected chmod"
    );

    let events = fx.hidden_reject_events();
    assert_eq!(
        events.len(),
        1,
        "chmod of a hidden skill must emit exactly one class=hidden_skill Rejected metadata event"
    );
    assert_eq!(events[0].skill_name.as_deref(), Some("hidden-skill"));
}

/// truncate(2) a hidden skill's passthrough payload: refused with
/// `ENOENT` AND audited with exactly one `class=hidden_skill` event.
/// Path-based truncate dispatches `setattr(size)` directly, so the probe
/// exercises the setattr gate (a truncating open is refused earlier, at
/// the open-time hidden gate).
#[test]
fn hidden_passthrough_truncate_is_audited() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_passthrough_truncate_is_audited: FUSE not available");
        return;
    }
    let fx = AuditedFlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let target = fx.skill("hidden-skill").join("target.txt");
    let c_path = CString::new(target.to_str().unwrap()).unwrap();
    let ret = unsafe { libc::truncate(c_path.as_ptr(), 0) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(ret, -1, "truncate of hidden passthrough must fail");
    assert_eq!(
        err,
        libc::ENOENT,
        "hidden passthrough truncate must be ENOENT, got {err}"
    );
    let body = std::fs::read(fx.source_skill("hidden-skill").join("target.txt"))
        .expect("physical payload survives");
    assert_eq!(body, b"target\n", "truncate must not reach the source");

    let events = fx.hidden_reject_events();
    assert_eq!(
        events.len(),
        1,
        "truncate of a hidden skill must emit exactly one class=hidden_skill Rejected metadata event"
    );
    assert_eq!(events[0].skill_name.as_deref(), Some("hidden-skill"));
    assert_eq!(
        events[0].relative_path.as_deref(),
        Some(Path::new("target.txt"))
    );
}

/// utimens a hidden skill's manifest: refused with `ENOENT` AND audited
/// with exactly one `class=hidden_skill` event.
#[test]
fn hidden_manifest_utimens_is_audited() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_manifest_utimens_is_audited: FUSE not available");
        return;
    }
    let fx = AuditedFlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let manifest = fx.skill("hidden-skill").join("SKILL.md");
    let c_path = CString::new(manifest.to_str().unwrap()).unwrap();
    let pinned = libc::timespec {
        tv_sec: 1_000_000,
        tv_nsec: 0,
    };
    let times = [pinned, pinned];
    let ret = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(ret, -1, "utimens of hidden manifest must fail");
    assert_eq!(
        err,
        libc::ENOENT,
        "hidden manifest utimens must be ENOENT, got {err}"
    );

    let events = fx.hidden_reject_events();
    assert_eq!(
        events.len(),
        1,
        "utimens of a hidden skill must emit exactly one class=hidden_skill Rejected metadata event"
    );
    assert_eq!(events[0].skill_name.as_deref(), Some("hidden-skill"));
}

/// chmod on the hidden skill's DIRECTORY (the `SkillDir` metadata arm):
/// refused with `ENOENT` AND audited with exactly one
/// `class=hidden_skill` event.
#[test]
fn hidden_skill_dir_chmod_is_audited() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_skill_dir_chmod_is_audited: FUSE not available");
        return;
    }
    let fx = AuditedFlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let err = std::fs::set_permissions(
        fx.skill("hidden-skill"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect_err("chmod of hidden skill dir must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden skill dir chmod must be ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("hidden-skill").is_dir(),
        "physical skill dir must survive the rejected chmod"
    );

    let events = fx.hidden_reject_events();
    assert_eq!(
        events.len(),
        1,
        "chmod on a hidden skill dir must emit exactly one class=hidden_skill Rejected metadata event"
    );
    assert_eq!(events[0].skill_name.as_deref(), Some("hidden-skill"));
}
