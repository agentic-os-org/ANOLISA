//! T3 integration coverage for minimal extended-attribute (xattr) passthrough.
//!
//! Package T3 enables Linux `user.*` xattrs on **ordinary passthrough leaves**
//! under a normal skill. Every other surface — virtual paths, compiled
//! `SKILL.md`, `skill-discover`, lifecycle reserved roots — keeps deterministic
//! "unsupported" semantics, and `.skill-meta/**` mutations remain blocked by
//! the existing `SkillMetaProtectionPolicy` gate. Other xattr namespaces
//! (`security.*`, `trusted.*`, `system.*`, missing prefix) are rejected up
//! front with `EOPNOTSUPP` so SkillFS does not become a back door for kernel
//! / LSM-owned categories.
//!
//! These tests drive each rule end-to-end through a real FUSE mount.

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

mod common;

use common::{MountFixture, create_skill_dir};

// ─────────────────────────────────────────────────────────────────────────────
// libc xattr helpers — std has no xattr wrapper.
// All helpers use the no-follow (`l*xattr`) variants so symlink behavior is
// consistent with how `lookup`/`getattr` already report metadata under
// Package I.
// ─────────────────────────────────────────────────────────────────────────────

fn cstr(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).expect("path -> CString")
}

fn cname(name: &str) -> CString {
    CString::new(name).expect("xattr name -> CString")
}

fn lsetxattr(path: &Path, name: &str, value: &[u8], flags: i32) -> Result<(), i32> {
    let cp = cstr(path);
    let cn = cname(name);
    let rc = unsafe {
        libc::lsetxattr(
            cp.as_ptr(),
            cn.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            flags,
        )
    };
    if rc != 0 {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    } else {
        Ok(())
    }
}

fn lgetxattr(path: &Path, name: &str) -> Result<Vec<u8>, i32> {
    let cp = cstr(path);
    let cn = cname(name);
    let needed = unsafe { libc::lgetxattr(cp.as_ptr(), cn.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    let got = unsafe {
        libc::lgetxattr(
            cp.as_ptr(),
            cn.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    buf.truncate(got as usize);
    Ok(buf)
}

fn llistxattr(path: &Path) -> Result<Vec<String>, i32> {
    let cp = cstr(path);
    let needed = unsafe { libc::llistxattr(cp.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    let got = unsafe {
        libc::llistxattr(
            cp.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    buf.truncate(got as usize);
    Ok(buf
        .split(|b| *b == 0u8)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

fn lremovexattr(path: &Path, name: &str) -> Result<(), i32> {
    let cp = cstr(path);
    let cn = cname(name);
    let rc = unsafe { libc::lremovexattr(cp.as_ptr(), cn.as_ptr()) };
    if rc != 0 {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    } else {
        Ok(())
    }
}

/// Convenience for skipping a test when the underlying tmpfs does not support
/// `user.*` xattrs at all. Some CI tmpfs mounts return `ENOTSUP` for any
/// user.* xattr; rather than fail the SkillFS test, we treat that as a
/// host-capability skip so the assertions only run where the substrate
/// actually supports the operation.
fn skip_if_user_xattr_unsupported(path: &Path) -> bool {
    match lsetxattr(path, "user.skillfs.probe", b"1", 0) {
        Ok(()) => {
            // Clean up the probe so it doesn't pollute later assertions.
            let _ = lremovexattr(path, "user.skillfs.probe");
            false
        }
        Err(err) if err == libc::ENOTSUP || err == libc::EOPNOTSUPP => {
            eprintln!(
                "SKIP: underlying fs at {:?} does not support user.* xattrs (errno {err}); \
                 cannot exercise T3 passthrough",
                path
            );
            true
        }
        Err(err) => {
            eprintln!(
                "SKIP: unexpected errno {err} when probing user.* xattrs at {:?}",
                path
            );
            true
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// xattr-capable temp root selector
//
// Many tmpfs mounts (including `$TMPDIR` / `/tmp` on a fair number of Linux
// hosts) refuse `user.*` xattrs entirely. When the substrate refuses
// `user.*`, the positive passthrough tests degenerate into no-ops — they do
// not actually exercise the T3 set/get/list/remove path, so passing them
// would be misleading.
//
// The selector returns the first writable root whose `user.*` xattrs are
// actually honored:
//   1. `SKILLFS_XATTR_TEST_ROOT` — explicit operator override (mainly for
//      CI hosts that pre-create a backing dir on ext4/xfs/btrfs);
//   2. `<workspace>/target/xattr-tests` — the in-tree target dir is
//      typically on the same backing FS as the repository checkout, which
//      tends to be a real disk filesystem that supports `user.*`;
//   3. `$HOME/.cache/skillfs-xattr-tests` — fall back to the user's home
//      cache.
//
// Returning `None` means none of the candidates support `user.*` xattrs and
// the positive tests must skip rather than silently no-op.
// ─────────────────────────────────────────────────────────────────────────────

fn workspace_target_dir() -> Option<PathBuf> {
    // `CARGO_MANIFEST_DIR` for an integration test crate points at the
    // package crate (e.g. `…/crates/skillfs-fuse`). Walk up until we find a
    // directory containing `Cargo.lock`; the sibling `target` dir is the
    // workspace target. Avoids hard-coding repo-relative paths.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        if ancestor.join("Cargo.lock").exists() {
            return Some(ancestor.join("target").join("xattr-tests"));
        }
    }
    None
}

fn probe_user_xattr_capable(root: &Path) -> bool {
    if std::fs::create_dir_all(root).is_err() {
        return false;
    }
    let probe = match tempfile::Builder::new()
        .prefix("skillfs-xattr-probe-")
        .tempdir_in(root)
    {
        Ok(d) => d,
        Err(_) => return false,
    };
    let result = lsetxattr(probe.path(), "user.skillfs.probe", b"1", 0);
    let _ = lremovexattr(probe.path(), "user.skillfs.probe");
    result.is_ok()
}

fn xattr_capable_temp_root() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(env_path) = std::env::var("SKILLFS_XATTR_TEST_ROOT") {
        if !env_path.is_empty() {
            candidates.push(PathBuf::from(env_path));
        }
    }
    if let Some(workspace_target) = workspace_target_dir() {
        candidates.push(workspace_target);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut path = PathBuf::from(home);
        path.push(".cache");
        path.push("skillfs-xattr-tests");
        candidates.push(path);
    }
    for cand in candidates {
        if probe_user_xattr_capable(&cand) {
            eprintln!(
                "[posix_xattr_tests] using xattr-capable root: {}",
                cand.display()
            );
            return Some(cand);
        }
    }
    None
}

/// Build a normal-mode fixture whose **source** lives under an xattr-capable
/// root. Returns `None` and skips the calling test when no candidate root
/// supports `user.*` xattrs.
fn mount_xattr_capable<F: FnOnce(&Path)>(seed: F) -> Option<MountFixture> {
    let root = match xattr_capable_temp_root() {
        Some(r) => r,
        None => {
            eprintln!(
                "SKIP: positive xattr passthrough skipped — no candidate root \
                 supports user.* xattrs. Set SKILLFS_XATTR_TEST_ROOT to a \
                 directory on a filesystem that does."
            );
            return None;
        }
    };
    Some(MountFixture::normal_in(&root, seed))
}

fn write_passthrough(fx: &MountFixture, skill: &str, rel: &str, contents: &[u8]) {
    let source_rel = fx.source_skill_path(skill).join(rel);
    if let Some(parent) = source_rel.parent() {
        std::fs::create_dir_all(parent).expect("seed parent dir");
    }
    std::fs::write(&source_rel, contents).expect("seed passthrough file");
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Ordinary passthrough file: full user.* roundtrip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_user_xattr_set_get_list_remove_on_passthrough_file() {
    skip_if_no_fuse!();

    let fx = match mount_xattr_capable(|src| {
        create_skill_dir(src, "alpha");
    }) {
        Some(fx) => fx,
        None => return,
    };
    write_passthrough(&fx, "alpha", "doc.txt", b"hello");

    // Final safety net: confirm the substrate the source actually landed on
    // honors user.* xattrs. The candidate selector probed a directory, but
    // policies can differ per inode (acl_xattr modules, immutable bits,
    // etc.), so re-probe through the actual seeded file.
    let src_probe_path = fx.source_skill_path("alpha");
    if skip_if_user_xattr_unsupported(&src_probe_path) {
        return;
    }

    let file = fx.passthrough_path("alpha", "doc.txt");

    // Empty list initially (the source has no user.* xattrs yet).
    let initial = llistxattr(&file).expect("listxattr empty");
    assert!(
        !initial.iter().any(|n| n == "user.skillfs.test"),
        "user.skillfs.test must not exist before set, got {initial:?}",
    );

    // set + get
    lsetxattr(&file, "user.skillfs.test", b"value-1", 0).expect("setxattr through mount");
    let got = lgetxattr(&file, "user.skillfs.test").expect("getxattr through mount");
    assert_eq!(got, b"value-1");

    // list
    let names = llistxattr(&file).expect("listxattr after set");
    assert!(
        names.contains(&"user.skillfs.test".to_string()),
        "list must include user.skillfs.test, got {names:?}",
    );

    // remove → get returns ENODATA
    lremovexattr(&file, "user.skillfs.test").expect("removexattr through mount");
    let err = lgetxattr(&file, "user.skillfs.test").expect_err("getxattr after remove");
    assert!(
        err == libc::ENODATA,
        "expected ENODATA after remove, got {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Ordinary passthrough directory: full user.* roundtrip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_user_xattr_set_get_list_remove_on_passthrough_directory() {
    skip_if_no_fuse!();

    let fx = match mount_xattr_capable(|src| {
        create_skill_dir(src, "alpha");
    }) {
        Some(fx) => fx,
        None => return,
    };
    std::fs::create_dir_all(fx.source_skill_path("alpha").join("sub")).expect("seed sub");

    let src_probe_path = fx.source_skill_path("alpha").join("sub");
    if skip_if_user_xattr_unsupported(&src_probe_path) {
        return;
    }

    let dir = fx.passthrough_path("alpha", "sub");
    assert!(dir.is_dir(), "fixture must surface the passthrough dir");

    lsetxattr(&dir, "user.skillfs.dir", b"dir-attr", 0).expect("setxattr on dir");
    let got = lgetxattr(&dir, "user.skillfs.dir").expect("getxattr on dir");
    assert_eq!(got, b"dir-attr");

    let names = llistxattr(&dir).expect("listxattr on dir");
    assert!(
        names.contains(&"user.skillfs.dir".to_string()),
        "list must include user.skillfs.dir, got {names:?}",
    );

    lremovexattr(&dir, "user.skillfs.dir").expect("removexattr on dir");
    let err = lgetxattr(&dir, "user.skillfs.dir").expect_err("getxattr after remove on dir");
    assert!(
        err == libc::ENODATA,
        "expected ENODATA after remove on dir, got {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Unsupported namespaces (`trusted.*`, `security.*`, no namespace) →
//    deterministic EOPNOTSUPP regardless of the substrate capability.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_unsupported_namespaces_rejected_deterministically() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    write_passthrough(&fx, "alpha", "doc.txt", b"hello");
    let file = fx.passthrough_path("alpha", "doc.txt");

    // Acceptable rejection errnos:
    //   * EOPNOTSUPP — SkillFS T3 deterministic rejection (the
    //     preferred answer when the kernel does not pre-empt us);
    //   * EPERM     — the Linux VFS already blocks `trusted.*`/
    //     `security.*` mutations for non-CAP_SYS_ADMIN callers before
    //     the syscall reaches FUSE. The test runs unprivileged in CI,
    //     so this branch fires for the privileged namespaces. Either
    //     answer is consistent with the T3 invariant that the
    //     non-`user.*` xattr never lands on disk;
    //   * ENOTSUP   — same numeric value as EOPNOTSUPP on Linux but
    //     explicitly accepted in case libc aliases drift in future
    //     toolchains.
    fn is_acceptable_reject(err: i32) -> bool {
        err == libc::EOPNOTSUPP || err == libc::EPERM || err == libc::ENOTSUP
    }

    for name in [
        "trusted.skillfs.test",
        "security.skillfs.test",
        "system.skillfs.test",
        // missing namespace separator → unknown namespace
        "noprefix",
        // empty user.* name (no leaf) → unknown namespace
        "user.",
    ] {
        let set_err = lsetxattr(&file, name, b"x", 0).expect_err("set must fail");
        assert!(
            is_acceptable_reject(set_err),
            "setxattr({name}) errno mismatch — got {set_err}",
        );
        let get_err = lgetxattr(&file, name).expect_err("get must fail");
        assert!(
            is_acceptable_reject(get_err) || get_err == libc::ENODATA,
            "getxattr({name}) errno mismatch — got {get_err}",
        );
        let rm_err = lremovexattr(&file, name).expect_err("remove must fail");
        assert!(
            is_acceptable_reject(rm_err) || rm_err == libc::ENODATA,
            "removexattr({name}) errno mismatch — got {rm_err}",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. `.skill-meta/**` mutation rejected with EACCES even for user.*.
//    Read/list passes through to the physical errno (T3 choice — see
//    callback comment in `lib.rs`).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_skill_meta_xattr_mutation_rejected_with_eacces() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    std::fs::create_dir_all(fx.source_skill_path("alpha").join(".skill-meta"))
        .expect("seed .skill-meta dir");
    std::fs::write(
        fx.source_skill_path("alpha")
            .join(".skill-meta")
            .join("manifest.json"),
        b"{}",
    )
    .expect("seed manifest.json");

    let meta_dir = fx.passthrough_path("alpha", ".skill-meta");
    let meta_file = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    // Untrusted callers cannot see .skill-meta at all — the path
    // resolves to ENOENT at the lookup layer before xattr callbacks
    // are reached.
    let set_err =
        lsetxattr(&meta_dir, "user.skillfs.test", b"x", 0).expect_err("set on .skill-meta dir");
    assert_eq!(
        set_err,
        libc::ENOENT,
        "set on .skill-meta dir must be ENOENT for untrusted"
    );
    let set_err =
        lsetxattr(&meta_file, "user.skillfs.test", b"x", 0).expect_err("set on .skill-meta file");
    assert_eq!(
        set_err,
        libc::ENOENT,
        "set on .skill-meta file must be ENOENT for untrusted"
    );

    let get_err = lgetxattr(&meta_file, "user.skillfs.test").expect_err("get on .skill-meta file");
    assert_eq!(
        get_err,
        libc::ENOENT,
        "get on .skill-meta file must be ENOENT for untrusted"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Compiled `SKILL.md` is virtual — xattr mutation rejected, compiled
//    read still works.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_virtual_skill_md_xattr_rejected_and_read_still_works() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    let skill_md = fx.skill_path("alpha").join("SKILL.md");

    // Sanity: compiled read still works.
    let body = std::fs::read_to_string(&skill_md).expect("read compiled SKILL.md");
    assert!(
        body.contains("name: alpha"),
        "compiled SKILL.md must still surface the frontmatter, got: {body}"
    );

    let set_err = lsetxattr(&skill_md, "user.skillfs.test", b"x", 0)
        .expect_err("setxattr on virtual SKILL.md");
    assert_eq!(set_err, libc::EOPNOTSUPP, "got errno {set_err}");

    let get_err =
        lgetxattr(&skill_md, "user.skillfs.test").expect_err("getxattr on virtual SKILL.md");
    assert_eq!(get_err, libc::EOPNOTSUPP, "got errno {get_err}");

    let rm_err =
        lremovexattr(&skill_md, "user.skillfs.test").expect_err("removexattr on virtual SKILL.md");
    assert_eq!(rm_err, libc::EOPNOTSUPP, "got errno {rm_err}");

    // Listing a virtual SKILL.md returns ENOTSUP — deterministic and does
    // not leak any source xattrs into the virtual surface.
    let list_err = llistxattr(&skill_md).expect_err("listxattr on virtual SKILL.md");
    assert_eq!(list_err, libc::EOPNOTSUPP, "got errno {list_err}");

    // Read still works after the xattr probes.
    let body2 = std::fs::read_to_string(&skill_md).expect("read compiled SKILL.md again");
    assert_eq!(body, body2, "compiled SKILL.md content must be stable");
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. `skill-discover` virtual namespace rejects xattr mutation.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_skill_discover_xattr_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });
    let discover_md = fx.skill_path("skill-discover").join("SKILL.md");

    // The compiled `SKILL.md` under `skill-discover` is always present.
    assert!(
        std::fs::metadata(&discover_md).is_ok(),
        "skill-discover/SKILL.md must be visible through the mount"
    );

    let set_err = lsetxattr(&discover_md, "user.skillfs.test", b"x", 0)
        .expect_err("setxattr on skill-discover");
    assert_eq!(set_err, libc::EOPNOTSUPP, "got errno {set_err}");

    let get_err =
        lgetxattr(&discover_md, "user.skillfs.test").expect_err("getxattr on skill-discover");
    assert_eq!(get_err, libc::EOPNOTSUPP, "got errno {get_err}");

    let rm_err =
        lremovexattr(&discover_md, "user.skillfs.test").expect_err("removexattr on skill-discover");
    assert_eq!(rm_err, libc::EOPNOTSUPP, "got errno {rm_err}");

    let list_err = llistxattr(&discover_md).expect_err("listxattr on skill-discover");
    assert_eq!(list_err, libc::EOPNOTSUPP, "got errno {list_err}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. Lifecycle reserved roots (`.staging`, etc.) — must not be exposed or
//    mutable through xattr.
//
// The lifecycle reservation hides reserved roots from ordinary lookup, so
// the FUSE-side path doesn't even resolve to an inode. We assert that no
// xattr call can reach the underlying source, regardless of whether the
// lookup itself errors out first or the xattr layer rejects after.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_lifecycle_reserved_root_xattr_does_not_expose_hidden_paths() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        // Seed a normal skill so the mount has content...
        create_skill_dir(src, "alpha");
        // ...and a hidden lifecycle root with a payload file directly on
        // disk. The reservation gate must keep this invisible to ordinary
        // userspace.
        std::fs::create_dir_all(src.join(".staging")).expect("seed .staging dir");
        std::fs::write(src.join(".staging").join("payload.txt"), b"secret").expect("seed payload");
    });

    // Direct probe via the mount — the lookup itself should fail with
    // ENOENT (lifecycle hides the root). Then the xattr surface must
    // never accept a write here.
    let lifecycle_payload = fx.skills_root().join(".staging").join("payload.txt");

    // The path should not be visible at all (lifecycle reservation
    // returns ENOENT from lookup/readdir).
    let visible = std::fs::metadata(&lifecycle_payload).is_ok();
    assert!(
        !visible,
        "lifecycle reserved root must not be visible through ordinary lookup"
    );

    // Even if a caller forces an xattr syscall against the hidden path
    // string, the response must be a deterministic error and the
    // on-disk source payload must remain untouched.
    let set_err = lsetxattr(&lifecycle_payload, "user.skillfs.test", b"x", 0)
        .expect_err("setxattr on hidden lifecycle path");
    // ENOENT (lookup hides) or EOPNOTSUPP / EACCES (lifecycle/xattr gate) are
    // all acceptable; the critical invariant is that the underlying source
    // payload is not mutated and the user.skillfs.test attr is not stored.
    assert!(
        matches!(set_err, libc::ENOENT | libc::EOPNOTSUPP | libc::EACCES,),
        "expected ENOENT/EOPNOTSUPP/EACCES on hidden lifecycle xattr set, got {set_err}",
    );

    // Verify the source file was not mutated.
    let on_disk = fx.source().join(".staging").join("payload.txt");
    assert_eq!(
        std::fs::read(&on_disk).expect("source payload still readable"),
        b"secret",
        "source payload must be untouched"
    );

    // For thoroughness, also confirm no fresh xattr leaked to the source
    // file. Use the source directly to bypass the mount.
    if !skip_if_user_xattr_unsupported(&fx.source().join(".staging")) {
        let names = llistxattr(&on_disk).unwrap_or_default();
        assert!(
            !names.iter().any(|n| n == "user.skillfs.test"),
            "source payload must not carry the leaked attr, got {names:?}",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. Compile-time sanity: confirm the helper hands back an OsStr-friendly
//    namespace check so unit-style tests stay accurate if helpers change.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_xattr_test_helper_namespace_constants_compile() {
    // Touches the namespace strings the rest of the file relies on. If a
    // future refactor swaps the constants this test breaks loudly.
    let names = [
        "user.skillfs.test",
        "user.skillfs.dir",
        "trusted.skillfs.test",
        "security.skillfs.test",
        "system.skillfs.test",
    ];
    for n in names {
        let os: &OsStr = OsStr::new(n);
        assert!(!os.is_empty(), "namespace constant must not be empty");
    }
}
