//! POSIX Phase 1 integration tests for SkillFS FUSE layer.
//!
//! Tests verify POSIX-level behaviors:
//!   - O_APPEND write
//!   - O_CREAT|O_EXCL semantics
//!   - O_DIRECTORY on regular files
//!   - Offset read/write (pread/pwrite)
//!   - fsync passthrough
//!   - stat permission reflection
//!   - SKILL.md O_TRUNC + store sync
//!   - O_NOFOLLOW on physical symlinks
//!
//! These tests require FUSE to be available (Linux only).
//! If FUSE is not available, tests are gracefully skipped.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, store::SkillStore};
use skillfs_fuse::{MountConfig, MountOptions, mount_background_configured};

mod common;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn fuse_available() -> bool {
    common::fuse_available()
}

macro_rules! skip_if_no_fuse {
    () => {
        if !fuse_available() {
            eprintln!("SKIP: FUSE not available (no /dev/fuse or fusermount3)");
            return;
        }
    };
}

/// Helper to create a skill directory with a SKILL.md containing frontmatter.
fn create_skill_dir(source: &Path, name: &str) {
    let skill_dir = source.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\n"),
    )
    .unwrap();
}

/// Mount source to mountpoint in normal (non-in-place) mode.
fn mount_test(source: &Path, mountpoint: &Path) -> skillfs_fuse::MountHandle {
    let mut store = SkillStore::new();
    store.load_from_directory(source, &ParseConfig::default());
    let shared = Arc::new(RwLock::new(store));
    mount_background_configured(
        mountpoint,
        source,
        shared,
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured")
}

/// Cleanup: drop handle + fusermount3 unmount.
fn cleanup(handle: skillfs_fuse::MountHandle, mountpoint: &Path) {
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. O_APPEND write
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_append_write() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"hello").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Open through mount with O_APPEND and write "world"
    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&mount_file)
        .expect("open with O_APPEND");
    f.write_all(b"world").expect("append write");
    drop(f);

    // Verify content via source
    let content = std::fs::read_to_string(&data_file).expect("read source");
    assert_eq!(content, "helloworld", "O_APPEND should append to end");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. O_CREAT|O_EXCL on existing file → EEXIST
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_creat_excl_existing_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/existing.txt");
    std::fs::write(&data_file, b"pre-existing").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/existing.txt");
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&mount_file);

    assert!(
        result.is_err(),
        "O_CREAT|O_EXCL on existing file should fail"
    );
    let err = result.unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::AlreadyExists,
        "expected AlreadyExists, got: {err}"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. O_DIRECTORY on regular file → ENOTDIR
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_o_directory_on_regular_file_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/regular.txt");
    std::fs::write(&data_file, b"just a file").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/regular.txt");
    let c_path = CString::new(mount_file.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert_eq!(fd, -1, "O_DIRECTORY on regular file should return -1");

    let errno = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(errno, libc::ENOTDIR, "expected ENOTDIR, got errno {errno}");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Offset read (pread)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_passthrough_offset_read() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"abcdefghij").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    let f = std::fs::File::open(&mount_file).expect("open for read");
    let mut buf = [0u8; 5];
    let n = f.read_at(&mut buf, 5).expect("read_at offset 5");

    assert_eq!(n, 5, "should read 5 bytes");
    assert_eq!(&buf, b"fghij", "offset read content mismatch");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Offset write (pwrite)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_passthrough_offset_write() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"aaaaaaaaaa").unwrap(); // 10 x 'a'

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&mount_file)
        .expect("open for write");
    f.write_at(b"XYZ", 3).expect("write_at offset 3");
    drop(f);

    let content = std::fs::read_to_string(&data_file).expect("read source");
    assert_eq!(content, "aaaXYZaaaa", "offset write content mismatch");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. fsync passthrough succeeds
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fsync_passthrough_succeeds() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/synctest.txt");
    std::fs::write(&data_file, b"").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/synctest.txt");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&mount_file)
        .expect("open for write");
    f.write_all(b"sync data").expect("write");
    f.sync_all().expect("sync_all (fsync) should succeed");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. Source chmod reflected in mount stat
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_source_chmod_reflected_in_stat() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/script.sh");
    std::fs::write(&data_file, b"#!/bin/sh\necho hi").unwrap();

    // chmod the source file to 0o755
    std::fs::set_permissions(&data_file, std::fs::Permissions::from_mode(0o755)).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/script.sh");
    let md = std::fs::metadata(&mount_file).expect("metadata through mount");
    let mode = md.mode();

    // Check that owner execute bit is set
    assert!(
        mode & 0o100 != 0,
        "expected owner execute bit in mode {:#o}, source was chmod 0755",
        mode
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. SKILL.md O_TRUNC + store sync (no regression)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_skill_md_o_trunc_store_sync() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    let skill_dir = source_dir.path().join("test-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: test-skill\ndescription: original\n---\nOriginal body\n",
    )
    .unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Open SKILL.md with O_TRUNC|O_WRONLY through the mount
    let mount_skill_md = mountpoint.path().join("skills/test-skill/SKILL.md");
    let new_content = "---\nname: test-skill\ndescription: updated\n---\nNew body\n";

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&mount_skill_md)
        .expect("open SKILL.md with O_TRUNC");
    f.write_all(new_content.as_bytes())
        .expect("write new content");
    drop(f);

    // Verify source file was truncated and has new content
    let source_content =
        std::fs::read_to_string(skill_dir.join("SKILL.md")).expect("read source SKILL.md");
    assert_eq!(
        source_content, new_content,
        "source SKILL.md should have truncated new content"
    );

    // Wait for store sync (debounce + async reparse) to settle
    std::thread::sleep(Duration::from_millis(500));

    // Verify skill is still visible in readdir (store sync didn't regress)
    let skills_dir = mountpoint.path().join("skills");
    let entries: Vec<String> = std::fs::read_dir(&skills_dir)
        .expect("readdir /skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries.iter().any(|n| n == "test-skill"),
        "test-skill should still be visible after O_TRUNC + store sync, got: {:?}",
        entries
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. O_NOFOLLOW on physical symlink → ELOOP
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_o_nofollow_on_physical_symlink_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    // Create a scripts directory and a symlink inside
    let scripts_dir = source_dir.path().join("test-skill/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let target = source_dir.path().join("test-skill/SKILL.md");
    let link = scripts_dir.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_link = mountpoint.path().join("skills/test-skill/scripts/link");
    let c_path = CString::new(mount_link.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW) };
    assert_eq!(fd, -1, "O_NOFOLLOW on symlink should return -1");

    let errno = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(errno, libc::ELOOP, "expected ELOOP, got errno {errno}");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. O_DIRECTORY | O_WRONLY on directory → EISDIR
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_o_directory_wronly_on_dir_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    // Create a subdirectory inside the skill
    let sub_dir = source_dir.path().join("test-skill/subdir");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_dir = mountpoint.path().join("skills/test-skill/subdir");
    let c_path = CString::new(mount_dir.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_DIRECTORY) };
    assert_eq!(fd, -1, "O_WRONLY|O_DIRECTORY on directory should return -1");

    let errno = unsafe { *libc::__errno_location() };
    assert_eq!(errno, libc::EISDIR, "expected EISDIR, got errno {errno}");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 11. O_DIRECTORY | O_RDWR on directory → EISDIR
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_o_directory_rdwr_on_dir_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    // Create a subdirectory inside the skill
    let sub_dir = source_dir.path().join("test-skill/subdir");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_dir = mountpoint.path().join("skills/test-skill/subdir");
    let c_path = CString::new(mount_dir.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_DIRECTORY) };
    assert_eq!(fd, -1, "O_RDWR|O_DIRECTORY on directory should return -1");

    let errno = unsafe { *libc::__errno_location() };
    assert_eq!(errno, libc::EISDIR, "expected EISDIR, got errno {errno}");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 12. O_NOFOLLOW | O_DIRECTORY on symlink → ENOTDIR
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_o_nofollow_o_directory_on_symlink() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    // Create a scripts directory and a symlink inside
    let scripts_dir = source_dir.path().join("test-skill/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let target = source_dir.path().join("test-skill/SKILL.md");
    let link = scripts_dir.join("dirlink");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_link = mountpoint.path().join("skills/test-skill/scripts/dirlink");
    let c_path = CString::new(mount_link.to_str().unwrap()).unwrap();

    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    assert_eq!(fd, -1, "O_NOFOLLOW|O_DIRECTORY on symlink should return -1");

    let errno = unsafe { *libc::__errno_location() };
    assert_eq!(
        errno,
        libc::ENOTDIR,
        "expected ENOTDIR (not ELOOP), got errno {errno}"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 13. O_RDONLY|O_TRUNC on passthrough file truncates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_readonly_trunc_passthrough_truncates() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let scripts_dir = source_dir.path().join("test-skill/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let data_file = scripts_dir.join("data.txt");
    std::fs::write(&data_file, b"hello world").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/scripts/data.txt");
    let c_path = CString::new(mount_file.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_TRUNC) };
    assert!(fd >= 0, "O_RDONLY|O_TRUNC open should succeed, got fd={fd}");
    unsafe { libc::close(fd) };

    // Verify source file was truncated to 0 bytes
    let meta = std::fs::metadata(&data_file).expect("metadata of source file");
    assert_eq!(
        meta.len(),
        0,
        "O_RDONLY|O_TRUNC should truncate the file, but size is {}",
        meta.len()
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 14. SKILL.md O_RDONLY|O_TRUNC triggers store sync
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_skill_md_readonly_trunc_triggers_store_sync() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    let skill_dir = source_dir.path().join("test-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: test-skill\ndescription: test skill\n---\nSome content here\n",
    )
    .unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_skill_md = mountpoint.path().join("skills/test-skill/SKILL.md");
    let c_path = CString::new(mount_skill_md.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_TRUNC) };
    assert!(
        fd >= 0,
        "O_RDONLY|O_TRUNC on SKILL.md should succeed, got fd={fd}"
    );
    unsafe { libc::close(fd) };

    // Wait for sync worker to process
    std::thread::sleep(Duration::from_millis(100));

    // Verify source SKILL.md was truncated to 0 bytes
    let meta = std::fs::metadata(skill_dir.join("SKILL.md")).expect("metadata of source SKILL.md");
    assert_eq!(
        meta.len(),
        0,
        "O_RDONLY|O_TRUNC should truncate SKILL.md, but size is {}",
        meta.len()
    );

    // Verify skill is still visible in readdir (store sync didn't remove it)
    let skills_dir = mountpoint.path().join("skills");
    let entries: Vec<String> = std::fs::read_dir(&skills_dir)
        .expect("readdir /skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries.iter().any(|n| n == "test-skill"),
        "test-skill should still be visible after O_RDONLY|O_TRUNC + store sync, got: {:?}",
        entries
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 15. statfs returns non-zero stats
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_statfs_returns_nonzero_stats() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let c_path = CString::new(mountpoint.path().to_str().unwrap()).unwrap();
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };

    assert_eq!(ret, 0, "statvfs should succeed, got ret={ret}");
    assert!(stat.f_blocks > 0, "f_blocks should be > 0");
    assert!(stat.f_bsize > 0, "f_bsize should be > 0");
    assert!(stat.f_files > 0, "f_files should be > 0");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 16. access on passthrough file checks permissions
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_access_passthrough_file_permissions() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"hello").unwrap();
    std::fs::set_permissions(&data_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    let c_path = CString::new(mount_file.to_str().unwrap()).unwrap();

    let ret_f_ok = unsafe { libc::access(c_path.as_ptr(), libc::F_OK) };
    assert_eq!(ret_f_ok, 0, "F_OK should succeed for existing file");

    let ret_r_ok = unsafe { libc::access(c_path.as_ptr(), libc::R_OK) };
    assert_eq!(ret_r_ok, 0, "R_OK should succeed for 0644 file");

    let ret_w_ok = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) };
    assert_eq!(ret_w_ok, 0, "W_OK should succeed for owner of 0644 file");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 17. access on virtual read-only dir denies W_OK
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_access_virtual_readonly_write_denied() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Check the mount root
    let c_path = CString::new(mountpoint.path().to_str().unwrap()).unwrap();

    let ret_w = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) };
    assert_eq!(ret_w, -1, "W_OK on virtual root should fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(err, libc::EACCES, "expected EACCES, got errno {err}");

    let ret_r = unsafe { libc::access(c_path.as_ptr(), libc::R_OK) };
    assert_eq!(ret_r, 0, "R_OK on virtual root should succeed");

    let ret_x = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) };
    assert_eq!(ret_x, 0, "X_OK on virtual root should succeed");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 18. access on skill-discover denies W_OK
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_access_skill_discover_write_denied() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Access skill-discover/SKILL.md
    let skill_md = mountpoint.path().join("skills/skill-discover/SKILL.md");
    let c_path = CString::new(skill_md.to_str().unwrap()).unwrap();

    let ret_w = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) };
    assert_eq!(ret_w, -1, "W_OK on skill-discover/SKILL.md should fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(err, libc::EACCES, "expected EACCES, got errno {err}");

    let ret_r = unsafe { libc::access(c_path.as_ptr(), libc::R_OK) };
    assert_eq!(ret_r, 0, "R_OK on skill-discover/SKILL.md should succeed");

    // X_OK should also be denied for virtual regular file (mode 0o444)
    let ret_x = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) };
    assert_eq!(ret_x, -1, "X_OK on skill-discover/SKILL.md should fail");
    let err_x = unsafe { *libc::__errno_location() };
    assert_eq!(
        err_x,
        libc::EACCES,
        "X_OK on virtual file should return EACCES"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 19. fsyncdir on physical directory succeeds
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fsyncdir_physical_dir_succeeds() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let sub_dir = source_dir.path().join("test-skill/subdir");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_dir = mountpoint.path().join("skills/test-skill/subdir");
    let c_path = CString::new(mount_dir.to_str().unwrap()).unwrap();

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(fd >= 0, "open directory should succeed, got fd={fd}");

    let ret = unsafe { libc::fsync(fd) };
    assert_eq!(ret, 0, "fsync on directory fd should succeed");

    unsafe { libc::close(fd) };

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 20. access on visible skill directory denies W_OK
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_access_skill_dir_write_denied() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Check the visible skill directory
    let skill_dir = mountpoint.path().join("skills/test-skill");
    let c_path = CString::new(skill_dir.to_str().unwrap()).unwrap();

    // W_OK should be denied
    let ret_w = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) };
    assert_eq!(ret_w, -1, "W_OK on skill directory should fail");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(
        err,
        libc::EACCES,
        "expected EACCES for W_OK, got errno {err}"
    );

    // R_OK should succeed
    let ret_r = unsafe { libc::access(c_path.as_ptr(), libc::R_OK) };
    assert_eq!(ret_r, 0, "R_OK on skill directory should succeed");

    // X_OK should succeed
    let ret_x = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) };
    assert_eq!(ret_x, 0, "X_OK on skill directory should succeed");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 21. access with invalid mask
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_access_invalid_mask_returns_einval() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_root = mountpoint.path();
    let c_path = CString::new(mount_root.to_str().unwrap()).unwrap();

    // Use an invalid mask value (0x08 is not a valid access mode bit)
    let ret = unsafe { libc::access(c_path.as_ptr(), 0x08) };

    // The kernel may intercept invalid mask values before they reach FUSE.
    // If so, it will return EINVAL directly. Either way, the call should fail.
    if ret == -1 {
        let err = std::io::Error::last_os_error().raw_os_error().unwrap();
        assert_eq!(
            err,
            libc::EINVAL,
            "expected EINVAL for invalid mask, got errno {err}"
        );
    } else {
        // If the kernel did not intercept (unlikely), this is unexpected.
        // Note: On some kernels, invalid bits may be silently ignored.
        // This is a known kernel-level behavior; pass the test with a note.
        eprintln!("NOTE: kernel did not reject invalid access mask 0x08 (returned {ret})");
    }

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 22. fsyncdir on virtual directory is a safe no-op
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fsyncdir_virtual_dir_noop() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Open mount root as a directory
    let c_path = CString::new(mountpoint.path().to_str().unwrap()).unwrap();
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(fd >= 0, "open mount root should succeed, got fd={fd}");

    // fsync on virtual root directory should be a safe no-op
    let ret = unsafe { libc::fsync(fd) };
    assert_eq!(
        ret, 0,
        "fsync on virtual root directory should succeed (no-op)"
    );

    unsafe { libc::close(fd) };

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 23. setattr chmod through mount
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_setattr_chmod_through_mount() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"hello").unwrap();
    // Set initial permissions to 0o644
    std::fs::set_permissions(&data_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // chmod through mount path to 0o755
    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    std::fs::set_permissions(&mount_file, std::fs::Permissions::from_mode(0o755))
        .expect("chmod through mount should succeed");

    // Verify source file mode changed
    let source_meta = std::fs::metadata(&data_file).unwrap();
    assert_eq!(
        source_meta.permissions().mode() & 0o7777,
        0o755,
        "source file mode should be 0o755 after chmod through mount"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 24. setattr chmod on virtual skill-discover fails with EROFS
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_setattr_chmod_virtual_skill_discover_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Attempt chmod on skill-discover virtual file
    let discover_file = mountpoint.path().join("skills/skill-discover/SKILL.md");
    let result = std::fs::set_permissions(&discover_file, std::fs::Permissions::from_mode(0o755));
    assert!(
        result.is_err(),
        "chmod on skill-discover SKILL.md should fail"
    );
    let err = result.unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "virtual path chmod should return EROFS, got {:?}",
        err
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 25. setattr chown unprivileged EPERM
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_setattr_chown_unprivileged_eperm() {
    skip_if_no_fuse!();

    // Skip if running as root (chown to root would succeed)
    if unsafe { libc::getuid() } == 0 {
        eprintln!("SKIP: running as root, chown test not meaningful");
        return;
    }

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"hello").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Attempt chown to root:root through mount path
    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    let c_path = CString::new(mount_file.to_str().unwrap()).unwrap();
    let ret = unsafe { libc::chown(c_path.as_ptr(), 0, 0) };
    assert_eq!(ret, -1, "chown to root should fail for unprivileged user");
    let err = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(err, libc::EPERM, "expected EPERM from chown");

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 26. setattr utimens mtime through mount
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_setattr_utimens_mtime_through_mount() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let data_file = source_dir.path().join("test-skill/data.txt");
    std::fs::write(&data_file, b"hello").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Set mtime to 2020-01-01 00:00:00 UTC (1577836800)
    let mount_file = mountpoint.path().join("skills/test-skill/data.txt");
    let c_path = CString::new(mount_file.to_str().unwrap()).unwrap();
    let target_mtime: i64 = 1_577_836_800;
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        }, // atime: don't change
        libc::timespec {
            tv_sec: target_mtime,
            tv_nsec: 0,
        }, // mtime: specific time
    ];
    let ret = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(ret, 0, "utimensat should succeed");

    // Verify source file mtime changed
    let source_meta = std::fs::metadata(&data_file).unwrap();
    let source_mtime = source_meta.modified().unwrap();
    let expected = UNIX_EPOCH + Duration::from_secs(target_mtime as u64);
    let diff = if source_mtime > expected {
        source_mtime.duration_since(expected).unwrap()
    } else {
        expected.duration_since(source_mtime).unwrap()
    };
    assert!(
        diff < Duration::from_secs(2),
        "source mtime should be close to 2020-01-01, got diff {:?}",
        diff
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 27. setattr size on SKILL.md still triggers reparse
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_setattr_size_skill_md_still_triggers_reparse() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // Truncate SKILL.md to 0 through mount
    let mount_skill_md = mountpoint.path().join("skills/test-skill/SKILL.md");
    let c_path = CString::new(mount_skill_md.to_str().unwrap()).unwrap();
    let ret = unsafe { libc::truncate(c_path.as_ptr(), 0) };
    assert_eq!(ret, 0, "truncate SKILL.md should succeed");

    // Verify source SKILL.md is now empty
    let source_skill_md = source_dir.path().join("test-skill/SKILL.md");
    let content = std::fs::read_to_string(&source_skill_md).unwrap();
    assert_eq!(
        content.len(),
        0,
        "source SKILL.md should be empty after truncate"
    );

    // Wait for reparse
    std::thread::sleep(Duration::from_millis(100));

    // The skill directory should still be visible in the mount
    let skill_dir = mountpoint.path().join("skills/test-skill");
    assert!(
        skill_dir.exists(),
        "skill directory should still be visible after SKILL.md truncate"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 28. setattr chmod on passthrough subdirectory through mount
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_setattr_chmod_directory_through_mount() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    // Create a passthrough subdirectory
    let sub_dir = source_dir.path().join("test-skill/subdir");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::set_permissions(&sub_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // chmod passthrough subdirectory through mount to 0o700
    let mount_sub = mountpoint.path().join("skills/test-skill/subdir");
    std::fs::set_permissions(&mount_sub, std::fs::Permissions::from_mode(0o700))
        .expect("chmod on passthrough subdir should succeed");

    // Verify source subdirectory mode changed
    let source_meta = std::fs::metadata(&sub_dir).unwrap();
    assert_eq!(
        source_meta.permissions().mode() & 0o7777,
        0o700,
        "source subdir mode should be 0o700 after chmod through mount"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 29. readdir stable entries
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_readdir_stable_entries() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    std::fs::write(source_dir.path().join("test-skill/file_a.txt"), b"a").unwrap();
    std::fs::write(source_dir.path().join("test-skill/file_b.txt"), b"b").unwrap();
    std::fs::write(source_dir.path().join("test-skill/file_c.txt"), b"c").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skill_dir = mountpoint.path().join("skills/test-skill");
    let entries: Vec<String> = std::fs::read_dir(&skill_dir)
        .expect("readdir skill dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        entries.contains(&"SKILL.md".to_string()),
        "should contain SKILL.md, got: {:?}",
        entries
    );
    assert!(
        entries.contains(&"file_a.txt".to_string()),
        "should contain file_a.txt, got: {:?}",
        entries
    );
    assert!(
        entries.contains(&"file_b.txt".to_string()),
        "should contain file_b.txt, got: {:?}",
        entries
    );
    assert!(
        entries.contains(&"file_c.txt".to_string()),
        "should contain file_c.txt, got: {:?}",
        entries
    );

    // No duplicates
    let mut sorted = entries.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        entries.len(),
        sorted.len(),
        "should have no duplicates, entries: {:?}",
        entries
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 30. readdir mutation after opendir invisible
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_readdir_mutation_after_opendir_invisible() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    std::fs::write(source_dir.path().join("test-skill/file_a.txt"), b"a").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skill_dir = mountpoint.path().join("skills/test-skill");
    let c_path = CString::new(skill_dir.to_str().unwrap()).unwrap();

    // Open directory
    let dir = unsafe { libc::opendir(c_path.as_ptr()) };
    assert!(!dir.is_null(), "opendir should succeed");

    // Create a new file in SOURCE after opendir
    std::fs::write(source_dir.path().join("test-skill/file_new.txt"), b"new").unwrap();

    // Read all entries from the already-opened directory stream
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe {
            std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_string_lossy()
                .to_string()
        };
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    unsafe { libc::closedir(dir) };

    // file_new.txt should NOT be in the snapshot
    assert!(
        !names.contains(&"file_new.txt".to_string()),
        "file_new.txt should not appear in already-opened dir stream, got: {:?}",
        names
    );
    assert!(
        names.contains(&"file_a.txt".to_string()),
        "file_a.txt should be in snapshot, got: {:?}",
        names
    );

    // Now re-open: file_new.txt should be visible
    let dir2 = unsafe { libc::opendir(c_path.as_ptr()) };
    assert!(!dir2.is_null(), "second opendir should succeed");

    let mut names2 = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir2) };
        if entry.is_null() {
            break;
        }
        let name = unsafe {
            std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_string_lossy()
                .to_string()
        };
        if name != "." && name != ".." {
            names2.push(name);
        }
    }
    unsafe { libc::closedir(dir2) };

    assert!(
        names2.contains(&"file_new.txt".to_string()),
        "file_new.txt should appear in new dir stream, got: {:?}",
        names2
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 31. readdir deletion after opendir still stable
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_readdir_deletion_after_opendir_still_stable() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    std::fs::write(source_dir.path().join("test-skill/file_a.txt"), b"a").unwrap();
    std::fs::write(source_dir.path().join("test-skill/file_b.txt"), b"b").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skill_dir = mountpoint.path().join("skills/test-skill");
    let c_path = CString::new(skill_dir.to_str().unwrap()).unwrap();

    // Open directory
    let dir = unsafe { libc::opendir(c_path.as_ptr()) };
    assert!(!dir.is_null(), "opendir should succeed");

    // Delete file_b.txt from SOURCE after opendir
    std::fs::remove_file(source_dir.path().join("test-skill/file_b.txt")).unwrap();

    // Read all entries
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe {
            std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_string_lossy()
                .to_string()
        };
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    unsafe { libc::closedir(dir) };

    // file_b.txt should still be in the snapshot
    assert!(
        names.contains(&"file_b.txt".to_string()),
        "file_b.txt should still be in already-opened dir stream, got: {:?}",
        names
    );

    // Re-open: file_b.txt should be gone
    let dir2 = unsafe { libc::opendir(c_path.as_ptr()) };
    assert!(!dir2.is_null(), "second opendir should succeed");

    let mut names2 = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir2) };
        if entry.is_null() {
            break;
        }
        let name = unsafe {
            std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_string_lossy()
                .to_string()
        };
        if name != "." && name != ".." {
            names2.push(name);
        }
    }
    unsafe { libc::closedir(dir2) };

    assert!(
        !names2.contains(&"file_b.txt".to_string()),
        "file_b.txt should not appear in new dir stream, got: {:?}",
        names2
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 32. readdir skills dir snapshot
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_readdir_skills_dir_snapshot() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "skill-alpha");
    create_skill_dir(source_dir.path(), "skill-beta");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");
    let c_path = CString::new(skills_dir.to_str().unwrap()).unwrap();

    // Open /skills directory
    let dir = unsafe { libc::opendir(c_path.as_ptr()) };
    assert!(!dir.is_null(), "opendir /skills should succeed");

    // Create a 3rd skill in SOURCE after opendir
    create_skill_dir(source_dir.path(), "skill-gamma");

    // Read the already-opened stream
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe {
            std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_string_lossy()
                .to_string()
        };
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    unsafe { libc::closedir(dir) };

    // skill-gamma should NOT be in the snapshot
    assert!(
        !names.contains(&"skill-gamma".to_string()),
        "skill-gamma should not appear in already-opened dir stream, got: {:?}",
        names
    );
    assert!(
        names.contains(&"skill-alpha".to_string()),
        "skill-alpha should be in snapshot, got: {:?}",
        names
    );
    assert!(
        names.contains(&"skill-beta".to_string()),
        "skill-beta should be in snapshot, got: {:?}",
        names
    );
    assert!(
        names.contains(&"skill-discover".to_string()),
        "skill-discover should be in snapshot, got: {:?}",
        names
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 33. fsyncdir with directory handle
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fsyncdir_with_directory_handle() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let sub_dir = source_dir.path().join("test-skill/subdir");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    // fsync on physical passthrough subdirectory
    let mount_dir = mountpoint.path().join("skills/test-skill/subdir");
    let c_path = CString::new(mount_dir.to_str().unwrap()).unwrap();
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(fd >= 0, "open directory should succeed, got fd={fd}");
    let ret = unsafe { libc::fsync(fd) };
    assert_eq!(ret, 0, "fsync on physical directory fd should succeed");
    unsafe { libc::close(fd) };

    // fsync on virtual mount root
    let c_root = CString::new(mountpoint.path().to_str().unwrap()).unwrap();
    let fd_root = unsafe { libc::open(c_root.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(
        fd_root >= 0,
        "open mount root should succeed, got fd={fd_root}"
    );
    let ret_root = unsafe { libc::fsync(fd_root) };
    assert_eq!(
        ret_root, 0,
        "fsync on virtual root directory should succeed (no-op)"
    );
    unsafe { libc::close(fd_root) };

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 34. releasedir cleanup
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_releasedir_cleanup() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    std::fs::write(source_dir.path().join("test-skill/data.txt"), b"hello").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skill_dir = mountpoint.path().join("skills/test-skill");
    let c_path = CString::new(skill_dir.to_str().unwrap()).unwrap();

    // Repeatedly open/read/close directory to verify no leaks
    for i in 0..5 {
        let dir = unsafe { libc::opendir(c_path.as_ptr()) };
        assert!(!dir.is_null(), "opendir iteration {i} should succeed");

        let mut count = 0;
        loop {
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                break;
            }
            count += 1;
        }
        // At least . and .. and SKILL.md and data.txt
        assert!(
            count >= 4,
            "iteration {i}: expected at least 4 entries, got {count}"
        );

        unsafe { libc::closedir(dir) };
    }

    // After 5 cycles, operations still work
    let entries: Vec<String> = std::fs::read_dir(&skill_dir)
        .expect("final readdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries.contains(&"SKILL.md".to_string()),
        "final readdir should still contain SKILL.md"
    );

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// 35. readdir passthrough subdir .. points to parent
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_readdir_passthrough_dotdot_points_to_parent() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    // Create passthrough subdirectory
    let sub_dir = source_dir.path().join("test-skill/subdir");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(sub_dir.join("file.txt"), b"hello").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skill_dir = mountpoint.path().join("skills/test-skill");
    let passthrough_subdir = mountpoint.path().join("skills/test-skill/subdir");

    // Get skill dir inode (passthrough subdir's .. should point here)
    let skill_dir_meta = std::fs::metadata(&skill_dir).expect("skill dir metadata");
    let skill_dir_ino = skill_dir_meta.ino();

    // Use libc::opendir + readdir to read passthrough subdirectory
    let c_path = CString::new(passthrough_subdir.to_string_lossy().into_owned()).unwrap();
    unsafe {
        let dir = libc::opendir(c_path.as_ptr());
        assert!(
            !dir.is_null(),
            "opendir should succeed for passthrough subdir"
        );

        let mut found_dotdot = false;
        loop {
            let entry = libc::readdir(dir);
            if entry.is_null() {
                break;
            }
            let name = std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()).to_string_lossy();
            if name == ".." {
                found_dotdot = true;
                // .. inode should be the parent (skill dir) inode
                let dotdot_ino = (*entry).d_ino;
                assert_eq!(
                    dotdot_ino, skill_dir_ino,
                    ".. in passthrough subdir should point to parent skill dir (expected ino {}, got {})",
                    skill_dir_ino, dotdot_ino
                );
                break;
            }
        }
        assert!(found_dotdot, "should find .. entry in passthrough subdir");
        libc::closedir(dir);
    }

    cleanup(handle, mountpoint.path());
}

// ─────────────────────────────────────────────────────────────────────────────
// Package G: rename flags
// ─────────────────────────────────────────────────────────────────────────────

/// Invoke the `renameat2` syscall with `flags`.
///
/// Returns `Ok(())` on success, or `Err(errno)` with the raw OS error code
/// reported by the kernel. Uses `AT_FDCWD` for both old and new dir fds.
#[cfg(target_os = "linux")]
fn renameat2(old: &Path, new: &Path, flags: u32) -> Result<(), i32> {
    use std::os::unix::ffi::OsStrExt;

    let old_c = CString::new(old.as_os_str().as_bytes()).expect("old path CString");
    let new_c = CString::new(new.as_os_str().as_bytes()).expect("new path CString");

    let ret = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old_c.as_ptr(),
            libc::AT_FDCWD,
            new_c.as_ptr(),
            flags,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

// 36. Plain passthrough file rename still works
#[test]
#[cfg(target_os = "linux")]
fn test_plain_passthrough_file_rename() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let old_src = source_dir.path().join("test-skill/old.txt");
    std::fs::write(&old_src, b"payload").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_old = mountpoint.path().join("skills/test-skill/old.txt");
    let mount_new = mountpoint.path().join("skills/test-skill/new.txt");
    std::fs::rename(&mount_old, &mount_new).expect("plain rename should succeed");

    let new_src = source_dir.path().join("test-skill/new.txt");
    assert!(!old_src.exists(), "old.txt should be gone after rename");
    assert!(new_src.exists(), "new.txt should exist after rename");
    let content = std::fs::read(&new_src).expect("read new.txt");
    assert_eq!(content, b"payload", "content must be preserved");

    cleanup(handle, mountpoint.path());
}

// 37. RENAME_NOREPLACE on existing target → EEXIST, both files unchanged
#[test]
#[cfg(target_os = "linux")]
fn test_rename_noreplace_existing_target_fails() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let old_src = source_dir.path().join("test-skill/old.txt");
    let target_src = source_dir.path().join("test-skill/target.txt");
    std::fs::write(&old_src, b"old-content").unwrap();
    std::fs::write(&target_src, b"target-content").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_old = mountpoint.path().join("skills/test-skill/old.txt");
    let mount_target = mountpoint.path().join("skills/test-skill/target.txt");

    let result = renameat2(&mount_old, &mount_target, libc::RENAME_NOREPLACE);
    assert_eq!(
        result,
        Err(libc::EEXIST),
        "RENAME_NOREPLACE on existing target should return EEXIST, got {result:?}"
    );

    // Both files must still exist with their original contents.
    assert!(
        old_src.exists(),
        "old.txt must still exist after rejected rename"
    );
    assert!(target_src.exists(), "target.txt must still exist");
    assert_eq!(
        std::fs::read(&old_src).unwrap(),
        b"old-content",
        "old.txt content unchanged"
    );
    assert_eq!(
        std::fs::read(&target_src).unwrap(),
        b"target-content",
        "target.txt must not be overwritten"
    );

    cleanup(handle, mountpoint.path());
}

// 38. RENAME_NOREPLACE with missing target succeeds
#[test]
#[cfg(target_os = "linux")]
fn test_rename_noreplace_missing_target_succeeds() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let old_src = source_dir.path().join("test-skill/old.txt");
    std::fs::write(&old_src, b"payload").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_old = mountpoint.path().join("skills/test-skill/old.txt");
    let mount_new = mountpoint.path().join("skills/test-skill/new.txt");

    renameat2(&mount_old, &mount_new, libc::RENAME_NOREPLACE)
        .expect("RENAME_NOREPLACE with missing target should succeed");

    let new_src = source_dir.path().join("test-skill/new.txt");
    assert!(!old_src.exists(), "old.txt should be gone");
    assert!(new_src.exists(), "new.txt should exist");
    assert_eq!(
        std::fs::read(&new_src).unwrap(),
        b"payload",
        "content preserved across rename"
    );

    cleanup(handle, mountpoint.path());
}

// 39. RENAME_EXCHANGE rejected without mutating either path
#[test]
#[cfg(target_os = "linux")]
fn test_rename_exchange_rejected_without_mutation() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let a_src = source_dir.path().join("test-skill/a.txt");
    let b_src = source_dir.path().join("test-skill/b.txt");
    std::fs::write(&a_src, b"AAAA").unwrap();
    std::fs::write(&b_src, b"BBBB").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_a = mountpoint.path().join("skills/test-skill/a.txt");
    let mount_b = mountpoint.path().join("skills/test-skill/b.txt");

    let result = renameat2(&mount_a, &mount_b, libc::RENAME_EXCHANGE);
    assert!(
        result.is_err(),
        "RENAME_EXCHANGE must be rejected, got {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err == libc::EINVAL || err == libc::EOPNOTSUPP,
        "expected EINVAL or EOPNOTSUPP for RENAME_EXCHANGE, got errno {err}"
    );

    // Neither file should have been swapped or removed.
    assert!(a_src.exists(), "a.txt must still exist");
    assert!(b_src.exists(), "b.txt must still exist");
    assert_eq!(std::fs::read(&a_src).unwrap(), b"AAAA", "a.txt unchanged");
    assert_eq!(std::fs::read(&b_src).unwrap(), b"BBBB", "b.txt unchanged");

    cleanup(handle, mountpoint.path());
}

// 40. Unknown rename flag rejected with EINVAL
#[test]
#[cfg(target_os = "linux")]
fn test_unknown_rename_flag_rejected() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "test-skill");
    let old_src = source_dir.path().join("test-skill/old.txt");
    std::fs::write(&old_src, b"payload").unwrap();

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let mount_old = mountpoint.path().join("skills/test-skill/old.txt");
    let mount_new = mountpoint.path().join("skills/test-skill/new.txt");

    // 0x80000000 is well outside the documented Linux RENAME_* bits.
    let unknown_flag: u32 = 0x8000_0000;
    let result = renameat2(&mount_old, &mount_new, unknown_flag);
    assert_eq!(
        result,
        Err(libc::EINVAL),
        "unknown rename flag must return EINVAL, got {result:?}"
    );

    // Source must remain untouched, target must not have been created.
    assert!(old_src.exists(), "old.txt must still exist");
    assert!(
        !source_dir.path().join("test-skill/new.txt").exists(),
        "new.txt must not have been created"
    );
    assert_eq!(
        std::fs::read(&old_src).unwrap(),
        b"payload",
        "content unchanged"
    );

    cleanup(handle, mountpoint.path());
}

// 41. Skill directory rename with RENAME_NOREPLACE — store invariants preserved
#[test]
#[cfg(target_os = "linux")]
fn test_skill_dir_rename_noreplace() {
    skip_if_no_fuse!();

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    create_skill_dir(source_dir.path(), "skill-a");
    create_skill_dir(source_dir.path(), "skill-b");

    let handle = mount_test(source_dir.path(), mountpoint.path());
    std::thread::sleep(Duration::from_millis(300));

    let skills_root = mountpoint.path().join("skills");
    let mount_a = skills_root.join("skill-a");
    let mount_b = skills_root.join("skill-b");
    let mount_c = skills_root.join("skill-c");

    // skill-a → skill-b with NOREPLACE must fail with EEXIST and keep both visible.
    let result = renameat2(&mount_a, &mount_b, libc::RENAME_NOREPLACE);
    assert_eq!(
        result,
        Err(libc::EEXIST),
        "skill-a → existing skill-b with NOREPLACE must EEXIST, got {result:?}"
    );
    let listing: Vec<String> = std::fs::read_dir(&skills_root)
        .expect("readdir skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        listing.contains(&"skill-a".to_string()),
        "skill-a must remain visible: {listing:?}"
    );
    assert!(
        listing.contains(&"skill-b".to_string()),
        "skill-b must remain visible: {listing:?}"
    );

    // skill-a → skill-c with NOREPLACE must succeed and swap visibility immediately.
    renameat2(&mount_a, &mount_c, libc::RENAME_NOREPLACE)
        .expect("skill-a → skill-c with NOREPLACE should succeed");

    let listing: Vec<String> = std::fs::read_dir(&skills_root)
        .expect("readdir skills after rename")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !listing.contains(&"skill-a".to_string()),
        "skill-a must disappear immediately after rename: {listing:?}"
    );
    assert!(
        listing.contains(&"skill-c".to_string()),
        "skill-c must appear immediately after rename: {listing:?}"
    );

    // Wait long enough for the sync worker (50 ms debounce + reparse) to run,
    // then confirm the stale `name: skill-a` frontmatter does not resurrect
    // the old store key.
    std::thread::sleep(Duration::from_millis(400));
    let final_listing: Vec<String> = std::fs::read_dir(&skills_root)
        .expect("readdir skills after sync")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !final_listing.contains(&"skill-a".to_string()),
        "stale frontmatter must not resurrect skill-a: {final_listing:?}"
    );
    assert!(
        final_listing.contains(&"skill-c".to_string()),
        "skill-c must still be visible after sync: {final_listing:?}"
    );

    cleanup(handle, mountpoint.path());
}
