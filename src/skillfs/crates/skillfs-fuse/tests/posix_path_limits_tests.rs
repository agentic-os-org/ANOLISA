//! T1 integration coverage for the openat-based long-path fallback in
//! SkillFS FUSE callbacks.
//!
//! pjdfstest `*/03.t` files (`ftruncate`, `mkdir`, `open`, `rename`,
//! `rmdir`, `truncate`, `unlink`) all build a path of `dirgen_max` =
//! `PATH_MAX - 1` bytes relative to `cwd`. Resolved against any non-trivial
//! sandbox cwd the absolute physical path exceeds `PATH_MAX`, and any
//! daemon-side `std::fs::*` on that path returns `ENAMETOOLONG` even though
//! the kernel itself accepted the userspace syscall. T1 routes Passthrough
//! mutations / stats through `*at` family syscalls anchored at the parent
//! dir fd so the leaf component is the only string the syscall sees.
//!
//! Two layers of coverage:
//!
//!   * **Unit tests** (no FUSE) exercise the openat helpers themselves
//!     against a deep on-disk structure that pushes the leaf absolute path
//!     past `PATH_MAX`. These prove the helpers behave like POSIX `*at`
//!     calls on Linux.
//!   * **FUSE end-to-end tests** seed the source side with a deep enough
//!     structure to make the daemon's `std::fs::*(physical)` fail with
//!     `ENAMETOOLONG` while leaving the mount-side path short enough that
//!     the userspace caller can still reach the FUSE callback. The seed
//!     uses a custom mount fixture so the source prefix is longer than the
//!     mountpoint prefix, which is the only configuration where the
//!     daemon-side ENAMETOOLONG path is reachable through the kernel.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use skillfs_core::store::SkillStore;
use skillfs_core::{ParseConfig, SharedSkillStore};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

mod common;

// ─────────────────────────────────────────────────────────────────────────────
// Local helpers (no openat helpers are re-exported from the crate; we
// re-implement the minimum needed surface here so the unit test verifies
// the behavior we depend on rather than the function bodies). Behavior
// parity is checked by walking a deep on-disk directory tree.
// ─────────────────────────────────────────────────────────────────────────────

const PATH_MAX_LINUX: usize = 4096;

fn long_segment(byte: u8, len: usize) -> String {
    String::from_utf8(vec![byte; len]).expect("ascii payload")
}

fn open_dir_for_at(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::io::FromRawFd;
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
}

fn mkdirat_local(dir: &std::fs::File, leaf: &str, mode: u32) -> std::io::Result<()> {
    let c = CString::new(leaf).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode as libc::mode_t) };
    if rc != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Build directories one component at a time until the absolute path of the
/// leaf would exceed `target_abs_len` bytes, then return (deep_parent_path,
/// leaf_name) such that:
///   * `deep_parent_path` fits within `PATH_MAX - 1`
///   * `deep_parent_path / leaf_name` exceeds `PATH_MAX - 1`
fn seed_dirs_to_overflow(base: &Path, target_abs_len: usize) -> (PathBuf, String) {
    assert!(
        target_abs_len > PATH_MAX_LINUX,
        "seed target must exceed PATH_MAX",
    );
    let component = long_segment(b'a', 127);
    let mut current = base.to_path_buf();
    loop {
        let next = current.join(&component);
        // Keep extending until the next dir creation itself is at the brink
        // of PATH_MAX. We deliberately do not let a single `create_dir`
        // call exceed PATH_MAX (the kernel would reject it). The returned
        // (parent, leaf) pair joins to a path past PATH_MAX so the
        // *daemon-side* operation goes through the openat fallback while
        // the *mount-side* relative is shorter and stays reachable from
        // userspace.
        if next.as_os_str().len() + 2 >= PATH_MAX_LINUX {
            return (current, component);
        }
        std::fs::create_dir(&next).expect("seed intermediate dir");
        current = next;
        // Stop once the next leaf join would exceed our overflow target.
        if current.as_os_str().len() + 1 + component.len() > target_abs_len {
            return (current, component);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure POSIX unit test: `mkdirat` / `unlinkat` on a deep tree.
//
// This exists to lock down the behavior the FUSE callbacks depend on: on
// Linux, `mkdir`/`unlink` of an absolute path longer than `PATH_MAX` fail
// with `ENAMETOOLONG`, but `mkdirat`/`unlinkat` against a parent fd + short
// leaf succeed even when the assembled absolute path would have been too
// long. This is the kernel-side primitive T1 plumbs through SkillFS.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn openat_family_handles_paths_over_path_max() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let base = tmp.path().to_path_buf();
    // Push the absolute path one component past PATH_MAX so the direct
    // syscall returns ENAMETOOLONG.
    let target_abs_len = PATH_MAX_LINUX + 100;
    let (deep_parent, leaf) = seed_dirs_to_overflow(&base, target_abs_len);

    let leaf_abs = deep_parent.join(&leaf);
    assert!(
        leaf_abs.as_os_str().len() > PATH_MAX_LINUX - 1,
        "leaf abs path must exceed PATH_MAX-1 (got {})",
        leaf_abs.as_os_str().len()
    );

    // Sanity: direct mkdir on the leaf absolute path must fail with
    // ENAMETOOLONG. If this assertion regresses the kernel-side
    // assumption no longer holds and the openat fallback is moot.
    let direct = std::fs::create_dir(&leaf_abs);
    let direct_err = direct.expect_err("direct mkdir on overlong path must fail");
    assert_eq!(
        direct_err.raw_os_error(),
        Some(libc::ENAMETOOLONG),
        "expected ENAMETOOLONG for direct mkdir of overlong path",
    );

    // openat fallback: open the parent (well under PATH_MAX) and mkdirat
    // with the short leaf component.
    let parent_fd = open_dir_for_at(&deep_parent).expect("open parent dir");
    mkdirat_local(&parent_fd, &leaf, 0o755).expect("mkdirat fallback creates the dir");

    // Cleanup via unlinkat is not needed for the test; the tempdir handles
    // it. But verify the dir is actually visible from the parent fd, which
    // exercises the *at codepath we rely on.
    let c_leaf = CString::new(leaf.as_bytes()).unwrap();
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(parent_fd.as_raw_fd(), c_leaf.as_ptr(), &mut st, 0) };
    assert_eq!(rc, 0, "fstatat on the created leaf should succeed");
    assert!(st.st_mode & libc::S_IFDIR != 0, "leaf must be a directory");
}

// ─────────────────────────────────────────────────────────────────────────────
// FUSE end-to-end: a mount where the source prefix is longer than the
// mountpoint prefix. This is the only configuration where the daemon side
// hits ENAMETOOLONG on the physical path before the kernel rejects the
// caller's mount-side path string.
// ─────────────────────────────────────────────────────────────────────────────

struct LongSourceMount {
    _source_root: tempfile::TempDir,
    _mount_root: tempfile::TempDir,
    source: PathBuf,
    mount: PathBuf,
    handle: Option<MountHandle>,
}

impl LongSourceMount {
    fn new() -> Option<Self> {
        if !common::fuse_available() {
            return None;
        }
        let source_root = tempfile::tempdir().expect("source tempdir");
        let mount_root = tempfile::tempdir().expect("mount tempdir");

        // Pad the source path with ~200 extra bytes so the daemon-side
        // physical path is meaningfully longer than the mount-side path.
        // Even after adding the long sandbox structure, the mount-side
        // path stays well within PATH_MAX so userspace can reach FUSE.
        let pad = "x".repeat(200);
        let padded_source = source_root.path().join(&pad);
        std::fs::create_dir(&padded_source).expect("padded source dir");
        let skill_dir = padded_source.join("h");
        std::fs::create_dir(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            b"---\nname: h\ndescription: t1 long path harness\n---\n",
        )
        .expect("seed SKILL.md");
        std::fs::create_dir(skill_dir.join("s")).expect("sandbox dir");
        // Views config so the skill appears under /skills.
        std::fs::write(
            padded_source.join("skillfs-views.toml"),
            b"[[view]]\nname = \"default\"\ndefault = true\nskills = [\"h\"]\n",
        )
        .expect("views config");

        let mut store = SkillStore::new();
        store.load_from_directory(&padded_source, &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mount_path = mount_root.path().to_path_buf();
        let handle = mount_background_configured(
            &mount_path,
            &padded_source,
            shared,
            MountOptions::default(),
            false,
            MountConfig::default(),
        )
        .ok()?;

        std::thread::sleep(std::time::Duration::from_millis(300));

        Some(Self {
            _source_root: source_root,
            _mount_root: mount_root,
            source: padded_source,
            mount: mount_path,
            handle: Some(handle),
        })
    }

    fn source_sandbox(&self) -> PathBuf {
        self.source.join("h/s")
    }

    fn mount_sandbox(&self) -> PathBuf {
        self.mount.join("skills/h/s")
    }
}

impl Drop for LongSourceMount {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", self.mount.to_str().unwrap()])
            .output();
    }
}

#[test]
fn fuse_mkdir_create_unlink_via_openat_fallback() {
    skip_if_no_fuse!();
    let fx = match LongSourceMount::new() {
        Some(fx) => fx,
        None => return,
    };

    // Seed enough intermediate dirs on the SOURCE side so the daemon-side
    // physical leaf path crosses PATH_MAX while the mount-side path stays
    // accessible to userspace.
    let target_abs_len = PATH_MAX_LINUX + 30;
    let (deep_source_parent, leaf) = seed_dirs_to_overflow(&fx.source_sandbox(), target_abs_len);
    // Mirror the relative structure on the mount side.
    let relative = deep_source_parent
        .strip_prefix(fx.source_sandbox())
        .expect("relative under sandbox")
        .to_owned();

    let mount_parent = fx.mount_sandbox().join(&relative);
    // The mount-side parent must still be readable by the kernel.
    let mount_parent_meta = std::fs::metadata(&mount_parent);
    assert!(
        mount_parent_meta.is_ok(),
        "mount-side parent must exist via FUSE (got {:?})",
        mount_parent_meta.err()
    );

    // The source-side absolute path is intentionally past PATH_MAX so
    // `std::fs::*` against it returns ENAMETOOLONG. We verify state on the
    // source side via fstatat against the parent fd, which fits.
    let source_parent_fd = open_dir_for_at(&deep_source_parent).expect("open source parent fd");

    fn source_leaf_exists(parent_fd: &std::fs::File, leaf: &str) -> bool {
        let c = CString::new(leaf).unwrap();
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::fstatat(
                parent_fd.as_raw_fd(),
                c.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        rc == 0
    }

    // 1) mkdir of a leaf whose physical absolute path exceeds PATH_MAX.
    let mount_leaf = mount_parent.join(&leaf);
    let source_leaf_abs = deep_source_parent.join(&leaf);
    assert!(
        source_leaf_abs.as_os_str().len() > PATH_MAX_LINUX - 1,
        "source leaf must exceed PATH_MAX-1 (got {})",
        source_leaf_abs.as_os_str().len()
    );
    std::fs::create_dir(&mount_leaf)
        .expect("FUSE mkdir of long-physical-path leaf must succeed via openat fallback");
    assert!(
        source_leaf_exists(&source_parent_fd, &leaf),
        "source side must reflect the created dir (via fstatat)",
    );

    // 2) rmdir of the same leaf — exercises the rmdir openat fallback.
    std::fs::remove_dir(&mount_leaf)
        .expect("FUSE rmdir of long-physical-path leaf must succeed via openat fallback");
    assert!(
        !source_leaf_exists(&source_parent_fd, &leaf),
        "source side reflects the removal (via fstatat)",
    );

    // 3) create + unlink — exercises the create / unlink openat fallbacks.
    let file_leaf = format!("{leaf}-file");
    let mount_file = mount_parent.join(&file_leaf);
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&mount_file)
            .expect("FUSE create of long-physical-path file must succeed");
        use std::io::Write;
        f.write_all(b"hi").expect("write through new fd");
    }
    assert!(
        source_leaf_exists(&source_parent_fd, &file_leaf),
        "source side reflects the created file (via fstatat)",
    );
    std::fs::remove_file(&mount_file)
        .expect("FUSE unlink of long-physical-path file must succeed via openat fallback");
    assert!(
        !source_leaf_exists(&source_parent_fd, &file_leaf),
        "source side reflects the unlink (via fstatat)",
    );
}

// pjdfstest `ftruncate/03.t` and `truncate/03.t` expect a path-based
// truncate at `dirgen_max` (= PATH_MAX-1) to succeed. The over-limit
// `nxx` (= PATH_MAX) path is rejected by the kernel before reaching
// FUSE, so we only assert the legal max-length case here.
#[test]
fn fuse_truncate_via_openat_fallback_at_max_path() {
    skip_if_no_fuse!();
    let fx = match LongSourceMount::new() {
        Some(fx) => fx,
        None => return,
    };

    let target_abs_len = PATH_MAX_LINUX + 30;
    let (deep_source_parent, leaf) = seed_dirs_to_overflow(&fx.source_sandbox(), target_abs_len);
    let relative = deep_source_parent
        .strip_prefix(fx.source_sandbox())
        .expect("relative under sandbox")
        .to_owned();
    let mount_parent = fx.mount_sandbox().join(&relative);

    // Seed the file on the source side so we know we are exercising the
    // truncate-of-existing-leaf code path that pjdfstest hits via
    // `expect 0 truncate ${nx} 123`.
    let source_parent_fd = open_dir_for_at(&deep_source_parent).expect("open source parent fd");
    // Seed via openat so the source-side creation is not blocked by the
    // very same PATH_MAX boundary we want to test.
    let leaf_c = CString::new(leaf.as_bytes()).unwrap();
    let seed_fd = unsafe {
        libc::openat(
            source_parent_fd.as_raw_fd(),
            leaf_c.as_ptr(),
            libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    };
    assert!(
        seed_fd >= 0,
        "openat seed failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { libc::close(seed_fd) };

    // Pre-truncate sanity: size 0.
    let mut st0: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            source_parent_fd.as_raw_fd(),
            leaf_c.as_ptr(),
            &mut st0,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(st0.st_size, 0, "seed file starts empty");

    // path-based truncate through FUSE. Use libc::truncate so we go
    // through the kernel's setattr-by-path route, mirroring
    // pjdfstest's `truncate ${nx}`.
    let mount_leaf = mount_parent.join(&leaf);
    let mount_leaf_c = CString::new(mount_leaf.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::truncate(mount_leaf_c.as_ptr(), 123) };
    assert_eq!(
        rc,
        0,
        "FUSE truncate(${{nx}}, 123) must succeed via openat fallback: {}",
        std::io::Error::last_os_error()
    );

    // Source-side size via fstatat (path-based stat would ENAMETOOLONG).
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            source_parent_fd.as_raw_fd(),
            leaf_c.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(rc, 0, "fstatat after truncate succeeds");
    assert_eq!(
        st.st_size, 123,
        "source-side size reflects the FUSE truncate"
    );

    // Mount-side stat must also reflect the new size — pjdfstest's
    // `expect regular,123 stat ${nx}` runs this step after the
    // truncate. Before the setattr-final-metadata fallback landed, the
    // kernel would surface ENAMETOOLONG on the truncate AND keep a
    // stale (size=0) cached attr, so the subsequent stat reported the
    // wrong size even though the truncate had already changed the
    // on-disk size.
    let mut mount_st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::stat(mount_leaf_c.as_ptr(), &mut mount_st) };
    assert_eq!(
        rc,
        0,
        "mount-side stat after truncate must succeed: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        mount_st.st_size, 123,
        "mount-side stat reflects the FUSE truncate"
    );
}

// `OsString` is intentionally imported but unused outside of this stub —
// future variants of `seed_dirs_to_overflow` may return it instead of a
// `String` for parity with the FUSE-side helpers.
const _: Option<OsString> = None;
