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

#[test]
fn test_hardlink_alias_rename_is_a_noop() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    write_passthrough(&fx, "alpha", "src.txt", b"shared");

    let src = fx.skill_path("alpha").join("src.txt");
    let dst = fx.skill_path("alpha").join("link.txt");
    std::fs::hard_link(&src, &dst).expect("hardlink same-skill regular file");

    // Both names get distinct per-path FUSE inodes (SkillFS maps paths to
    // inodes even for hard links); pin them before the rename.
    let src_ino = std::fs::symlink_metadata(&src).expect("lstat src").ino();
    let dst_ino = std::fs::symlink_metadata(&dst).expect("lstat dst").ino();
    assert_ne!(src_ino, dst_ino, "per-path FUSE inodes must differ");

    // POSIX: rename(a, b) where a and b are links to the same object is a
    // successful no-op — both directory entries survive. The kernel's VFS
    // does not short-circuit it here (the two names carry distinct
    // per-path FUSE inodes), so the rename reaches the daemon, whose
    // same-object recognition (`backing_object_identity` in mutate.rs)
    // must keep the inode map untouched.
    std::fs::rename(&src, &dst).expect("rename onto same-object hardlink");

    // The kernel moved `src`'s dentry onto `dst`'s name and keeps its
    // cached attributes (1s validity) until revalidation; wait that out
    // so the re-stats below answer from fresh LOOKUPs — i.e. from the
    // daemon's inode map, which is the contract under test.
    std::thread::sleep(std::time::Duration::from_millis(1500));

    for path in [&src, &dst] {
        assert!(
            std::fs::symlink_metadata(path).is_ok(),
            "both names must survive the same-object rename: {}",
            path.display()
        );
    }
    // `src`'s mapping was untouched by the no-op, so a fresh LOOKUP
    // returns the same FUSE inode. (`dst`'s userspace inode number
    // legitimately changes: the kernel replaced the target dentry,
    // FORGOT the old per-path inode, and the next LOOKUP allocates a
    // fresh one. What must NOT happen is `dst` adopting `src`'s
    // identity — exactly what the pre-fix inode surgery produced by
    // re-pointing the moved subtree onto the replaced name.)
    assert_eq!(
        std::fs::symlink_metadata(&src)
            .expect("lstat src after")
            .ino(),
        src_ino,
        "src must keep its inode across a same-object rename"
    );
    assert_ne!(
        std::fs::symlink_metadata(&dst)
            .expect("lstat dst after")
            .ino(),
        src_ino,
        "dst must not adopt src's inode across a same-object rename"
    );
    // And both names keep serving the shared content, including after one
    // link is removed.
    assert_eq!(std::fs::read(&src).expect("read src"), b"shared");
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"shared");

    // The same no-op in the other direction. Pin dst's CURRENT inode
    // first: after the first rename it is a freshly allocated per-path
    // inode (the original one was FORGOT when the kernel replaced the
    // target dentry), and it is now the SOURCE side of the reverse
    // rename, whose mapping the no-op must leave untouched.
    let dst_ino_now = std::fs::symlink_metadata(&dst)
        .expect("lstat dst before reverse")
        .ino();
    assert_ne!(dst_ino_now, src_ino);
    std::fs::rename(&dst, &src).expect("rename back onto same object");
    // Wait out the moved dentry's attribute validity again so the
    // re-stats answer from fresh LOOKUPs.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(
        std::fs::symlink_metadata(&dst)
            .expect("lstat dst after back")
            .ino(),
        dst_ino_now,
        "dst must keep its inode across the reverse same-object rename"
    );
    assert_ne!(
        std::fs::symlink_metadata(&src)
            .expect("lstat src after back")
            .ino(),
        dst_ino_now,
        "src must not adopt dst's inode across the reverse same-object rename"
    );

    std::fs::remove_file(&src).expect("unlink src");
    assert_eq!(
        std::fs::read(&dst).expect("read dst after unlink"),
        b"shared"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Same-object rename whose physical paths exceed PATH_MAX
// ─────────────────────────────────────────────────────────────────────────────

/// Open `root` as a directory and descend `components` one `openat` at a
/// time. Passing a single component per syscall is how the kernel itself
/// traverses trees whose total path length exceeds PATH_MAX.
#[cfg(target_os = "linux")]
fn openat_walk_dir(root: &Path, components: &[&std::ffi::OsStr]) -> std::fs::File {
    use std::os::unix::io::FromRawFd;

    let root_c = CString::new(root.as_os_str().as_bytes()).expect("root cstring");
    let mut fd = unsafe {
        libc::open(
            root_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    assert!(fd >= 0, "open {}", root.display());
    for comp in components {
        let c = CString::new(comp.as_bytes()).expect("component cstring");
        let next = unsafe {
            libc::openat(
                fd,
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(next >= 0, "openat component of length {}", comp.len());
        unsafe { libc::close(fd) };
        fd = next;
    }
    unsafe { std::fs::File::from_raw_fd(fd) }
}

/// `fstatat(dir, leaf, AT_SYMLINK_NOFOLLOW)` — stat one leaf through an
/// already-open directory fd.
#[cfg(target_os = "linux")]
fn fstatat_through(dir: &std::fs::File, leaf: &str) -> libc::stat {
    use std::os::unix::io::AsRawFd;

    let c = CString::new(leaf).expect("leaf cstring");
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(rc, 0, "fstatat leaf of length {}", leaf.len());
    st
}

#[test]
#[cfg(target_os = "linux")]
fn test_hardlink_alias_rename_beyond_path_max_is_a_noop() {
    use std::io::{Read, Write};
    use std::os::unix::io::{AsRawFd, FromRawFd};

    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    // Build a deep chain inside the physical skill dir whose deepest
    // directory stays below PATH_MAX (so every parent remains openable)
    // while the leaf names push the absolute file paths past it — the
    // shape where plain lstat and the absolute-path rename both fail
    // with ENAMETOOLONG but the dirfd rename succeeds.
    let mut deep = fx.source_skill_path("alpha");
    let mut chain: Vec<std::ffi::OsString> = Vec::new();
    loop {
        let component = "d".repeat(200);
        let next = deep.join(&component);
        // Descend as deep as the directory path itself allows; the
        // ~240-char leaf names then push the file paths past PATH_MAX.
        if next.as_os_str().len() >= libc::PATH_MAX as usize {
            break;
        }
        std::fs::create_dir(&next).expect("deep mkdir");
        chain.push(component.into());
        deep = next;
    }
    let leaf_a = "s".repeat(240);
    let leaf_b = "l".repeat(240);
    assert!(
        deep.join(&leaf_a).as_os_str().len() >= libc::PATH_MAX as usize,
        "the physical leaf paths must exceed PATH_MAX"
    );
    assert!(
        deep.as_os_str().len() < libc::PATH_MAX as usize,
        "the deepest physical directory must stay openable"
    );

    // Create the file and its hard-link alias on the source side through
    // the parent dir fd — the absolute paths cannot be used at this depth.
    let src_dir = openat_walk_dir(
        &fx.source_skill_path("alpha"),
        &chain.iter().map(|c| c.as_os_str()).collect::<Vec<_>>(),
    );
    {
        let ca = CString::new(leaf_a.as_bytes()).expect("leaf a cstring");
        let fd = unsafe {
            libc::openat(
                src_dir.as_raw_fd(),
                ca.as_ptr(),
                libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | libc::O_CLOEXEC,
                0o644,
            )
        };
        assert!(fd >= 0, "create leaf a via dirfd");
        let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
        f.write_all(b"shared").expect("write leaf a");
    }
    {
        let ca = CString::new(leaf_a.as_bytes()).expect("leaf a cstring");
        let cb = CString::new(leaf_b.as_bytes()).expect("leaf b cstring");
        let rc = unsafe {
            libc::linkat(
                src_dir.as_raw_fd(),
                ca.as_ptr(),
                src_dir.as_raw_fd(),
                cb.as_ptr(),
                0,
            )
        };
        assert_eq!(rc, 0, "linkat through the dirfd must succeed");
    }

    // Walk the same chain through the MOUNT, one component per openat;
    // each step is a FUSE lookup, so the deepest lookups exercise the
    // daemon's own long-path fallback.
    let mut mount_components: Vec<std::ffi::OsString> = vec!["skills".into(), "alpha".into()];
    mount_components.extend(chain.iter().cloned());
    let mount_dir = openat_walk_dir(
        fx.mountpoint(),
        &mount_components
            .iter()
            .map(|c| c.as_os_str())
            .collect::<Vec<_>>(),
    );

    // Pin the per-path FUSE inodes of both aliases.
    let ino_a = fstatat_through(&mount_dir, &leaf_a).st_ino;
    let ino_b = fstatat_through(&mount_dir, &leaf_b).st_ino;
    assert_ne!(ino_a, ino_b, "per-path FUSE inodes must differ");

    // rename(a, b) through the mount. The kernel forwards it (distinct
    // per-path FUSE inodes, so no VFS same-inode short-circuit), the
    // daemon's physical paths exceed PATH_MAX, and the physical rename
    // succeeds through its dirfd fallback as a POSIX no-op. The identity
    // probe must recognize the same-object pair through its own dirfd
    // fallback, or the inode surgery evicts the still-live alias.
    {
        let ca = CString::new(leaf_a.as_bytes()).expect("leaf a cstring");
        let cb = CString::new(leaf_b.as_bytes()).expect("leaf b cstring");
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                mount_dir.as_raw_fd(),
                ca.as_ptr(),
                mount_dir.as_raw_fd(),
                cb.as_ptr(),
                0,
            )
        };
        assert_eq!(rc, 0, "renameat2 through the mount must succeed");
    }

    // The kernel moved `leaf_a`'s dentry onto `leaf_b`'s name and keeps
    // its cached attributes (1s validity) until revalidation; wait that
    // out so the re-stats below answer from fresh LOOKUPs — i.e. from
    // the daemon's inode map, which is the contract under test.
    std::thread::sleep(std::time::Duration::from_millis(1500));

    // Both aliases survive with stable identities: `src`'s mapping was
    // untouched by the no-op (a fresh LOOKUP returns the same FUSE
    // inode), and `dst` never adopts `src`'s identity — the pre-fix
    // inode surgery re-pointed the moved subtree onto the replaced name,
    // which is exactly what must not happen. (`dst`'s userspace inode
    // number itself may change: the kernel replaced the target dentry
    // and FORGOT the old per-path inode, so the next LOOKUP can allocate
    // a fresh one.)
    assert_eq!(
        fstatat_through(&mount_dir, &leaf_a).st_ino,
        ino_a,
        "src must keep its inode across a same-object rename beyond PATH_MAX"
    );
    assert_ne!(
        fstatat_through(&mount_dir, &leaf_b).st_ino,
        ino_a,
        "dst must not adopt src's inode across a same-object rename beyond PATH_MAX"
    );

    // And the shared content is still served through the mount.
    {
        let cb = CString::new(leaf_b.as_bytes()).expect("leaf b cstring");
        let fd = unsafe {
            libc::openat(
                mount_dir.as_raw_fd(),
                cb.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        assert!(fd >= 0, "open leaf b through the mount");
        let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read leaf b");
        assert_eq!(buf, b"shared");
    }
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
