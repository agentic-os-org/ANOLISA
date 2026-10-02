//! T2 integration coverage for safe link + FIFO compatibility.
//!
//! Package T2 enables:
//!   * `symlink()` for `PathType::Passthrough` leaves whose target lexically
//!     resolves to the same skill — every other classification is rejected
//!     with `EACCES`;
//!   * `link()` (hardlink) for same-skill ordinary passthrough regular files;
//!   * `mknod()` for FIFOs only — sockets and device nodes are rejected with
//!     `EPERM`.
//!
//! All three callbacks continue to honor `.skill-meta`, lifecycle namespaces,
//! and `skill-discover`'s virtual read-only semantics. The tests below exercise
//! each rule end-to-end through a real FUSE mount.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

mod common;

use common::{MountFixture, create_skill_dir, list_dir_names};

// ─────────────────────────────────────────────────────────────────────────────
// Seeding helpers
// ─────────────────────────────────────────────────────────────────────────────

fn write_passthrough(fx: &MountFixture, skill: &str, rel: &str, contents: &[u8]) {
    let source_rel = fx.source_skill_path(skill).join(rel);
    if let Some(parent) = source_rel.parent() {
        std::fs::create_dir_all(parent).expect("seed parent dir");
    }
    std::fs::write(&source_rel, contents).expect("seed passthrough file");
}

fn raw_errno<T>(result: std::io::Result<T>) -> i32 {
    result
        .err()
        .and_then(|e| e.raw_os_error())
        .unwrap_or_else(|| panic!("expected an io::Error with raw_os_error"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Symlink — allowed cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_same_skill_symlink_allowed_relative_target() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    let skill = fx.skill_path("alpha");
    std::fs::create_dir(skill.join("sub")).expect("mkdir sub");
    std::fs::write(skill.join("sub").join("target.txt"), b"hello").expect("seed target");

    let link = skill.join("sub").join("link");
    std::os::unix::fs::symlink("target.txt", &link).expect("symlink same-skill relative");

    let meta = std::fs::symlink_metadata(&link).expect("lstat the link");
    assert!(
        meta.file_type().is_symlink(),
        "lstat must report a symlink, got {:?}",
        meta.file_type()
    );

    let resolved = std::fs::read_link(&link).expect("readlink");
    assert_eq!(resolved.as_os_str(), Path::new("target.txt").as_os_str());

    let entries = list_dir_names(&skill.join("sub"));
    assert!(
        entries.contains(&"link".to_string()),
        "readdir must list the link, got {entries:?}"
    );

    let through_link =
        std::fs::read_to_string(&link).expect("read should follow same-skill symlink");
    assert_eq!(through_link, "hello");
}

#[test]
fn test_same_skill_absolute_symlink_rejected_by_default_policy() {
    skip_if_no_fuse!();

    // T2 default policy refuses absolute symlink targets even when they
    // resolve inside the same skill: in non-in-place mounts the
    // resolved absolute path is the *physical* source path, so
    // following the link from userspace bypasses the FUSE layer along
    // with its audit / `.skill-meta` / lifecycle enforcement. A future
    // package may re-enable absolute targets only under
    // `--security-mode` / in-place mounts where the resolved path
    // still flows through SkillFS.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    write_passthrough(&fx, "alpha", "abs_target.txt", b"abs");

    let link = fx.skill_path("alpha").join("abs_link");
    let abs_target = fx.source_skill_path("alpha").join("abs_target.txt");
    let err = raw_errno(std::os::unix::fs::symlink(&abs_target, &link));
    assert_eq!(
        err,
        libc::EACCES,
        "absolute same-skill symlink must be rejected with EACCES, got {err}"
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "rejected absolute symlink must leave no on-mount entry"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Symlink — rejected cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_cross_skill_symlink_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        create_skill_dir(src, "beta");
    });
    write_passthrough(&fx, "beta", "b.txt", b"b");
    std::fs::create_dir(fx.skill_path("alpha").join("sub")).expect("mkdir sub");

    let link = fx.skill_path("alpha").join("sub").join("cross_link");
    // From `<source>/alpha/sub/`, target `../../beta/b.txt` resolves to
    // `<source>/beta/b.txt` — first component after stripping the
    // source prefix is `beta`, which the classifier flags as
    // `CrossSkill`. Using a relative target (not an absolute one)
    // ensures the cross-skill gate is what fires, not the absolute
    // target gate added by T2's tightening.
    let err = raw_errno(std::os::unix::fs::symlink("../../beta/b.txt", &link));
    assert_eq!(
        err,
        libc::EACCES,
        "cross-skill symlink must be rejected with EACCES, got {err}"
    );
    assert!(
        !link.exists() && std::fs::symlink_metadata(&link).is_err(),
        "rejected symlink must leave no on-mount entry"
    );
}

#[test]
fn test_outside_source_symlink_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    let link = fx.skill_path("alpha").join("escape");
    let err = raw_errno(std::os::unix::fs::symlink("/etc/passwd", &link));
    assert_eq!(
        err,
        libc::EACCES,
        "absolute outside-source symlink must be rejected with EACCES, got {err}"
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
}

#[test]
fn test_relative_escape_symlink_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    std::fs::create_dir(fx.skill_path("alpha").join("sub")).expect("mkdir sub");

    let link = fx.skill_path("alpha").join("sub").join("escape");
    // `../../../etc/passwd` from `<src>/alpha/sub/` lexically escapes
    // the source root entirely.
    let err = raw_errno(std::os::unix::fs::symlink("../../../etc/passwd", &link));
    assert_eq!(
        err,
        libc::EACCES,
        "relative escape symlink must be rejected with EACCES, got {err}"
    );
}

#[test]
fn test_symlink_into_skill_meta_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        // Seed `.skill-meta` on disk so the parent directory exists for
        // kernel-level lookup; the policy must still refuse the symlink
        // creation regardless.
        std::fs::create_dir_all(src.join("alpha").join(".skill-meta"))
            .expect("seed .skill-meta dir");
    });

    let link = fx
        .skill_path("alpha")
        .join(".skill-meta")
        .join("planted_link");
    let err = raw_errno(std::os::unix::fs::symlink("anywhere", &link));
    assert!(
        err == libc::ENOENT || err == libc::EACCES,
        ".skill-meta symlink must be rejected, got {err}"
    );
}

#[test]
fn test_symlink_target_into_skill_meta_rejected() {
    skip_if_no_fuse!();

    // The link path itself is OK (under an ordinary subdir), but the
    // **target** lexically resolves to the skill's `.skill-meta/**`.
    // Following the link from userspace would expose the protected
    // payload via an unprotected name, so T2 refuses with `EACCES`.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        std::fs::create_dir_all(src.join("alpha").join(".skill-meta"))
            .expect("seed .skill-meta dir");
    });
    std::fs::create_dir(fx.skill_path("alpha").join("sub")).expect("mkdir sub");

    let link = fx.skill_path("alpha").join("sub").join("leak");
    let err = raw_errno(std::os::unix::fs::symlink("../.skill-meta/secret", &link));
    assert_eq!(
        err,
        libc::EACCES,
        "same-skill symlink whose target lands in .skill-meta must be EACCES, got {err}"
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
}

#[test]
fn test_symlink_target_into_lifecycle_root_rejected() {
    skip_if_no_fuse!();

    // The link path itself is OK; the target points at a lifecycle
    // reserved root inside the same skill (e.g. `.staging/**`). Even
    // though `.staging` is a hidden, mutation-protected namespace, a
    // dangling link to it would survive after a future package
    // exposes the namespace, leaking pre-existing references. T2
    // rejects up-front with `EACCES`.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    std::fs::create_dir(fx.skill_path("alpha").join("sub")).expect("mkdir sub");

    let link = fx.skill_path("alpha").join("sub").join("staged_alias");
    let err = raw_errno(std::os::unix::fs::symlink("../.staging/payload", &link));
    assert_eq!(
        err,
        libc::EACCES,
        "same-skill symlink whose target lands in lifecycle root must be EACCES, got {err}"
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
}

#[test]
fn test_symlink_under_skill_discover_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|_src| {});
    // `skill-discover` is always virtually visible; attempts to create
    // a symlink under it must hit the virtual read-only rejection
    // (EROFS) before any classifier work.
    let link = fx.skill_path("skill-discover").join("link");
    let err = raw_errno(std::os::unix::fs::symlink("target", &link));
    assert_eq!(
        err,
        libc::EROFS,
        "skill-discover symlink must be EROFS, got {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Hardlink — allowed
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_same_skill_hardlink_allowed() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    write_passthrough(&fx, "alpha", "src.txt", b"shared");

    let src = fx.skill_path("alpha").join("src.txt");
    let dst = fx.skill_path("alpha").join("link.txt");
    std::fs::hard_link(&src, &dst).expect("hardlink same-skill regular file");

    // `nlink` comes from the underlying physical inode via the kernel's
    // `lstat`, so a freshly returned `dst` lookup must report at least
    // two links. We do not assert ino equality here — SkillFS allocates
    // per-path FUSE inodes, so the two names will receive distinct
    // `ino` values even though they share the same on-disk inode.
    let dst_meta = std::fs::symlink_metadata(&dst).expect("lstat dst");
    assert!(
        dst_meta.nlink() >= 2,
        "dst nlink must be ≥ 2 after hardlink, got {}",
        dst_meta.nlink()
    );
    assert_eq!(
        std::fs::read(&dst).expect("read dst"),
        b"shared",
        "linked file must observe same content"
    );

    // Writing through one name must surface through the other because both
    // names point at the same physical inode. Keep the replacement the same
    // length as the original payload: SkillFS currently allocates per-path
    // FUSE inodes for hardlinks, so this test should not depend on immediate
    // cross-path size-cache invalidation.
    std::fs::write(&dst, b"update").expect("write through link");
    let src_after = std::fs::read(&src).expect("read src after update via link");
    assert_eq!(
        src_after, b"update",
        "writes through dst must surface at src (shared inode)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Hardlink — rejected
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_cross_skill_hardlink_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        create_skill_dir(src, "beta");
    });
    write_passthrough(&fx, "alpha", "src.txt", b"x");

    let src = fx.skill_path("alpha").join("src.txt");
    let dst = fx.skill_path("beta").join("dst.txt");
    let err = raw_errno(std::fs::hard_link(&src, &dst));
    assert_eq!(
        err,
        libc::EACCES,
        "cross-skill hardlink must be rejected with EACCES, got {err}"
    );
    assert!(
        std::fs::symlink_metadata(&dst).is_err(),
        "rejected hardlink must not appear on disk"
    );

    let src_meta = std::fs::symlink_metadata(&src).expect("lstat src after reject");
    assert_eq!(
        src_meta.nlink(),
        1,
        "source nlink must stay at 1 after a rejected cross-skill link"
    );
}

#[test]
fn test_hardlink_into_skill_meta_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        std::fs::create_dir_all(src.join("alpha").join(".skill-meta"))
            .expect("seed .skill-meta dir");
    });
    write_passthrough(&fx, "alpha", "src.txt", b"x");

    let src = fx.skill_path("alpha").join("src.txt");
    let dst = fx.skill_path("alpha").join(".skill-meta").join("planted");
    let err = raw_errno(std::fs::hard_link(&src, &dst));
    assert!(
        err == libc::ENOENT || err == libc::EACCES,
        ".skill-meta hardlink must be rejected, got {err}"
    );
}

#[test]
fn test_hardlink_symlink_source_rejected_as_non_regular() {
    skip_if_no_fuse!();

    // T2 hardlink scope is same-skill ordinary regular files only.
    // A symlink source must NOT be silently followed (which would
    // pin a hidden inode behind a stable name). Refuse with `EPERM`
    // and a `class=non_regular_source` audit detail.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    write_passthrough(&fx, "alpha", "real.txt", b"r");

    // Create a same-skill symlink first — this is the allowed T2
    // path, then we attempt to hardlink off of that symlink itself.
    let symlink_path = fx.skill_path("alpha").join("alias");
    std::os::unix::fs::symlink("real.txt", &symlink_path).expect("seed same-skill symlink");

    let dst = fx.skill_path("alpha").join("hardlinked_alias");
    let err = raw_errno(std::fs::hard_link(&symlink_path, &dst));
    assert_eq!(
        err,
        libc::EPERM,
        "hardlink source = symlink must be rejected with EPERM, got {err}"
    );
    assert!(
        std::fs::symlink_metadata(&dst).is_err(),
        "rejected non-regular hardlink must leave no on-mount entry"
    );
}

#[test]
fn test_hardlink_fifo_source_rejected_as_non_regular() {
    skip_if_no_fuse!();

    // FIFO creation is part of T2 (`mknod` accepts `S_IFIFO`); but a
    // FIFO is not a regular file, so hardlinking off of one must fail
    // with `EPERM` and `class=non_regular_source` regardless of the
    // physical kernel behavior.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    let fifo = fx.skill_path("alpha").join("pipe");
    let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("CString for fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo must succeed (T2 surface)");

    let dst = fx.skill_path("alpha").join("pipe_link");
    let err = raw_errno(std::fs::hard_link(&fifo, &dst));
    assert_eq!(
        err,
        libc::EPERM,
        "hardlink source = FIFO must be rejected with EPERM, got {err}"
    );
    assert!(std::fs::symlink_metadata(&dst).is_err());
}

#[test]
fn test_hardlink_directory_source_still_rejected() {
    skip_if_no_fuse!();

    // The pre-existing directory rejection now flows through the
    // `class=non_regular_source` branch instead of a dedicated
    // directory branch; behaviorally the kernel still sees `EPERM`.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    let dir = fx.skill_path("alpha").join("subdir");
    std::fs::create_dir(&dir).expect("mkdir subdir");

    let dst = fx.skill_path("alpha").join("subdir_link");
    let err = raw_errno(std::fs::hard_link(&dir, &dst));
    assert_eq!(
        err,
        libc::EPERM,
        "hardlink source = directory must be rejected with EPERM, got {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// FIFO (mknod) — allowed
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fifo_creation_allowed_and_reported_correctly() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    let fifo = fx.skill_path("alpha").join("pipe");
    let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("CString for fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(
        rc,
        0,
        "mkfifo on ordinary passthrough must succeed, errno={}",
        std::io::Error::last_os_error()
    );

    let meta = std::fs::symlink_metadata(&fifo).expect("lstat fifo");
    assert!(
        meta.file_type().is_fifo(),
        "kernel must report the new entry as a FIFO, got {:?}",
        meta.file_type()
    );
    // Mode bits should land at 0o644 once the kernel's umask has been
    // applied (the daemon's own umask was neutralized at mount).
    let umask = unsafe {
        let m = libc::umask(0);
        libc::umask(m);
        m
    };
    let expected_perm: u32 = 0o644 & !(umask as u32) & 0o7777;
    assert_eq!(
        meta.mode() & 0o7777,
        expected_perm,
        "FIFO permission bits should be 0o644 minus the caller umask"
    );

    let entries = list_dir_names(&fx.skill_path("alpha"));
    assert!(entries.iter().any(|n| n == "pipe"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Device mknod — rejected
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_char_device_mknod_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    let node = fx.skill_path("alpha").join("zero_clone");
    let c_path = CString::new(node.as_os_str().as_bytes()).expect("CString");
    // /dev/zero major=1 minor=5 → makedev(1, 5).
    let dev = libc::makedev(1, 5);
    let rc = unsafe { libc::mknod(c_path.as_ptr(), libc::S_IFCHR | 0o644, dev) };
    assert_eq!(rc, -1, "char-device mknod must fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(
        err,
        libc::EPERM,
        "char-device mknod must be rejected with EPERM, got {err}"
    );
    assert!(std::fs::symlink_metadata(&node).is_err());
}

#[test]
fn test_block_device_mknod_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    let node = fx.skill_path("alpha").join("loop_clone");
    let c_path = CString::new(node.as_os_str().as_bytes()).expect("CString");
    let dev = libc::makedev(7, 0);
    let rc = unsafe { libc::mknod(c_path.as_ptr(), libc::S_IFBLK | 0o644, dev) };
    assert_eq!(rc, -1, "block-device mknod must fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(
        err,
        libc::EPERM,
        "block-device mknod must be rejected with EPERM, got {err}"
    );
}

#[test]
fn test_socket_mknod_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    let node = fx.skill_path("alpha").join("sock");
    let c_path = CString::new(node.as_os_str().as_bytes()).expect("CString");
    let rc = unsafe { libc::mknod(c_path.as_ptr(), libc::S_IFSOCK | 0o644, 0) };
    assert_eq!(rc, -1, "socket mknod must fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(
        err,
        libc::EPERM,
        "socket mknod must be rejected with EPERM, got {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// skill-discover remains read-only for hardlink/FIFO too
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_hardlink_under_skill_discover_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    write_passthrough(&fx, "alpha", "src.txt", b"x");

    let src = fx.skill_path("alpha").join("src.txt");
    let dst = fx.skill_path("skill-discover").join("linked.txt");
    let err = raw_errno(std::fs::hard_link(&src, &dst));
    assert!(
        err == libc::EROFS || err == libc::EACCES,
        "skill-discover hardlink must be denied (EROFS or EACCES), got {err}"
    );
}

#[test]
fn test_fifo_under_skill_discover_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|_src| {});

    let fifo = fx.skill_path("skill-discover").join("pipe");
    let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("CString");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(rc, -1, "mkfifo under skill-discover must fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert!(
        err == libc::EROFS || err == libc::EACCES,
        "skill-discover mkfifo must be denied (EROFS or EACCES), got {err}"
    );
}
