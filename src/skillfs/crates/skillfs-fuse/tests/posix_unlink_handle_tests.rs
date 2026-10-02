//! T1 integration coverage for POSIX open-after-unlink semantics.
//!
//! pjdfstest `unlink/14.t` opens a file, `unlink`s the path while the fd is
//! still open, then exercises `fstat`/`pread`/`write` on that fd and expects
//! them to behave normally (`unlink` only removes the directory entry; the
//! inode and any open fds survive until last close). SkillFS used to fail
//! these subtests because:
//!
//!   * `unlink` tears down the inode → path mapping immediately, so a
//!     subsequent `getattr(ino, fh)` returned `ENOENT` from the path
//!     lookup even though the kernel still held an open FUSE fh.
//!   * `read`/`write` likewise dropped to `ENOENT` because they re-resolved
//!     the path on every request.
//!
//! T1 adds fallbacks that prefer the open `HandleEntry::file` when the path
//! mapping is gone. These tests exercise the three callbacks end-to-end.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;

use common::{MountFixture, create_skill_dir};

mod common;

fn open_passthrough_for(fx: &MountFixture, skill: &str, rel: &str, contents: &[u8]) {
    let source_rel = fx.source_skill_path(skill).join(rel);
    if let Some(parent) = source_rel.parent() {
        std::fs::create_dir_all(parent).expect("seed parent dir");
    }
    std::fs::write(&source_rel, contents).expect("seed passthrough file");
}

#[test]
fn test_fstat_after_unlink_via_handle() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness");
    });
    open_passthrough_for(&fx, "harness", "sandbox/keep", b"abc");

    let mount_path = fx.skill_path("harness").join("sandbox").join("keep");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(&mount_path)
        .expect("open before unlink");
    let before = file.metadata().expect("fstat before unlink");
    assert_eq!(before.nlink(), 1, "single link before unlink");
    assert_eq!(before.size(), 3, "size matches seed");

    std::fs::remove_file(&mount_path).expect("unlink through mount");

    // The path mapping is gone now. fstat on the still-open fd must succeed
    // and report `nlink == 0` (POSIX: link count drops after unlink).
    let after = file.metadata().expect("fstat after unlink");
    assert_eq!(after.nlink(), 0, "link count zero after unlink");
    assert_eq!(after.size(), 3, "size preserved on open fd");
}

#[test]
fn test_read_after_unlink_via_handle() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness");
    });
    let payload = b"Hello, World!";
    open_passthrough_for(&fx, "harness", "sandbox/payload", payload);

    let mount_path = fx.skill_path("harness").join("sandbox").join("payload");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .open(&mount_path)
        .expect("open before unlink");

    std::fs::remove_file(&mount_path).expect("unlink through mount");

    let mut buf = [0u8; 13];
    file.seek(SeekFrom::Start(0)).expect("seek back");
    file.read_exact(&mut buf).expect("read after unlink");
    assert_eq!(&buf, payload, "data readable after unlink");
}

#[test]
fn test_write_then_read_after_unlink_via_handle() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "harness");
    });
    // Empty file so we can write fresh data through the handle.
    open_passthrough_for(&fx, "harness", "sandbox/scratch", b"");

    let mount_path = fx.skill_path("harness").join("sandbox").join("scratch");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&mount_path)
        .expect("open RW before unlink");

    let payload = b"survives-unlink";
    file.write_all(payload).expect("write before unlink");
    std::fs::remove_file(&mount_path).expect("unlink through mount");

    // After unlink the kernel keeps using the same fh for our handle.
    // Both write and read on that handle must continue to work.
    file.seek(SeekFrom::Start(0)).expect("seek back");
    let mut buf = vec![0u8; payload.len()];
    file.read_exact(&mut buf).expect("read after unlink");
    assert_eq!(&buf, payload, "data round-trips through unlinked fd");

    file.write_all(b"+more").expect("write after unlink");
    let final_size = file.metadata().expect("fstat at end").size() as usize;
    assert_eq!(
        final_size,
        payload.len() + 5,
        "size grew via post-unlink write"
    );
}
