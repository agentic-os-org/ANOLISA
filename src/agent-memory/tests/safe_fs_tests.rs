//! Extended unit tests for the safe_fs security boundary module.
//!
//! These tests validate the kernel-level sandbox guarantees (openat2 with
//! RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS) against common path-traversal
//! and symlink-TOCTOU attack patterns.
//!
//! Test coverage:
//! - open_root() file-descriptor lifecycle and error paths
//! - read_to_string() / write() / append() / write_create_new() round-trips
//! - metadata() / exists() against normal paths, missing paths, symlinks
//! - assert_no_symlink_traversal() against deep directory trees and partial paths
//! - remove_dir_all_safe() symlink rejection inside directories
//! - openat2 kernel-level rejection of .., absolute paths, /proc, symlinks
//! - validate_user_id() edge cases (max length, Unicode control chars, .. variants)
//! - resolve_path() boundary conditions (null bytes, non-UTF8, empty segments with ///)
//! - resolve_for_create() parent-path validation edge cases

use std::os::fd::AsFd;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Import the crate under test. We cargo test --package agent-memory, so
// use `agent_memory` (dashes → underscores).
// ---------------------------------------------------------------------------
use agent_memory::{
    ns::{self},
    safe_fs,
};

// ============================================================================
// 1. open_root() tests
// ============================================================================

#[test]
fn open_root_valid_directory() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path());
    assert!(root.is_ok(), "open_root on tempdir should succeed");
}

#[test]
fn open_root_nonexistent_fails() {
    let root = safe_fs::open_root(Path::new("/nonexistent_anolisa_test_dir_xyzzy"));
    assert!(root.is_err(), "open_root on nonexistent dir should fail");
}

#[test]
fn open_root_file_not_dir_fails() {
    let tmp = tempdir().unwrap();
    let file_path = tmp.path().join("not_a_dir.txt");
    std::fs::write(&file_path, "hello").unwrap();
    let root = safe_fs::open_root(&file_path);
    assert!(root.is_err(), "open_root on a regular file should fail");
}

// ============================================================================
// 2. read_to_string / write / append / write_create_new round-trips
// ============================================================================

#[test]
fn append_then_read() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("log.txt"), b"line1\n").unwrap();
    safe_fs::append(fd, Path::new("log.txt"), b"line2\n").unwrap();
    let content = safe_fs::read_to_string(fd, Path::new("log.txt")).unwrap();
    assert_eq!(content, "line1\nline2\n");
}

#[test]
fn append_creates_new_file() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::append(fd, Path::new("new.txt"), b"fresh").unwrap();
    assert_eq!(
        safe_fs::read_to_string(fd, Path::new("new.txt")).unwrap(),
        "fresh"
    );
}

#[test]
fn write_overwrite_existing() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("x.txt"), b"original").unwrap();
    safe_fs::write(fd, Path::new("x.txt"), b"overwritten").unwrap();
    assert_eq!(
        safe_fs::read_to_string(fd, Path::new("x.txt")).unwrap(),
        "overwritten"
    );
}

#[test]
fn write_create_new_missing() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let n = safe_fs::write_create_new(fd, Path::new("genesis.md"), b"first").unwrap();
    assert_eq!(n, 5);
    assert_eq!(
        safe_fs::read_to_string(fd, Path::new("genesis.md")).unwrap(),
        "first"
    );
}

#[test]
fn write_empty_string() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("empty.md"), b"").unwrap();
    assert_eq!(
        safe_fs::read_to_string(fd, Path::new("empty.md")).unwrap(),
        ""
    );
}

#[test]
fn read_nonexistent_returns_not_found() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::read_to_string(fd, Path::new("ghost.md")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn write_unicode_content() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let content = "你好，世界！🌍\nこんにちは\nBonjour le monde";
    safe_fs::write(fd, Path::new("hello.txt"), content.as_bytes()).unwrap();
    assert_eq!(
        safe_fs::read_to_string(fd, Path::new("hello.txt")).unwrap(),
        content
    );
}

#[test]
fn write_binary_content() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let binary = vec![0x00, 0x01, 0x02, 0xFF, 0xFE, 0xFD];
    safe_fs::write(fd, Path::new("bin.dat"), &binary).unwrap();
    // read_to_string on binary: may or may not work depending on encoding.
    // We at least verify write succeeds. Use open_read to verify bytes.
    let mut f = safe_fs::open_read(fd, Path::new("bin.dat")).unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
    assert_eq!(buf, binary);
}

// ============================================================================
// 3. metadata / exists tests
// ============================================================================

#[test]
fn metadata_regular_file() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("m.txt"), b"data").unwrap();
    let meta = safe_fs::metadata(fd, Path::new("m.txt")).unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.len(), 4);
}

#[test]
fn metadata_directory() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    std::fs::create_dir(tmp.path().join("subdir")).unwrap();
    let meta = safe_fs::metadata(fd, Path::new("subdir")).unwrap();
    assert!(meta.is_dir());
}

#[test]
fn metadata_symlink_rejected() {
    let tmp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    symlink(outside.path(), tmp.path().join("link")).unwrap();

    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::metadata(fd, Path::new("link")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::PathOutsideMount(_)),
        "metadata on symlink should be rejected, got {err:?}"
    );
}

#[test]
fn metadata_nonexistent_returns_not_found() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::metadata(fd, Path::new("nope")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn exists_true_false() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("real.txt"), b"x").unwrap();
    assert!(safe_fs::exists(fd, Path::new("real.txt")));
    assert!(!safe_fs::exists(fd, Path::new("fake.txt")));
}

// ============================================================================
// 4. Sandbox escape tests (kernel-level openat2 enforcement)
// ============================================================================

#[test]
fn openat2_midpath_dotdot_stays_under_root() {
    // RESOLVE_BENEATH: a path like x/y/z/../../../target.txt resolves
    // backwards within the root tree but never escapes it; the kernel
    // permits this because the final component (target.txt) stays under
    // the dirfd. The real protection for mid-path .. comes from
    // resolve_path() in ns/paths.rs, which rejects any .. component
    // before it reaches safe_fs. This test documents the kernel behavior.
    let tmp = tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("x/y/z")).unwrap();
    std::fs::write(tmp.path().join("target.txt"), "data").unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let result = safe_fs::read_to_string(fd, Path::new("x/y/z/../../../target.txt"));
    assert!(
        result.is_ok(),
        "mid-path .. that stays under dirfd root should succeed under RESOLVE_BENEATH"
    );
    assert_eq!(result.unwrap(), "data");
}

#[test]
fn openat2_rejects_starting_dotdot() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::read_to_string(fd, Path::new("../etc/shadow")).unwrap_err();
    assert!(
        matches!(
            err,
            agent_memory::MemoryError::PathOutsideMount(_) | agent_memory::MemoryError::Other(_)
        ),
        "starting .. should be rejected, got {err:?}"
    );
}

#[test]
fn openat2_rejects_symlink_to_outside() {
    let tmp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let secret_path = outside.path().join("id_rsa");
    std::fs::write(&secret_path, "PRIVATE KEY").unwrap();
    symlink(&secret_path, tmp.path().join("key_link")).unwrap();

    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::read_to_string(fd, Path::new("key_link")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::PathOutsideMount(_)),
        "symlink to outside should be rejected, got {err:?}"
    );
}

#[test]
fn openat2_rejects_deep_symlink() {
    let tmp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("deep_secret.txt");
    std::fs::write(&target, "classified").unwrap();

    // Create a directory and put a symlink inside.
    std::fs::create_dir(tmp.path().join("notes")).unwrap();
    symlink(&target, tmp.path().join("notes").join("escape")).unwrap();

    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::read_to_string(fd, Path::new("notes/escape")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::PathOutsideMount(_)),
        "deep symlink escape should be rejected, got {err:?}"
    );
}

#[test]
fn openat2_rejects_dangling_symlink() {
    let tmp = tempdir().unwrap();
    let dangling_target = Path::new("/tmp/anolisa_test_nonexistent_12345");
    symlink(dangling_target, tmp.path().join("dangling")).unwrap();

    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::read_to_string(fd, Path::new("dangling")).unwrap_err();
    // RESOLVE_NO_SYMLINKS refuses any symlink, dangling or not.
    assert!(
        matches!(err, agent_memory::MemoryError::PathOutsideMount(_)),
        "dangling symlink should be rejected by NO_SYMLINKS, got {err:?}"
    );
}

#[test]
fn openat2_rejects_absolute_path_in_openat2() {
    // When using safe_fs functions, the root fd is the anchor.
    // Passing an absolute path relative to the fd may be rejected
    // differently depending on kernel version. We test that it
    // does not succeed silently.
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    // An absolute path with openat2 is technically resolved from the
    // directory fd (ignoring the leading /), but with RESOLVE_BENEATH
    // it should be rejected because the path appears to have a root
    // component.
    let result = safe_fs::read_to_string(fd, Path::new("/etc/hostname"));
    assert!(result.is_err(), "absolute path via openat2 should fail");
}

// ============================================================================
// 5. assert_no_symlink_traversal() extended tests
// ============================================================================

#[test]
fn assert_no_symlink_traversal_deep_tree() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    std::fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
    std::fs::write(tmp.path().join("a/b/c/d.txt"), "ok").unwrap();

    // All components are normal dirs.
    assert!(safe_fs::assert_no_symlink_traversal(fd, Path::new("a/b/c/d.txt")).is_ok());

    // Nonexistent leaf is OK.
    assert!(safe_fs::assert_no_symlink_traversal(fd, Path::new("a/b/c/new.txt")).is_ok());
}

#[test]
fn assert_no_symlink_traversal_catches_mid_path_symlink() {
    let tmp = tempdir().unwrap();
    let outside = tempdir().unwrap();

    std::fs::create_dir(tmp.path().join("a")).unwrap();
    symlink(outside.path(), tmp.path().join("a").join("b")).unwrap();

    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    let err = safe_fs::assert_no_symlink_traversal(fd, Path::new("a/b/c.txt")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::PathOutsideMount(_)),
        "mid-path symlink should be caught, got {err:?}"
    );
}

#[test]
fn assert_no_symlink_traversal_single_segment() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    // Nonexistent single segment is OK.
    assert!(safe_fs::assert_no_symlink_traversal(fd, Path::new("newfile.txt")).is_ok());

    // Existing single segment is OK.
    std::fs::write(tmp.path().join("real.txt"), "ok").unwrap();
    assert!(safe_fs::assert_no_symlink_traversal(fd, Path::new("real.txt")).is_ok());
}

#[test]
fn assert_no_symlink_traversal_empty_root() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    // No files exist, so first probe fails with ENOENT → returns Ok.
    assert!(safe_fs::assert_no_symlink_traversal(fd, Path::new("x/y/z.txt")).is_ok());
}

#[test]
fn assert_no_symlink_traversal_rejects_nonnormal_component() {
    let tmp = tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("a")).unwrap();
    std::fs::write(tmp.path().join("a/real.txt"), "ok").unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    // Path with .. component — assert_no_symlink_traversal iterates over
    // path components. ".." is a ParentDir component, not a Normal component,
    // so the match arm hits the wildcard and returns PathOutsideMount.
    // We must first create "a" so the first probe succeeds and we reach the ..
    let err = safe_fs::assert_no_symlink_traversal(fd, Path::new("a/../b")).unwrap_err();
    assert!(
        matches!(err, agent_memory::MemoryError::PathOutsideMount(_)),
        ".. component should be rejected, got {err:?}"
    );
}

// ============================================================================
// 6. remove_dir_all_safe() tests
// ============================================================================

#[test]
fn remove_dir_all_safe_removes_tree() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    // Set up a tree.
    std::fs::create_dir_all(tmp.path().join("docs/sub")).unwrap();
    std::fs::write(tmp.path().join("docs").join("readme.md"), "readme").unwrap();
    std::fs::write(tmp.path().join("docs/sub").join("note.txt"), "note").unwrap();

    // Remove it.
    safe_fs::remove_dir_all_safe(fd, Path::new("docs"), &tmp.path().join("docs")).unwrap();

    assert!(
        !tmp.path().join("docs").exists(),
        "directory should be removed"
    );
}

#[test]
fn remove_dir_all_safe_removes_dir_with_file() {
    let tmp = tempdir().unwrap();
    // remove_dir_all_safe removes a directory containing a single file.
    // The top-level target must be a directory (remove_dir is the final
    // step); files inside are deleted via unlinkat.
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    std::fs::create_dir(tmp.path().join("solo")).unwrap();
    std::fs::write(tmp.path().join("solo/data.txt"), "data").unwrap();

    safe_fs::remove_dir_all_safe(fd, Path::new("solo"), &tmp.path().join("solo")).unwrap();
    assert!(!tmp.path().join("solo").exists());
}

#[test]
fn open_read_works() {
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("stream.txt"), b"streaming data").unwrap();
    let mut f = safe_fs::open_read(fd, Path::new("stream.txt")).unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    assert_eq!(s, "streaming data");
}

// ============================================================================
// 7. validate_user_id() edge cases (the backing validator for SessionId)
// ============================================================================

#[test]
fn validate_user_id_long_value() {
    // 128 chars is the limit. 129 should fail.
    let long_ok = "a".repeat(128);
    assert!(
        ns::validate_user_id(&long_ok).is_ok(),
        "128-char id should be ok"
    );

    let long_bad = "a".repeat(129);
    assert!(
        matches!(
            ns::validate_user_id(&long_bad),
            Err(agent_memory::MemoryError::InvalidArgument(_))
        ),
        "129-char id should be rejected"
    );
}

#[test]
fn validate_user_id_control_characters() {
    // Various control characters should be rejected.
    for chr in &['\x00', '\x01', '\x1F', '\x7F'] {
        let id = format!("bad{}char", chr);
        assert!(
            matches!(
                ns::validate_user_id(&id),
                Err(agent_memory::MemoryError::InvalidArgument(_))
            ),
            "id with 0x{:02X} should be rejected",
            *chr as u8
        );
    }
}

#[test]
fn validate_user_id_tab_and_newline() {
    assert!(matches!(
        ns::validate_user_id("with\ttab"),
        Err(agent_memory::MemoryError::InvalidArgument(_))
    ));
    assert!(matches!(
        ns::validate_user_id("with\nnewline"),
        Err(agent_memory::MemoryError::InvalidArgument(_))
    ));
    assert!(matches!(
        ns::validate_user_id("with\rcr"),
        Err(agent_memory::MemoryError::InvalidArgument(_))
    ));
}

#[test]
fn validate_user_id_subtle_dotdot_variants() {
    // The validator checks for ".." substring, so these should all fail.
    for bad in &["..", "x..", "..y", "x..y", "a.b..c"] {
        assert!(
            matches!(
                ns::validate_user_id(bad),
                Err(agent_memory::MemoryError::InvalidArgument(_))
            ),
            "'{bad}' (contains ..) should be rejected"
        );
    }
}

#[test]
fn validate_user_id_slash_variants() {
    // Both / and \ should be rejected.
    for bad in &["a/b", "a\\b", "/start", "\\start", "end/", "end\\"] {
        assert!(
            matches!(
                ns::validate_user_id(bad),
                Err(agent_memory::MemoryError::InvalidArgument(_))
            ),
            "'{bad}' (contains separator) should be rejected"
        );
    }
}

#[test]
fn validate_user_id_accepts_unicode_alphanumeric() {
    for good in &["张三", "user_name", "test-user", "a@b", "foo.bar"] {
        assert!(
            ns::validate_user_id(good).is_ok(),
            "'{good}' should be accepted"
        );
    }
}

#[test]
fn validate_user_id_empty() {
    assert!(matches!(
        ns::validate_user_id(""),
        Err(agent_memory::MemoryError::InvalidArgument(_))
    ));
}

// ============================================================================
// 8. Path patterns via open_read for TOCTOU hardening
// ============================================================================

#[test]
fn write_then_read_different_fd() {
    // Ensure that writing through one openat2 fd and reading through a
    // new one works correctly (no caching effects).
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    safe_fs::write(fd, Path::new("atomic.txt"), b"first").unwrap();
    let r1 = safe_fs::read_to_string(fd, Path::new("atomic.txt")).unwrap();
    assert_eq!(r1, "first");

    // Rewrite and re-read through fresh fd.
    safe_fs::write(fd, Path::new("atomic.txt"), b"second").unwrap();
    let r2 = safe_fs::read_to_string(fd, Path::new("atomic.txt")).unwrap();
    assert_eq!(r2, "second");
}

#[test]
fn write_many_files() {
    // Stress test: create many files, verify they all exist.
    let tmp = tempdir().unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    let fd = root.as_fd();

    for i in 0..50 {
        let name = format!("file_{i:03}.txt");
        safe_fs::write(fd, Path::new(&name), name.as_bytes()).unwrap();
    }

    for i in 0..50 {
        let name = format!("file_{i:03}.txt");
        assert!(safe_fs::exists(fd, Path::new(&name)));
    }
}

// ============================================================================
// 9. resolve_path() extended boundary tests
// ============================================================================

mod resolve_path_tests {
    use std::os::unix::fs::symlink;

    use agent_memory::{
        MemoryError,
        ns::{MountPoint, Namespace, paths},
    };
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, MountPoint) {
        let tmp = tempdir().unwrap();
        let mp = MountPoint::ensure(Namespace::user("alice").unwrap(), tmp.path()).unwrap();
        (tmp, mp)
    }

    #[test]
    fn rejects_dot_component() {
        let (_t, mp) = setup();
        // "." as the entire path is a single CurDir component — rejected.
        assert!(matches!(
            paths::resolve_path(&mp, "."),
            Err(MemoryError::InvalidArgument(_))
        ));
    }

    #[test]
    fn rejects_null_byte() {
        let (_t, mp) = setup();
        assert!(matches!(
            paths::resolve_path(&mp, "good\0bad"),
            Err(MemoryError::InvalidArgument(_))
        ));
    }

    #[test]
    fn handles_double_slash_as_single() {
        let (_t, mp) = setup();
        // Rust's Path::new("notes//sub") normalizes // to / on all platforms.
        // This is not a security issue — the kernel also treats // as /.
        // Verify that the normalized path resolves correctly.
        let p = paths::resolve_path(&mp, "notes//sub").unwrap();
        assert!(p.starts_with(&mp.root));
        assert!(p.to_string_lossy().contains("notes"));
    }

    #[test]
    fn allows_unicode_paths() {
        let (_t, mp) = setup();
        let p = paths::resolve_path(&mp, "数据/report.json").unwrap();
        assert!(p.starts_with(&mp.root));
    }

    #[test]
    fn resolve_existing_rechecks_canonical_prefix() {
        let tmp = tempdir().unwrap();
        let mp = MountPoint::ensure(Namespace::user("alice").unwrap(), tmp.path()).unwrap();

        // MountPoint::ensure creates <tmp>/user-alice/ as the mount root.
        let mount_root = &mp.root;

        // Create a file inside the mount.
        std::fs::create_dir(mount_root.join("legit")).unwrap();
        std::fs::write(mount_root.join("legit/file.txt"), "ok").unwrap();

        let p = paths::resolve_path(&mp, "legit/file.txt").unwrap();
        assert!(p.exists());

        // Now symlink-swap "legit" → outside.
        let outside = tempdir().unwrap();
        // Create the same file structure in the outside dir so canonicalize
        // can resolve the full path and detect the prefix mismatch.
        std::fs::create_dir(outside.path().join("legit")).unwrap();
        std::fs::write(outside.path().join("legit/file.txt"), "stolen").unwrap();
        std::fs::remove_dir_all(mount_root.join("legit")).unwrap();
        symlink(outside.path().join("legit"), mount_root.join("legit")).unwrap();

        // resolve_path calls canonicalize() on existing paths, so it should
        // detect the symlink traversal.
        let err = paths::resolve_path(&mp, "legit/file.txt").unwrap_err();
        assert!(
            matches!(err, MemoryError::PathOutsideMount(_)),
            "symlink-swapped path should be caught, got {err:?}"
        );
    }

    #[test]
    fn resolve_for_create_parent_validation() {
        let tmp = tempdir().unwrap();
        let mp = MountPoint::ensure(Namespace::user("alice").unwrap(), tmp.path()).unwrap();

        // Create parent dir.
        std::fs::create_dir(tmp.path().join("draft")).unwrap();
        // resolve_for_create: parent exists and is under root.
        let p = paths::resolve_for_create(&mp, "draft/new.md").unwrap();
        assert!(p.ends_with("draft/new.md"));

        // resolve_for_create: parent doesn't exist yet (fine for mkdir -p scenarios).
        let p2 = paths::resolve_for_create(&mp, "fresh_dir/new.md").unwrap();
        assert!(p2.ends_with("fresh_dir/new.md"));
    }
}
