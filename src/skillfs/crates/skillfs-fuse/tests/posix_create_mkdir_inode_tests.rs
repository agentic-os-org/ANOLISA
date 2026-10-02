//! T0.1 integration coverage for the three small POSIX gaps the
//! external pjdfstest baseline surfaced:
//!
//!   1. `create()` honors mode & ~umask;
//!   2. `mkdir()`  honors mode & ~umask;
//!   3. `getattr()` for `PathType::Passthrough` returns the
//!      SkillFS-allocated inode (not 0).
//!
//! These run through `MountFixture::normal` and require FUSE.

use std::ffi::CString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Mutex;

use common::{MountFixture, create_skill_dir};

mod common;

// ─────────────────────────────────────────────────────────────────────────────
// Umask RAII guard
//
// `umask(2)` mutates process-global state, so two tests that change it must
// not run concurrently. `cargo test` defaults to multi-threaded execution
// and FUSE-backed tests in this suite all serialize on this mutex via the
// `umask_guard()` helper. The guard's `Drop` restores the previous umask
// even when an assertion panics mid-test, which is the constraint the
// reviewer explicitly called out for T0.1.
// ─────────────────────────────────────────────────────────────────────────────

static UMASK_LOCK: Mutex<()> = Mutex::new(());

struct UmaskGuard {
    previous: libc::mode_t,
    // Held for the lifetime of the guard so other umask-touching tests
    // are forced to serialize. The compiler-unused field is intentional.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl UmaskGuard {
    fn new(mask: libc::mode_t) -> Self {
        // Poisoned-mutex recovery: if a prior test panicked under this
        // lock the umask may already be wrong. Take the poisoned guard
        // and forcefully re-establish a known state.
        let lock = UMASK_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: umask is async-signal-safe and always succeeds.
        let previous = unsafe { libc::umask(mask) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: same as above.
        unsafe { libc::umask(self.previous) };
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1 — create() honors mode & ~umask
// ─────────────────────────────────────────────────────────────────────────────

/// `(requested_mode, umask, expected_perm)` — pjdfstest open/00.t cases.
const CREATE_CASES: &[(libc::mode_t, libc::mode_t, libc::mode_t)] = &[
    (0o755, 0o077, 0o700),
    (0o151, 0o077, 0o100),
    (0o345, 0o070, 0o305),
    (0o345, 0o501, 0o244),
];

#[test]
fn test_create_honors_mode_and_umask() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness");
    });
    let sandbox_src = fx.source_skill_path("harness").join("sandbox");
    std::fs::create_dir_all(&sandbox_src).expect("seed sandbox dir");
    let sandbox_mnt = fx.skill_path("harness").join("sandbox");

    for (idx, &(mode, mask, want_perm)) in CREATE_CASES.iter().enumerate() {
        let name = format!("f{idx}");
        let mount_path = sandbox_mnt.join(&name);
        let source_path = sandbox_src.join(&name);

        let perm = {
            let _guard = UmaskGuard::new(mask);
            let c_path = CString::new(mount_path.to_str().unwrap()).expect("CString path");
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_CREAT | libc::O_WRONLY,
                    mode as libc::c_uint,
                )
            };
            assert!(
                fd >= 0,
                "open(O_CREAT,{mode:#o}) under umask {mask:#o} failed: {}",
                std::io::Error::last_os_error()
            );
            unsafe { libc::close(fd) };
            // Read mode from the source side so we observe what really
            // landed on the underlying filesystem, bypassing any FUSE
            // attribute cache.
            std::fs::metadata(&source_path)
                .expect("source metadata")
                .permissions()
                .mode()
                & 0o7777
        };

        assert_eq!(
            perm, want_perm,
            "create({name}, mode={mode:#o}, umask={mask:#o}) expected perm {want_perm:#o}, got {perm:#o}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2 — mkdir() honors mode & ~umask
// ─────────────────────────────────────────────────────────────────────────────

/// pjdfstest mkdir/00.t cases.
const MKDIR_CASES: &[(libc::mode_t, libc::mode_t, libc::mode_t)] = &[
    (0o755, 0o077, 0o700),
    (0o151, 0o077, 0o100),
    (0o345, 0o070, 0o305),
    (0o345, 0o501, 0o244),
];

#[test]
fn test_mkdir_honors_mode_and_umask() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness");
    });
    let sandbox_src = fx.source_skill_path("harness").join("sandbox");
    std::fs::create_dir_all(&sandbox_src).expect("seed sandbox dir");
    let sandbox_mnt = fx.skill_path("harness").join("sandbox");

    for (idx, &(mode, mask, want_perm)) in MKDIR_CASES.iter().enumerate() {
        let name = format!("d{idx}");
        let mount_path = sandbox_mnt.join(&name);
        let source_path = sandbox_src.join(&name);

        let perm = {
            let _guard = UmaskGuard::new(mask);
            let c_path = CString::new(mount_path.to_str().unwrap()).expect("CString path");
            let rc = unsafe { libc::mkdir(c_path.as_ptr(), mode as libc::c_uint) };
            assert_eq!(
                rc,
                0,
                "mkdir({name}, {mode:#o}) under umask {mask:#o} failed: {}",
                std::io::Error::last_os_error()
            );
            std::fs::metadata(&source_path)
                .expect("source metadata")
                .permissions()
                .mode()
                & 0o7777
        };

        assert_eq!(
            perm, want_perm,
            "mkdir({name}, mode={mode:#o}, umask={mask:#o}) expected perm {want_perm:#o}, got {perm:#o}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3 — getattr() for Passthrough returns the SkillFS inode
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_passthrough_getattr_ino_matches_lookup() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness");
        let sandbox = src.join("harness/sandbox");
        std::fs::create_dir_all(&sandbox).expect("seed sandbox");
        std::fs::write(sandbox.join("file"), b"x").expect("seed file");
        std::fs::create_dir(sandbox.join("dir")).expect("seed dir");
    });

    let file_path = fx.skill_path("harness").join("sandbox").join("file");
    let dir_path = fx.skill_path("harness").join("sandbox").join("dir");

    // Each stat() goes through the kernel: the FIRST stat triggers lookup
    // (allocates the SkillFS inode), the SECOND stat hits getattr against
    // that cached inode. Before the T0.1 fix, getattr returned ino=0 for
    // Passthrough and the second stat reported a different inode value.
    let file_ino_a = std::fs::metadata(&file_path).expect("stat file #1").ino();
    let file_ino_b = std::fs::metadata(&file_path).expect("stat file #2").ino();
    let dir_ino_a = std::fs::metadata(&dir_path).expect("stat dir #1").ino();
    let dir_ino_b = std::fs::metadata(&dir_path).expect("stat dir #2").ino();

    assert_ne!(file_ino_a, 0, "passthrough file inode must not be 0");
    assert_ne!(dir_ino_a, 0, "passthrough dir inode must not be 0");
    assert_eq!(
        file_ino_a, file_ino_b,
        "passthrough file inode must be stable across stat() calls"
    );
    assert_eq!(
        dir_ino_a, dir_ino_b,
        "passthrough dir inode must be stable across stat() calls"
    );
    assert_ne!(
        file_ino_a, dir_ino_a,
        "sibling passthrough entries must have distinct inodes"
    );
}
