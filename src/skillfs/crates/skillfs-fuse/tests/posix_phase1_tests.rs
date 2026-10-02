//! POSIX Phase 1 acceptance tests — the entrypoint for Package H.
//!
//! Goals:
//!   * Provide a stable suite that exercises P0 acceptance behaviors end-to-end
//!     through a real FUSE mount.
//!   * Skip gracefully when FUSE is unavailable instead of failing.
//!   * Cover the gaps the audit flagged in `POSIX_FS_TEST_MATRIX.csv`:
//!       - `io/negative_or_large_offset`
//!       - `directory/mkdir`, `directory/rmdir`, `namespace/unlink`
//!         passthrough errno behavior;
//!       - `mount/inplace_normal_parity` for the core read/write/access path;
//!       - `io/write_skill_md_store_sync` after `unlink` in addition to the
//!         existing rename + truncate coverage.
//!
//! Existing detailed coverage lives in:
//!   * `crates/skillfs-fuse/tests/posix_open_io_tests.rs`
//!     (open flags, read/write, fsync, access, setattr, readdir snapshots,
//!     rename flag handling).
//!   * `crates/skillfs-fuse/tests/write_guard_tests.rs`
//!     (write passthrough guards, EROFS rejections, mkdir/rename invariants,
//!     normal vs in-place mount parity for the immediate-consistency path).
//!
//! This file deliberately does NOT duplicate that coverage; it only adds what
//! the audit identified as missing or partial.
//!
//! Phase 1 closeout additions:
//!   - `path/lookup`: explicit acceptance over the six path classes (mount root,
//!     `/skills`, skill dir, compiled `SKILL.md`, passthrough file, missing
//!     path errno).

use std::ffi::CString;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

mod common;

use common::{MountFixture, MountMode, create_skill_dir, list_dir_names};

// ─────────────────────────────────────────────────────────────────────────────
// path/lookup (P0)
// ─────────────────────────────────────────────────────────────────────────────
//
// The matrix calls for "stable inode and accurate errno for virtual and
// physical paths" across root, `/skills`, skill dirs, `SKILL.md`, passthrough
// files, and missing paths. Other tests touch lookup as a side-effect of read
// or readdir; this section exercises it explicitly so reviewers grepping the
// matrix find a one-shot reference.

#[test]
fn test_lookup_known_paths_return_stable_inodes() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "lookup-skill");
        std::fs::write(src.join("lookup-skill/data.txt"), b"payload").unwrap();
    });

    let cases: [(&str, std::path::PathBuf, bool); 5] = [
        ("mount root", fx.mountpoint().to_path_buf(), true),
        ("/skills", fx.skills_root(), true),
        ("skill dir", fx.skill_path("lookup-skill"), true),
        (
            "SKILL.md",
            fx.passthrough_path("lookup-skill", "SKILL.md"),
            false,
        ),
        (
            "passthrough",
            fx.passthrough_path("lookup-skill", "data.txt"),
            false,
        ),
    ];

    for (label, path, expect_dir) in &cases {
        let m1 =
            std::fs::metadata(path).unwrap_or_else(|e| panic!("[{label}] first stat failed: {e}"));
        let m2 =
            std::fs::metadata(path).unwrap_or_else(|e| panic!("[{label}] second stat failed: {e}"));

        assert_eq!(
            m1.is_dir(),
            *expect_dir,
            "[{label}] is_dir={} expected {}",
            m1.is_dir(),
            expect_dir
        );
        assert_eq!(
            m1.is_file(),
            !*expect_dir,
            "[{label}] is_file={} expected {}",
            m1.is_file(),
            !*expect_dir
        );
        assert_eq!(
            m1.ino(),
            m2.ino(),
            "[{label}] inode must be stable across lookups: {} vs {}",
            m1.ino(),
            m2.ino()
        );
    }

    // Compiled SKILL.md should report a non-zero size so downstream tools that
    // pre-allocate from `st_size` can read at least one byte.
    let skill_md_meta = std::fs::metadata(fx.passthrough_path("lookup-skill", "SKILL.md"))
        .expect("SKILL.md metadata");
    assert!(
        skill_md_meta.len() > 0,
        "compiled SKILL.md size must be > 0, got {}",
        skill_md_meta.len()
    );
}

#[test]
fn test_lookup_missing_path_returns_enoent() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "lookup-skill");
    });

    // Missing names under each layer of the mount: root, virtual /skills, and
    // a real skill directory. All three must surface ENOENT, not EIO or a
    // synthetic error code.
    let missing: [(&str, std::path::PathBuf); 3] = [
        ("under mount root", fx.mountpoint().join("nope")),
        ("under /skills", fx.skills_root().join("ghost-skill")),
        (
            "under skill dir",
            fx.skill_path("lookup-skill").join("absent.txt"),
        ),
    ];

    for (label, path) in &missing {
        let err = std::fs::metadata(path)
            .err()
            .unwrap_or_else(|| panic!("[{label}] missing path must fail: {path:?}"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert_eq!(
            raw,
            libc::ENOENT,
            "[{label}] missing must surface ENOENT, got {raw} ({err})"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// io/negative_or_large_offset (P0)
// ─────────────────────────────────────────────────────────────────────────────
//
// The FUSE protocol passes offsets as `u64`, so userspace cannot send a
// negative offset to SkillFS — `pread`/`pwrite` take `off_t` (`i64`) and the
// kernel rejects negative values with `EINVAL` before they reach the FUSE
// driver. The remaining concern is that SkillFS handles "valid `u64`" offsets
// without buffering the entire file, returns EOF beyond size, and lets the
// underlying filesystem write a sparse hole when offset > file size.

/// `pread` with a negative offset must fail with EINVAL. The kernel intercepts
/// this; SkillFS just needs to not accept it as a successful zero-length read.
#[test]
fn test_pread_negative_offset_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        std::fs::write(src.join("test-skill/data.txt"), b"hello").unwrap();
    });

    let mount_file = fx.passthrough_path("test-skill", "data.txt");
    let c_path = CString::new(mount_file.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
    assert!(fd >= 0, "open should succeed, got fd={fd}");

    let mut buf = [0u8; 4];
    let n = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut _, buf.len(), -1) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    unsafe { libc::close(fd) };

    assert_eq!(n, -1, "pread with negative offset must fail");
    assert_eq!(
        err,
        libc::EINVAL,
        "pread negative offset must produce EINVAL, got {err}"
    );
}

/// Reading at an offset beyond the file's size must return zero bytes (EOF),
/// not an error — and it must not buffer the whole file.
#[test]
fn test_read_beyond_eof_returns_zero() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        std::fs::write(src.join("test-skill/short.txt"), b"abc").unwrap();
    });

    use std::os::unix::fs::FileExt;
    let f = std::fs::File::open(fx.passthrough_path("test-skill", "short.txt"))
        .expect("open short.txt");

    let mut buf = [0u8; 16];

    // Far beyond EOF but well within u64::MAX (1 TiB).
    let n = f
        .read_at(&mut buf, 1u64 << 40)
        .expect("read_at huge offset");
    assert_eq!(n, 0, "read past EOF must return 0 bytes, got {n}");

    // Just at EOF.
    let n = f.read_at(&mut buf, 3).expect("read_at at EOF");
    assert_eq!(n, 0, "read at EOF must return 0 bytes, got {n}");
}

/// Writing at an offset past EOF on a passthrough file must extend the file
/// with a sparse hole and surface the resulting size + zero-fill correctly
/// when read back. This guards the "do not cast offset as u64 incorrectly"
/// concern flagged in the matrix.
#[test]
fn test_write_at_offset_creates_sparse_hole() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        std::fs::write(src.join("test-skill/sparse.bin"), b"").unwrap();
    });

    use std::os::unix::fs::FileExt;
    let path = fx.passthrough_path("test-skill", "sparse.bin");
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open for write");

    // Write 4 bytes at offset 100 — leaves a 100-byte hole.
    let n = f.write_at(b"DATA", 100).expect("pwrite at offset 100");
    assert_eq!(n, 4, "pwrite must report 4 bytes written");
    drop(f);

    let source_path = fx.source().join("test-skill/sparse.bin");
    let meta = std::fs::metadata(&source_path).expect("source metadata");
    assert_eq!(meta.len(), 104, "file size must be 100 + 4 = 104");

    let content = std::fs::read(&source_path).expect("read source");
    assert_eq!(content.len(), 104);
    assert!(
        content[..100].iter().all(|b| *b == 0),
        "hole must be zero-filled"
    );
    assert_eq!(&content[100..], b"DATA");
}

// ─────────────────────────────────────────────────────────────────────────────
// directory/mkdir, directory/rmdir, directory/nested_dirs (P0)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_passthrough_nested_mkdir_rmdir() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
    });

    // mkdir nested: scripts/a/b
    let nested = fx.passthrough_path("test-skill", "scripts/a/b");
    std::fs::create_dir_all(&nested).expect("mkdir nested");

    // Source must reflect the nested structure.
    assert!(fx.source().join("test-skill/scripts/a/b").is_dir());

    // rmdir bottom-up.
    std::fs::remove_dir(fx.passthrough_path("test-skill", "scripts/a/b")).expect("rmdir b");
    std::fs::remove_dir(fx.passthrough_path("test-skill", "scripts/a")).expect("rmdir a");
    std::fs::remove_dir(fx.passthrough_path("test-skill", "scripts")).expect("rmdir scripts");

    assert!(!fx.source().join("test-skill/scripts").exists());
}

#[test]
fn test_rmdir_nonempty_returns_enotempty() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        let dir = src.join("test-skill/scripts");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), b"keep").unwrap();
    });

    let result = std::fs::remove_dir(fx.passthrough_path("test-skill", "scripts"));
    let err = result.expect_err("rmdir on non-empty dir must fail");
    let raw = err.raw_os_error().unwrap_or(0);
    assert_eq!(
        raw,
        libc::ENOTEMPTY,
        "rmdir non-empty must surface ENOTEMPTY, got {raw}"
    );

    // Nothing was actually removed.
    assert!(fx.source().join("test-skill/scripts/inner.txt").exists());
}

#[test]
fn test_rmdir_missing_returns_enoent() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
    });

    let result = std::fs::remove_dir(fx.passthrough_path("test-skill", "ghost"));
    let err = result.expect_err("rmdir on missing dir must fail");
    let raw = err.raw_os_error().unwrap_or(0);
    assert_eq!(
        raw,
        libc::ENOENT,
        "rmdir missing must surface ENOENT, got {raw}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// namespace/unlink (P0)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_unlink_passthrough_succeeds() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        std::fs::write(src.join("test-skill/dispose.txt"), b"x").unwrap();
    });

    let path = fx.passthrough_path("test-skill", "dispose.txt");
    std::fs::remove_file(&path).expect("unlink should succeed");
    assert!(!fx.source().join("test-skill/dispose.txt").exists());
}

#[test]
fn test_unlink_missing_returns_enoent() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
    });

    let result = std::fs::remove_file(fx.passthrough_path("test-skill", "nope.txt"));
    let err = result.expect_err("unlink missing must fail");
    let raw = err.raw_os_error().unwrap_or(0);
    assert_eq!(
        raw,
        libc::ENOENT,
        "unlink missing must surface ENOENT, got {raw}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// io/write_skill_md_store_sync (P0) — unlink path
// ─────────────────────────────────────────────────────────────────────────────
//
// The matrix entry calls out "Existing tests cover rename stale name; add
// truncation/error cases". Truncation is covered by
// `posix_open_io_tests::test_skill_md_o_trunc_store_sync`. The remaining hole
// is the unlink case: deleting `SKILL.md` should remove the skill from the
// store so the directory disappears from the virtual `/skills` listing.

#[test]
fn test_skill_md_unlink_removes_store_entry() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "fading-skill");
        create_skill_dir(src, "keeper-skill");
    });

    // Sanity: both visible up front.
    let before = list_dir_names(&fx.skills_root());
    assert!(before.contains(&"fading-skill".to_string()), "{before:?}");
    assert!(before.contains(&"keeper-skill".to_string()), "{before:?}");

    // Remove SKILL.md through the mount.
    let skill_md = fx.passthrough_path("fading-skill", "SKILL.md");
    std::fs::remove_file(&skill_md).expect("unlink SKILL.md");

    // Wait long enough for the sync worker (debounce + reparse) to settle.
    std::thread::sleep(std::time::Duration::from_millis(400));

    let after = list_dir_names(&fx.skills_root());
    assert!(
        !after.contains(&"fading-skill".to_string()),
        "fading-skill must disappear after SKILL.md unlink, got {after:?}"
    );
    assert!(
        after.contains(&"keeper-skill".to_string()),
        "keeper-skill must remain visible, got {after:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// mount/inplace_normal_parity (P0)
// ─────────────────────────────────────────────────────────────────────────────
//
// `write_guard_tests` already covers the rename / mkdir immediate-consistency
// invariants in both modes. These tests cover the *POSIX* core of the parity
// matrix: read-compiled-SKILL.md, passthrough write+read round trip, chmod
// passthrough, and statfs.

fn parity_smoke(fx: &MountFixture) {
    let mode_label = match fx.mode() {
        MountMode::Normal => "normal",
        MountMode::InPlace => "in-place",
    };

    // 1. SKILL.md compiled read works.
    let skill_md = fx.passthrough_path("parity-skill", "SKILL.md");
    let compiled = std::fs::read_to_string(&skill_md)
        .unwrap_or_else(|e| panic!("[{mode_label}] read SKILL.md: {e}"));
    assert!(
        compiled.contains("parity-skill"),
        "[{mode_label}] compiled SKILL.md must mention skill name, got: {compiled}"
    );

    // 2. Passthrough write + read-back round trip.
    let data_file = fx.passthrough_path("parity-skill", "data.txt");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&data_file)
            .unwrap_or_else(|e| panic!("[{mode_label}] open for write: {e}"));
        f.write_all(b"parity").unwrap();
    }
    let payload =
        std::fs::read(&data_file).unwrap_or_else(|e| panic!("[{mode_label}] read back: {e}"));
    assert_eq!(payload, b"parity", "[{mode_label}] write/read round trip");

    // 3. chmod passthrough.
    std::fs::set_permissions(&data_file, std::fs::Permissions::from_mode(0o600))
        .unwrap_or_else(|e| panic!("[{mode_label}] chmod: {e}"));
    let src_meta =
        std::fs::metadata(fx.source().join("parity-skill/data.txt")).expect("source metadata");
    assert_eq!(
        src_meta.permissions().mode() & 0o7777,
        0o600,
        "[{mode_label}] chmod must reach source"
    );

    // 4. statfs reports non-zero stats.
    let c_path = CString::new(fx.mountpoint().to_str().unwrap()).unwrap();
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    assert_eq!(ret, 0, "[{mode_label}] statvfs must succeed");
    assert!(stat.f_blocks > 0, "[{mode_label}] f_blocks > 0");
    assert!(stat.f_bsize > 0, "[{mode_label}] f_bsize > 0");
}

#[test]
fn test_parity_normal_mode() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "parity-skill");
    });
    parity_smoke(&fx);
}

#[test]
fn test_parity_inplace_mode() {
    skip_if_no_fuse!();

    let fx = MountFixture::in_place(|src| {
        create_skill_dir(src, "parity-skill");
    });
    parity_smoke(&fx);
}

/// In-place specific: the skills root `IS` the mountpoint, so the seeded skill
/// must be visible at `<mount>/parity-skill` directly (no `/skills/` prefix).
#[test]
fn test_inplace_skills_layout_no_prefix() {
    skip_if_no_fuse!();

    let fx = MountFixture::in_place(|src| {
        create_skill_dir(src, "parity-skill");
    });

    // Visible at the root.
    let listing = list_dir_names(fx.mountpoint());
    assert!(
        listing.contains(&"parity-skill".to_string()),
        "in-place root must list parity-skill, got {listing:?}"
    );

    // Confirm the explicit skill_path matches the root layout.
    assert_eq!(
        fx.skill_path("parity-skill"),
        fx.mountpoint().join("parity-skill")
    );

    // SKILL.md still readable from the in-place layout.
    let compiled = std::fs::read_to_string(fx.skill_path("parity-skill").join("SKILL.md"))
        .expect("read SKILL.md in-place");
    assert!(compiled.contains("parity-skill"));
}

// ─────────────────────────────────────────────────────────────────────────────
// FUSE-unavailable graceful skip (executed even without FUSE)
// ─────────────────────────────────────────────────────────────────────────────

/// Sanity test that the harness honors absent FUSE. This test runs in every
/// environment — if FUSE is unavailable it must succeed via the skip branch;
/// if FUSE is available it must succeed via the mount branch.
///
/// The mount branch exercises a real FUSE-served read (compiled `SKILL.md`)
/// and a virtual-listing check, so mount-thread failures produce an explicit
/// test failure rather than a silent pass. Path-existence alone would be a
/// false positive — `tempfile::tempdir` creates the directory before mounting.
#[test]
fn test_harness_skip_when_fuse_unavailable() {
    if !common::fuse_available() {
        eprintln!("OK: harness reports FUSE unavailable, suite would skip");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness-probe");
    });

    // 1. The virtual `/skills` listing must include the seeded skill — this
    //    only succeeds if `readdir` was actually served by SkillFS through
    //    the FUSE channel.
    let listing = list_dir_names(&fx.skills_root());
    assert!(
        listing.contains(&"harness-probe".to_string()),
        "harness-probe must be visible through the mount, got {listing:?}"
    );

    // 2. Reading `<mount>/skills/harness-probe/SKILL.md` goes through the
    //    SkillFS compiled-read path. If the background mount thread had
    //    failed, this read would surface an OS error.
    let compiled = std::fs::read_to_string(fx.passthrough_path("harness-probe", "SKILL.md"))
        .expect("compiled SKILL.md read through mount must succeed");
    assert!(
        compiled.contains("harness-probe"),
        "compiled SKILL.md must mention skill name, got {compiled:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// metadata/lstat_symlink, metadata/readlink (Package I)
// ─────────────────────────────────────────────────────────────────────────────
//
// Package I exposes symlink identity for physical passthrough paths and adds
// a `readlink` callback. The classifier at `skillfs_fuse::symlink_policy` is
// purely lexical and is exercised by unit tests below; these integration tests
// drive the FUSE callbacks themselves.

/// `lstat` on a passthrough symlink must report `S_IFLNK` rather than the
/// followed target, so tools like `find -type l` and `ls -l` see the link
/// identity. `stat` (which follows) must continue to report the target's
/// attributes.
#[test]
fn test_lstat_passthrough_symlink_reports_symlink() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "link-skill");
        let scripts = src.join("link-skill/scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("real.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("real.txt", scripts.join("link.txt")).unwrap();
    });

    let link_path = fx.passthrough_path("link-skill", "scripts/link.txt");

    let lmeta = std::fs::symlink_metadata(&link_path).expect("lstat through mount");
    assert!(
        lmeta.file_type().is_symlink(),
        "lstat must report symlink, got {:?}",
        lmeta.file_type()
    );

    // `stat` follows the link and sees the regular file behind it.
    let smeta = std::fs::metadata(&link_path).expect("stat through mount");
    assert!(
        smeta.is_file(),
        "stat must follow link to regular file, got {:?}",
        smeta.file_type()
    );
    assert_eq!(smeta.len(), b"hello".len() as u64);
}

/// `readdir` snapshots must distinguish symlink entries from regular files,
/// because tools that pre-filter by `d_type` rely on the kernel reflecting
/// `DT_LNK`.
#[test]
fn test_readdir_distinguishes_symlink_entries() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "link-skill");
        let scripts = src.join("link-skill/scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("real.txt"), b"hi").unwrap();
        std::os::unix::fs::symlink("real.txt", scripts.join("link.txt")).unwrap();
    });

    let dir = fx.passthrough_path("link-skill", "scripts");
    let mut saw_symlink = false;
    let mut saw_regular = false;
    for entry in std::fs::read_dir(&dir).expect("readdir scripts") {
        let entry = entry.expect("dir entry");
        let ft = entry.file_type().expect("dir entry file_type");
        match entry.file_name().to_string_lossy().as_ref() {
            "link.txt" => {
                assert!(
                    ft.is_symlink(),
                    "link.txt must be reported as symlink in readdir, got {ft:?}"
                );
                saw_symlink = true;
            }
            "real.txt" => {
                assert!(ft.is_file(), "real.txt must be regular");
                saw_regular = true;
            }
            _ => {}
        }
    }
    assert!(saw_symlink && saw_regular, "expected both entries");
}

/// `readlink` on a passthrough symlink returns the raw target bytes recorded
/// when the link was created, even when the target does not exist or contains
/// `..` traversal.
#[test]
fn test_readlink_passthrough_returns_raw_target() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "link-skill");
        let scripts = src.join("link-skill/scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        // Relative target with `..` — readlink must return verbatim.
        std::os::unix::fs::symlink("../SKILL.md", scripts.join("up-md")).unwrap();
        // Absolute target outside source — readlink must return verbatim.
        std::os::unix::fs::symlink("/nonexistent/abs", scripts.join("abs-link")).unwrap();
        // Broken target — readlink succeeds; only follow-stat would fail.
        std::os::unix::fs::symlink("does-not-exist", scripts.join("broken")).unwrap();
    });

    let cases: &[(&str, &str)] = &[
        ("scripts/up-md", "../SKILL.md"),
        ("scripts/abs-link", "/nonexistent/abs"),
        ("scripts/broken", "does-not-exist"),
    ];
    for (rel, expected) in cases {
        let target = std::fs::read_link(fx.passthrough_path("link-skill", rel))
            .unwrap_or_else(|e| panic!("[{rel}] readlink failed: {e}"));
        assert_eq!(
            target.to_string_lossy(),
            *expected,
            "[{rel}] readlink target mismatch"
        );
    }
}

/// `readlink` on a missing path must surface `ENOENT`, matching Linux semantics
/// — no falling back to `EIO` and no synthetic success.
#[test]
fn test_readlink_missing_path_returns_enoent() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "link-skill");
        let scripts = src.join("link-skill/scripts");
        std::fs::create_dir_all(&scripts).unwrap();
    });

    let missing = fx.passthrough_path("link-skill", "scripts/ghost");
    let c_path = CString::new(missing.to_str().unwrap()).unwrap();
    let mut buf = [0u8; 64];
    let ret = unsafe { libc::readlink(c_path.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
    assert_eq!(ret, -1, "readlink on missing path must fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(err, libc::ENOENT, "expected ENOENT, got errno {err}");
}

/// `readlink` on virtual paths (root, `/skills`, a skill dir, compiled
/// `SKILL.md`, `skill-discover/SKILL.md`) must fail deterministically. The
/// FUSE callback returns `EINVAL` to match Linux's behavior on non-symlink
/// targets, which the kernel surfaces verbatim.
#[test]
fn test_readlink_virtual_paths_fail_deterministically() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "link-skill");
    });

    let virtual_paths: [(&str, std::path::PathBuf); 5] = [
        ("mount root", fx.mountpoint().to_path_buf()),
        ("/skills", fx.skills_root()),
        ("skill dir", fx.skill_path("link-skill")),
        (
            "compiled SKILL.md",
            fx.passthrough_path("link-skill", "SKILL.md"),
        ),
        (
            "skill-discover SKILL",
            fx.skills_root().join("skill-discover/SKILL.md"),
        ),
    ];

    for (label, path) in &virtual_paths {
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        let mut buf = [0u8; 64];
        let ret = unsafe { libc::readlink(c_path.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
        assert_eq!(ret, -1, "[{label}] readlink must fail on virtual path");
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert_eq!(
            err,
            libc::EINVAL,
            "[{label}] virtual readlink must surface EINVAL, got {err}"
        );
    }
}

/// Symlink creation at virtual paths (`/skills`, root) remains rejected.
/// Package T2 enables same-skill symlinks under `Passthrough` leaves; the
/// in-skill path is exercised by `posix_link_fifo_tests` rather than here.
#[test]
fn test_symlink_creation_at_virtual_paths_still_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "link-skill");
    });

    // Under `/skills` (no physical mapping at the virtual layer). The
    // virtual semantics remain EROFS — only ordinary passthrough leaves
    // host writable inodes.
    let at_skills = fx.skills_root().join("rogue-link");
    let err = std::os::unix::fs::symlink("/tmp/whatever", &at_skills)
        .expect_err("symlink creation at /skills must fail");
    let raw = err.raw_os_error().unwrap_or(0);
    assert_eq!(
        raw,
        libc::EROFS,
        "symlink at /skills must surface EROFS, got {raw}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Symlink target boundary classifier (Package I) — pure unit tests
// ─────────────────────────────────────────────────────────────────────────────
//
// `skillfs_fuse::symlink_policy::classify_symlink_target` is the seam future
// Skill Security work will plug into. These tests pin its lexical behavior
// without spinning up FUSE so they run in every environment.

#[test]
fn test_classify_symlink_target_same_skill_relative() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha", "beta"],
        std::path::Path::new("/srv/skillfs/source/alpha/scripts"),
        std::path::Path::new("../SKILL.md"),
    );
    assert_eq!(class, SymlinkTargetClass::SameSkill);
}

#[test]
fn test_classify_symlink_target_same_skill_absolute() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha", "beta"],
        std::path::Path::new("/srv/skillfs/source/alpha/scripts"),
        std::path::Path::new("/srv/skillfs/source/alpha/data/file.txt"),
    );
    assert_eq!(class, SymlinkTargetClass::SameSkill);
}

#[test]
fn test_classify_symlink_target_cross_skill() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha", "beta"],
        std::path::Path::new("/srv/skillfs/source/alpha/scripts"),
        std::path::Path::new("../../beta/SKILL.md"),
    );
    assert_eq!(
        class,
        SymlinkTargetClass::CrossSkill {
            other_skill: "beta".to_string()
        }
    );
}

#[test]
fn test_classify_symlink_target_inside_source_outside_skill() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    // Top-level source file that is not a registered skill (e.g. config).
    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha", "beta"],
        std::path::Path::new("/srv/skillfs/source/alpha"),
        std::path::Path::new("/srv/skillfs/source/skillfs-views.toml"),
    );
    assert_eq!(class, SymlinkTargetClass::InsideSourceOutsideSkill);
}

#[test]
fn test_classify_symlink_target_outside_source_absolute() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha"],
        std::path::Path::new("/srv/skillfs/source/alpha/scripts"),
        std::path::Path::new("/etc/passwd"),
    );
    assert_eq!(class, SymlinkTargetClass::OutsideSource);
}

#[test]
fn test_classify_symlink_target_outside_source_via_parent_traversal() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    // `../../../etc/passwd` from a/scripts escapes the source root.
    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha"],
        std::path::Path::new("/srv/skillfs/source/alpha/scripts"),
        std::path::Path::new("../../../etc/passwd"),
    );
    assert_eq!(class, SymlinkTargetClass::OutsideSource);
}

#[test]
fn test_classify_symlink_target_relative_unknown_cases() {
    use skillfs_fuse::symlink_policy::{SymlinkTargetClass, classify_symlink_target};

    // Empty target: caller cannot lexically resolve.
    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha"],
        std::path::Path::new("/srv/skillfs/source/alpha"),
        std::path::Path::new(""),
    );
    assert_eq!(class, SymlinkTargetClass::RelativeUnknown);

    // Relative target with non-absolute link parent — refuse to guess.
    let class = classify_symlink_target(
        std::path::Path::new("/srv/skillfs/source"),
        "alpha",
        &["alpha"],
        std::path::Path::new("relative/parent"),
        std::path::Path::new("file.txt"),
    );
    assert_eq!(class, SymlinkTargetClass::RelativeUnknown);
}

// ─────────────────────────────────────────────────────────────────────────────
// Package S1: `.skill-meta` protection MVP
// ─────────────────────────────────────────────────────────────────────────────
//
// `.skill-meta/**` under each skill is read-visible but mutation-protected by
// default. These tests pin the FUSE-level behavior: read/stat/readdir keep
// working, every documented mutation surface is rejected with `EACCES`, and
// non-`.skill-meta` passthrough operations stay unaffected.

/// Seed a skill containing a small `.skill-meta` directory with a manifest and
/// a nested signatures payload. Returns the fixture so tests can drive it.
fn fixture_with_skill_meta(skill: &str) -> MountFixture {
    let skill_owned = skill.to_string();
    MountFixture::normal(move |src| {
        create_skill_dir(src, &skill_owned);
        let meta = src.join(&skill_owned).join(".skill-meta");
        std::fs::create_dir_all(meta.join("signatures")).expect("mkdir .skill-meta");
        std::fs::write(meta.join("manifest.json"), b"{\"v\":1}").expect("seed manifest");
        std::fs::write(meta.join("signatures").join("root.json"), b"signed-payload")
            .expect("seed signatures/root.json");
    })
}

#[test]
fn test_skill_meta_read_stat_readdir_still_work() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");

    // Untrusted callers cannot see .skill-meta in readdir.
    let skill_listing = list_dir_names(&fx.skill_path("alpha"));
    assert!(
        !skill_listing.contains(&".skill-meta".to_string()),
        "skill listing must NOT include .skill-meta for untrusted, got {skill_listing:?}"
    );

    // Untrusted callers cannot stat .skill-meta.
    let err = std::fs::metadata(fx.passthrough_path("alpha", ".skill-meta"))
        .expect_err(".skill-meta stat must fail for untrusted");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

    // Untrusted callers cannot read .skill-meta files.
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");
    let err = std::fs::read(&manifest).expect_err("manifest read must fail for untrusted");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn test_skill_meta_create_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let target = fx.passthrough_path("alpha", ".skill-meta/new.json");

    let err = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&target)
        .expect_err("create under .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "create under .skill-meta must surface ENOENT, got {err}"
    );
    // No partial file left on the source side.
    assert!(
        !fx.source().join("alpha/.skill-meta/new.json").exists(),
        "denied create must not have written to the source"
    );
}

#[test]
fn test_skill_meta_open_for_write_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    let err = std::fs::OpenOptions::new()
        .write(true)
        .open(&manifest)
        .expect_err("write open of .skill-meta file must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "write open must surface ENOENT, got {err}"
    );

    let original = std::fs::read(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("source manifest read");
    assert_eq!(
        original, b"{\"v\":1}",
        "denied open must not change source content"
    );
}

#[test]
fn test_skill_meta_open_with_trunc_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    let err = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&manifest)
        .expect_err("write|trunc open of .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "write|trunc open must surface ENOENT, got {err}"
    );

    let original = std::fs::read(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("source manifest read");
    assert_eq!(original, b"{\"v\":1}", "trunc must not have happened");
}

#[test]
fn test_skill_meta_rdonly_trunc_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    // O_RDONLY|O_TRUNC mutates on Linux, so SkillFS must reject it for
    // protected metadata.
    let c_path = CString::new(manifest.to_str().unwrap()).unwrap();
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_TRUNC) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if fd >= 0 {
        unsafe { libc::close(fd) };
    }
    assert_eq!(fd, -1, "O_RDONLY|O_TRUNC must fail for .skill-meta");
    assert_eq!(
        err,
        libc::ENOENT,
        "O_RDONLY|O_TRUNC must surface ENOENT, got {err}"
    );

    let original = std::fs::read(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("source manifest read");
    assert_eq!(
        original, b"{\"v\":1}",
        "O_RDONLY|O_TRUNC must not have truncated the source"
    );
}

#[test]
fn test_skill_meta_unlink_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    let err = std::fs::remove_file(&manifest).expect_err("unlink under .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "unlink must surface ENOENT, got {err}"
    );
    assert!(
        fx.source().join("alpha/.skill-meta/manifest.json").exists(),
        ".skill-meta file must remain after denied unlink"
    );
}

#[test]
fn test_skill_meta_rmdir_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");

    // Even an empty subdir under .skill-meta cannot be removed.
    let empty = fx.source().join("alpha/.skill-meta/empty");
    std::fs::create_dir(&empty).expect("seed empty dir");

    let err = std::fs::remove_dir(fx.passthrough_path("alpha", ".skill-meta/empty"))
        .expect_err("rmdir under .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "rmdir must surface ENOENT, got {err}"
    );
    assert!(empty.exists(), "rmdir target must remain on source");
}

#[test]
fn test_skill_meta_mkdir_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let new_dir = fx.passthrough_path("alpha", ".skill-meta/newsub");

    let err = std::fs::create_dir(&new_dir).expect_err("mkdir under .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "mkdir must surface ENOENT, got {err}"
    );
    assert!(
        !fx.source().join("alpha/.skill-meta/newsub").exists(),
        "mkdir denied must not have created anything on source"
    );
}

#[test]
fn test_skill_meta_rename_from_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");

    let from = fx.passthrough_path("alpha", ".skill-meta/manifest.json");
    let to = fx.passthrough_path("alpha", "manifest.json");

    let err = std::fs::rename(&from, &to).expect_err("rename out of .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "rename from must surface ENOENT, got {err}"
    );
    assert!(
        fx.source().join("alpha/.skill-meta/manifest.json").exists(),
        "source must still hold manifest.json"
    );
    assert!(
        !fx.source().join("alpha/manifest.json").exists(),
        "rename target must not have been created on source"
    );
}

#[test]
fn test_skill_meta_rename_to_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");

    // Seed an outside-meta file that we'll attempt to move into `.skill-meta`.
    std::fs::write(fx.source().join("alpha/normal.txt"), b"untouched").unwrap();

    let from = fx.passthrough_path("alpha", "normal.txt");
    let to = fx.passthrough_path("alpha", ".skill-meta/normal.txt");

    let err = std::fs::rename(&from, &to).expect_err("rename into .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "rename into must surface ENOENT, got {err}"
    );
    assert!(
        fx.source().join("alpha/normal.txt").exists(),
        "source file must remain"
    );
    assert!(
        !fx.source().join("alpha/.skill-meta/normal.txt").exists(),
        "rename target must not have been created"
    );
}

#[test]
fn test_skill_meta_truncate_path_returns_eacces() {
    skip_if_no_fuse!();

    // `truncate(2)` goes through SkillFS's `setattr(size = ...)` path
    // without any open() call, so this exercises the policy gate that
    // O_TRUNC opens cannot reach.
    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    let original =
        std::fs::read(fx.source().join("alpha/.skill-meta/manifest.json")).expect("source read");
    let original_len = original.len();

    let c_path = CString::new(manifest.to_str().unwrap()).unwrap();
    let ret = unsafe { libc::truncate(c_path.as_ptr(), 0) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);

    assert_eq!(ret, -1, "truncate on .skill-meta must fail");
    assert_eq!(
        err,
        libc::ENOENT,
        "truncate on .skill-meta must surface ENOENT, got {err}"
    );

    let after = std::fs::read(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("source read after");
    assert_eq!(
        after.len(),
        original_len,
        "truncate denied must leave length unchanged"
    );
    assert_eq!(
        after, original,
        "truncate denied must leave bytes unchanged"
    );
}

#[test]
fn test_skill_meta_chmod_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    let original_mode = std::fs::metadata(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("source metadata")
        .mode()
        & 0o7777;

    let err = std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600))
        .expect_err("chmod under .skill-meta must fail");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        "chmod must surface ENOENT, got {err}"
    );

    let after = std::fs::metadata(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("source metadata after")
        .mode()
        & 0o7777;
    assert_eq!(
        after, original_mode,
        "chmod denied must not have altered source mode"
    );
}

#[test]
fn test_skill_meta_utimens_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");

    let c_path = CString::new(manifest.to_str().unwrap()).unwrap();
    // Pin to a known time so we can assert it did NOT change.
    let target_time = libc::timespec {
        tv_sec: 1_000_000,
        tv_nsec: 0,
    };
    let times = [target_time, target_time];

    let original_mtime = std::fs::metadata(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("metadata")
        .mtime();

    let ret = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);

    assert_eq!(ret, -1, "utimens under .skill-meta must fail");
    assert_eq!(err, libc::ENOENT, "utimens must surface ENOENT, got {err}");

    let after_mtime = std::fs::metadata(fx.source().join("alpha/.skill-meta/manifest.json"))
        .expect("metadata after")
        .mtime();
    assert_eq!(
        after_mtime, original_mtime,
        "utimens denied must not have changed mtime"
    );
}

#[test]
fn test_skill_meta_access_w_ok_returns_eacces() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let manifest = fx.passthrough_path("alpha", ".skill-meta/manifest.json");
    let c_path = CString::new(manifest.to_str().unwrap()).unwrap();

    // W_OK denied by policy.
    let ret = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) };
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(ret, -1, "access W_OK must fail");
    assert_eq!(
        err,
        libc::ENOENT,
        "access W_OK must surface ENOENT, got {err}"
    );

    // F_OK / R_OK also fail — untrusted callers cannot even see .skill-meta.
    let ret_f = unsafe { libc::access(c_path.as_ptr(), libc::F_OK) };
    assert_eq!(ret_f, -1, "F_OK must fail for untrusted .skill-meta entry");
    let ret_r = unsafe { libc::access(c_path.as_ptr(), libc::R_OK) };
    assert_eq!(ret_r, -1, "R_OK must fail for untrusted .skill-meta entry");
}

#[test]
fn test_non_meta_passthrough_mutation_still_succeeds() {
    skip_if_no_fuse!();

    let fx = fixture_with_skill_meta("alpha");
    let normal = fx.passthrough_path("alpha", "regular.txt");

    // Create, write, chmod, unlink — all should keep working outside `.skill-meta`.
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&normal)
            .expect("create regular.txt");
        f.write_all(b"hello").expect("write regular.txt");
    }
    assert_eq!(
        std::fs::read(fx.source().join("alpha/regular.txt")).unwrap(),
        b"hello"
    );

    std::fs::set_permissions(&normal, std::fs::Permissions::from_mode(0o644))
        .expect("chmod regular.txt");

    std::fs::remove_file(&normal).expect("unlink regular.txt");
    assert!(!fx.source().join("alpha/regular.txt").exists());
}

#[test]
fn test_neighbour_named_skill_meta2_is_not_protected() {
    skip_if_no_fuse!();

    // Lookalike directory `.skill-meta2` must NOT be treated as protected
    // metadata — only the exact `.skill-meta` directory class is reserved.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        std::fs::create_dir_all(src.join("alpha/.skill-meta2")).unwrap();
    });

    let target = fx.passthrough_path("alpha", ".skill-meta2/file.txt");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&target)
            .expect("create under .skill-meta2 must succeed");
        f.write_all(b"ok").unwrap();
    }
    assert_eq!(
        std::fs::read(fx.source().join("alpha/.skill-meta2/file.txt")).unwrap(),
        b"ok"
    );
}

#[test]
fn test_skill_meta_protection_holds_for_symlink_creation() {
    skip_if_no_fuse!();

    // Package T2 enables same-skill symlink creation. The `.skill-meta`
    // view gate hides the namespace from untrusted callers, so symlink
    // creation fails at path resolution (ENOENT).
    let fx = fixture_with_skill_meta("alpha");

    let inside_meta = fx.passthrough_path("alpha", ".skill-meta/planted-link");
    let err = std::os::unix::fs::symlink("SKILL.md", &inside_meta)
        .expect_err("symlink under .skill-meta must remain rejected");
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        ".skill-meta symlink creation must surface ENOENT, got {err}"
    );
}
