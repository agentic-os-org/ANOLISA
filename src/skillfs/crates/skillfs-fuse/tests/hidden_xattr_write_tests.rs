//! I4 hidden-skill write gate for the xattr mutators (`setxattr` /
//! `removexattr`).
//!
//! Every mutating callback refuses to touch a ledger-hidden skill's live
//! source; the xattr mutators must refuse too. The kernel dispatches
//! `fsetxattr`/`fremovexattr` directly on a file descriptor without a fresh
//! lookup, so the interesting vector is an fd opened while the skill still
//! resolved `current` and held open across the ledger flip — the stale inode
//! outlives the hiding decision exactly as it does for `write`.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{ActiveSkillResolver, LedgerResolveResult};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use common::{create_skill_dir, fuse_available};

// ── libc xattr helpers ──────────────────────────────────────────────────────

fn io_errno() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn lset_xattr(path: &Path, name: &str, val: &[u8]) -> std::io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let c_name = CString::new(name).unwrap();
    let rc = unsafe {
        libc::lsetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            val.as_ptr() as *const libc::c_void,
            val.len(),
            0,
        )
    };
    if rc < 0 { Err(io_errno()) } else { Ok(()) }
}

fn lget_xattr(path: &Path, name: &str) -> std::io::Result<Vec<u8>> {
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let c_name = CString::new(name).unwrap();
    let needed =
        unsafe { libc::lgetxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(io_errno());
    }
    let mut buf = vec![0u8; needed as usize];
    let n = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if n < 0 {
        return Err(io_errno());
    }
    buf.truncate(n as usize);
    Ok(buf)
}

fn fset_xattr(fd: i32, name: &str, val: &[u8]) -> std::io::Result<()> {
    let c_name = CString::new(name).unwrap();
    let rc = unsafe {
        libc::fsetxattr(
            fd,
            c_name.as_ptr(),
            val.as_ptr() as *const libc::c_void,
            val.len(),
            0,
        )
    };
    if rc < 0 { Err(io_errno()) } else { Ok(()) }
}

fn fremovexattr(fd: i32, name: &str) -> std::io::Result<()> {
    let c_name = CString::new(name).unwrap();
    let rc = unsafe { libc::fremovexattr(fd, c_name.as_ptr()) };
    if rc < 0 { Err(io_errno()) } else { Ok(()) }
}

/// Skip the test when the substrate hosting the source tempdir refuses
/// `user.*` xattrs entirely (some tmpfs mounts return ENOTSUP): the positive
/// control could not distinguish "gate holds" from "nothing works".
fn user_xattrs_supported() -> bool {
    let probe = tempfile::tempdir().expect("probe tempdir");
    match lset_xattr(probe.path(), "user.skillfs.probe", b"1") {
        Ok(()) => true,
        Err(e)
            if e.raw_os_error() == Some(libc::ENOTSUP)
                || e.raw_os_error() == Some(libc::EOPNOTSUPP) =>
        {
            eprintln!(
                "SKIP: substrate at {:?} refuses user.* xattrs",
                probe.path()
            );
            false
        }
        Err(e) => panic!("unexpected errno probing user.* xattrs: {e}"),
    }
}

// ── fixture (minimal resolver-aware mount, as in ledger tests) ──────────────

struct GateMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl GateMount {
    fn new<S, R>(seed: S, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());
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
                active_resolver: resolver,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn skill_dir(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn source_skill_dir(&self, name: &str) -> PathBuf {
        self.source.path().join(name)
    }
}

impl Drop for GateMount {
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

fn current_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{"schemaVersion": 1, "skillName": "{skill}", "status": "pass", "decision": "current", "currentVersion": "v000001", "trustedVersion": "v000001"}}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("current json")
}

fn hidden_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{"schemaVersion": 1, "skillName": "{skill}", "status": "deny", "decision": "hidden", "reason": "no trusted version available"}}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

fn wait_for_mount_path(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::fs::symlink_metadata(path).is_err() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        std::fs::symlink_metadata(path).is_ok(),
        "mounted view never served {}",
        path.display()
    );
}

/// Mount with `demo` resolving `current`, keeping a handle on the resolver so
/// the test can flip it to `hidden` while a stale fd holds the inode.
fn mount_current_with_holder() -> (GateMount, Arc<ActiveSkillResolver>) {
    let holder: std::sync::Mutex<Option<Arc<ActiveSkillResolver>>> = std::sync::Mutex::new(None);
    let mount = GateMount::new(
        |src| {
            create_skill_dir(src, "demo");
            std::fs::write(src.join("demo/data.txt"), "live-body").expect("write live file");
        },
        |src_root| {
            let r = Arc::new(ActiveSkillResolver::new(src_root.to_path_buf()));
            r.set_from_resolve(&current_result("demo")).unwrap();
            *holder.lock().unwrap() = Some(Arc::clone(&r));
            Some(r)
        },
    );
    let resolver = holder.lock().unwrap().take().unwrap();
    (mount, resolver)
}

// ── tests ───────────────────────────────────────────────────────────────────

/// `fsetxattr` through an fd opened while the skill was `current` must be
/// rejected once the ledger hides the skill, and the live source must be
/// untouched — the hidden source is not mutable through the mount.
#[test]
fn hidden_open_fd_setxattr_is_rejected_and_source_untouched() {
    if !fuse_available() || !user_xattrs_supported() {
        return;
    }

    let (mount, resolver) = mount_current_with_holder();
    lset_xattr(
        &mount.source_skill_dir("demo").join("data.txt"),
        "user.probe",
        b"original",
    )
    .expect("seed source xattr");

    let via_mount = mount.skill_dir("demo").join("data.txt");
    wait_for_mount_path(&via_mount);
    let file = std::fs::File::open(&via_mount).expect("open while current");
    resolver.set_from_resolve(&hidden_result("demo")).unwrap();

    let err = fset_xattr(file.as_raw_fd(), "user.probe", b"tampered")
        .expect_err("setxattr on a hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden skill setxattr must surface ENOENT, got {err:?}"
    );
    let v = lget_xattr(
        &mount.source_skill_dir("demo").join("data.txt"),
        "user.probe",
    )
    .expect("source xattr after attempt");
    assert_eq!(
        String::from_utf8_lossy(&v),
        "original",
        "the live source of a hidden skill must not be mutated through the mount"
    );
}

/// `fremovexattr` through the stale fd must be rejected the same way, and
/// the source xattr must survive.
#[test]
fn hidden_open_fd_removexattr_is_rejected_and_source_untouched() {
    if !fuse_available() || !user_xattrs_supported() {
        return;
    }

    let (mount, resolver) = mount_current_with_holder();
    lset_xattr(
        &mount.source_skill_dir("demo").join("data.txt"),
        "user.probe",
        b"original",
    )
    .expect("seed source xattr");

    let via_mount = mount.skill_dir("demo").join("data.txt");
    wait_for_mount_path(&via_mount);
    let file = std::fs::File::open(&via_mount).expect("open while current");
    resolver.set_from_resolve(&hidden_result("demo")).unwrap();

    let err = fremovexattr(file.as_raw_fd(), "user.probe")
        .expect_err("removexattr on a hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden skill removexattr must surface ENOENT, got {err:?}"
    );
    let v = lget_xattr(
        &mount.source_skill_dir("demo").join("data.txt"),
        "user.probe",
    )
    .expect("source xattr must survive the rejected remove");
    assert_eq!(String::from_utf8_lossy(&v), "original");
}

/// Control: a `current` skill's xattr writes keep working through the mount,
/// both by path and through an open fd.
#[test]
fn current_skill_xattr_writes_still_work() {
    if !fuse_available() || !user_xattrs_supported() {
        return;
    }

    let (mount, resolver) = mount_current_with_holder();
    let _ = &resolver; // stays `current` for the whole test

    let via_mount = mount.skill_dir("demo").join("data.txt");
    wait_for_mount_path(&via_mount);

    lset_xattr(&via_mount, "user.probe", b"by-path").expect("setxattr by path while current");
    let v = lget_xattr(&via_mount, "user.probe").expect("getxattr by path");
    assert_eq!(String::from_utf8_lossy(&v), "by-path");

    let file = std::fs::File::open(&via_mount).expect("open while current");
    fset_xattr(file.as_raw_fd(), "user.probe", b"by-fd").expect("fsetxattr while current");
    let v = lget_xattr(&via_mount, "user.probe").expect("getxattr after fd write");
    assert_eq!(String::from_utf8_lossy(&v), "by-fd");

    fremovexattr(file.as_raw_fd(), "user.probe").expect("fremovexattr while current");
    let err = lget_xattr(&via_mount, "user.probe").expect_err("xattr must be gone after remove");
    assert_eq!(err.raw_os_error(), Some(libc::ENODATA));
}
