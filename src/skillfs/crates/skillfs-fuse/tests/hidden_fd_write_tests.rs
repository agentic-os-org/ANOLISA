//! Integration tests: `write` through a stale fd is gated on hidden
//! skills like `fsetxattr` (#5183's contract).
//!
//! An fd opened through the mount while the skill resolved `current`
//! keeps its inode mapping after the ledger flips the skill to `hidden`.
//! `setxattr_impl` / `removexattr_impl` already refuse such an fd with
//! `ENOENT` — the kernel dispatches `fsetxattr` directly on the fd, so a
//! fresh lookup would never run — but `write_impl` consulted only the
//! lifecycle (S3) and `.skill-meta` (S1) gates, never
//! `should_reject_hidden_write`. A `write()` on the very same fd
//! therefore kept mutating the hidden skill's live source: the audit
//! probe smuggled the literal body "smuggled\n" onto the hidden source
//! while `fsetxattr` on the same fd answered `ENOENT`.
//!
//! The open-after-unlink raw-fd branch is deliberately NOT gated: POSIX
//! keeps an fd over an unlinked file writable until last close, and the
//! unlink itself already passed the protection gates. The control test
//! pins that behavior.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
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

/// Whether the substrate hosting the source tempdir serves `user.*`
/// xattrs at all (some tmpfs mounts refuse them with ENOTSUP): the
/// `fsetxattr` control assertion is only meaningful when it does.
fn user_xattrs_supported() -> bool {
    let probe = tempfile::tempdir().expect("probe tempdir");
    let path = std::ffi::CString::new(probe.path().as_os_str().as_encoded_bytes()).unwrap();
    let name = std::ffi::CString::new("user.skillfs.probe").unwrap();
    let rc = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            b"1".as_ptr() as *const libc::c_void,
            1,
            0,
        )
    };
    if rc == 0 {
        return true;
    }
    let e = std::io::Error::last_os_error();
    matches!(e.raw_os_error(), Some(libc::EOPNOTSUPP))
}

/// Normal-mode mount whose resolver stays reachable so a test can flip a
/// skill to `hidden` while a write-capable fd is held open. An optional
/// recording sink can be attached for the audit regression.
struct StaleFdMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    resolver: Arc<ActiveSkillResolver>,
    _handle: MountHandle,
}

impl StaleFdMount {
    fn new_current(skill: &str, body: &str) -> Self {
        Self::new_current_with_sink(skill, body, None)
    }

    fn new_current_with_sink(
        skill: &str,
        body: &str,
        sink: Option<Arc<InMemoryEventSink>>,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));
        create_skill_dir(source.path(), skill);
        std::fs::write(source.path().join(skill).join("data.txt"), body).expect("seed data.txt");
        resolver
            .set_from_resolve(&current_result(skill))
            .expect("seed current");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        let sink_dyn: Option<Arc<dyn SkillEventSink>> = sink.map(|s| s as Arc<dyn SkillEventSink>);
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(resolver.clone()),
                event_sink: sink_dyn,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            resolver,
            _handle: handle,
        }
    }

    fn flip_hidden(&self, skill: &str) {
        self.resolver
            .set_from_resolve(&hidden_result(skill))
            .expect("flip to hidden");
    }

    fn skill_file(&self, name: &str, rel: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name).join(rel)
    }

    fn source_file(&self, name: &str, rel: &str) -> PathBuf {
        self.source.path().join(name).join(rel)
    }
}

/// `write()` on an fd opened while the skill resolved `current` must be
/// rejected once the ledger hides the skill — exactly like `fsetxattr` on
/// the same fd (#5183) — and the hidden skill's live source must be
/// untouched. Pre-fix, the write succeeded and the literal body
/// "smuggled\n" landed on the hidden source.
#[test]
fn stale_fd_write_after_hidden_flip_is_rejected_like_fsetxattr() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP stale_fd_write_after_hidden_flip_is_rejected_like_fsetxattr: FUSE not available"
        );
        return;
    }

    let fx = StaleFdMount::new_current("demo", "live-body");

    let via_mount = fx.skill_file("demo", "data.txt");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&via_mount)
        .expect("open write fd while current");
    fx.flip_hidden("demo");

    let err = file
        .write_all(b"smuggled\n")
        .expect_err("write through a stale fd on a hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "stale-fd write on a hidden skill must surface ENOENT like fsetxattr, got {err:?}"
    );

    let body = std::fs::read_to_string(fx.source_file("demo", "data.txt"))
        .expect("source body after the rejected write");
    assert_eq!(
        body, "live-body",
        "the hidden skill's live source must not be mutated through the mount"
    );

    // Control mirroring the audit probe: `fsetxattr` on the SAME fd is
    // the #5183 gate and answers ENOENT — write must agree with it.
    if user_xattrs_supported() {
        let c_name = std::ffi::CString::new("user.skillfs.probe").unwrap();
        let rc = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c_name.as_ptr(),
                b"x".as_ptr() as *const libc::c_void,
                1,
                0,
            )
        };
        assert!(rc < 0, "fsetxattr on the stale fd must be rejected");
        let e = std::io::Error::last_os_error();
        assert_eq!(
            e.raw_os_error(),
            Some(libc::ENOENT),
            "fsetxattr control must surface ENOENT, got {e:?}"
        );
    }
}

/// The rejected stale-fd write must also be AUDITED: exactly one
/// `Write`/`Rejected`/`ENOENT` record tagged `class=hidden_skill`, the
/// same trace `fsetxattr` on the same fd leaves (#5183's convention).
/// Pre-fix the write was refused correctly but emitted nothing, so the
/// security-boundary probe was untrackable.
#[test]
fn stale_fd_write_rejection_is_audited() {
    if !common::fuse_available() {
        eprintln!("SKIP stale_fd_write_rejection_is_audited: FUSE not available");
        return;
    }

    let sink = Arc::new(InMemoryEventSink::new());
    let fx = StaleFdMount::new_current_with_sink("demo", "live-body", Some(sink.clone()));

    let via_mount = fx.skill_file("demo", "data.txt");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&via_mount)
        .expect("open write fd while current");
    fx.flip_hidden("demo");

    let err = file
        .write_all(b"smuggled\n")
        .expect_err("write through a stale fd on a hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "stale-fd write on a hidden skill must surface ENOENT, got {err:?}"
    );

    let events: Vec<_> = sink
        .events()
        .into_iter()
        .filter(|e| e.kind == SkillEventKind::Write)
        .filter(|e| e.action == Some(SkillEventAction::Rejected))
        .filter(|e| e.errno == Some(libc::ENOENT))
        .filter(|e| {
            e.detail
                .as_deref()
                .is_some_and(|d| d.contains("class=hidden_skill"))
        })
        .collect();
    assert_eq!(
        events.len(),
        1,
        "the rejected stale-fd write must emit exactly one class=hidden_skill Write/Rejected event"
    );
    assert_eq!(events[0].skill_name.as_deref(), Some("demo"));
    assert_eq!(
        events[0].relative_path.as_deref(),
        Some(std::path::Path::new("data.txt"))
    );
}

/// Control: writing through an fd while the skill still resolves
/// `current` keeps working.
#[test]
fn current_skill_fd_write_still_works() {
    if !common::fuse_available() {
        eprintln!("SKIP current_skill_fd_write_still_works: FUSE not available");
        return;
    }

    let fx = StaleFdMount::new_current("demo", "live-body");

    let via_mount = fx.skill_file("demo", "data.txt");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&via_mount)
        .expect("open write fd while current");
    file.write_all(b"fresh-body")
        .expect("fd write while current");

    let body = std::fs::read_to_string(fx.source_file("demo", "data.txt"))
        .expect("source body after the current write");
    assert_eq!(body, "fresh-body");
}

/// Control: the open-after-unlink raw-fd branch keeps POSIX semantics —
/// `write` on an fd whose path mapping is already gone (the file was
/// unlinked through the mount, and that unlink passed the gates because
/// the skill was `current`) must still succeed on the descriptor. The
/// hidden-skill gate must never reach this branch.
#[test]
fn raw_fd_write_after_unlink_keeps_posix_semantics() {
    if !common::fuse_available() {
        eprintln!("SKIP raw_fd_write_after_unlink_keeps_posix_semantics: FUSE not available");
        return;
    }

    let fx = StaleFdMount::new_current("demo", "");

    let via_mount = fx.skill_file("demo", "data.txt");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&via_mount)
        .expect("open rw fd while current");
    file.write_all(b"survives-unlink")
        .expect("write before unlink through the fd");
    std::fs::remove_file(&via_mount).expect("unlink while current drops the path mapping");

    // The path mapping is gone; the write must land on the raw fd.
    file.write_all(b"+tail").expect("raw-fd write after unlink");
    file.seek(SeekFrom::Start(0)).expect("seek back to start");
    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .expect("read back through raw fd");
    assert_eq!(
        buf, "survives-unlink+tail",
        "the raw-fd branch must keep serving POSIX write semantics"
    );
}
