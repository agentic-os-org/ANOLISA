//! Integration tests: `rename` rejects file-to-inbox-candidate-slot moves.
//!
//! The inbox presents `/.skillfs-inbox/<candidate>` as a virtual
//! DIRECTORY facade over the physical `source/<candidate>` directory,
//! and the create side enforces "an inbox candidate must be a
//! directory" with `EISDIR` (`create_impl` refuses a fresh entry at the
//! slot before any physical resolution). `rename_impl` had no matching
//! arm: `mv /.skillfs-inbox/<demo>/note /.skillfs-inbox/fresh` passed
//! the same-namespace check and the physical rename materialized a
//! plain regular file at `source/<fresh>`. Inbox readdir then hides
//! that non-directory while direct lookup returns `ENOTDIR`, so the
//! moved file is no longer reachable through the mount — rename
//! bypassed the create-side rule. The gate mirrors the create-side
//! errno/audit contract.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

mod common;

use common::{MountFixture, create_skill_dir};

fn inbox_path(fx: &MountFixture, name: &str) -> std::path::PathBuf {
    fx.mountpoint().join(".skillfs-inbox").join(name)
}

/// `mv /.skillfs-inbox/<demo>/note /.skillfs-inbox/fresh` must be
/// refused with `EISDIR` — the same answer `create` gives at that slot
/// ("this name is a directory slot, not a regular-file slot") — and
/// nothing may be materialized on the source side. On the pre-fix main
/// the rename succeeded (ok) and physically materialized a plain
/// regular file at `source/fresh-candidate` (verified on a real FUSE
/// mount as part of the self-integration audit).
#[test]
fn rename_file_onto_fresh_inbox_candidate_slot_materializes_file() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP rename_file_onto_fresh_inbox_candidate_slot_materializes_file: FUSE not available"
        );
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "cand-a");
    });

    let src_file = inbox_path(&fx, "cand-a/notes.txt");
    std::fs::write(&src_file, b"payload\n").expect("create passthrough file through inbox");

    let slot = inbox_path(&fx, "fresh-candidate");
    let err = std::fs::rename(&src_file, &slot)
        .expect_err("rename of a file onto a fresh inbox candidate slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EISDIR),
        "rename onto /.skillfs-inbox/<fresh> must be EISDIR like create, got {err:?}"
    );

    // No plain regular file (or anything else) may appear at the slot,
    // and the moved file must survive untouched at its origin.
    assert!(
        !fx.source().join("fresh-candidate").exists(),
        "no entry may be materialized at the source candidate slot"
    );
    assert!(
        fx.source().join("cand-a/notes.txt").is_file(),
        "the source file must survive the rejected rename"
    );

    // Control: plain `create` at the same slot keeps answering EISDIR.
    let err = std::fs::write(&slot, b"payload\n")
        .expect_err("create at /.skillfs-inbox/<fresh> must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EISDIR),
        "create at a fresh inbox candidate slot must stay EISDIR, got {err:?}"
    );
}

/// The manifest twin: renaming a candidate's `SKILL.md` onto a fresh
/// candidate slot materializes a plain file at `source/<fresh>` on the
/// pre-fix main — same confusion, manifest-shaped.
#[test]
fn rename_manifest_onto_fresh_inbox_candidate_slot_materializes_file() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP rename_manifest_onto_fresh_inbox_candidate_slot_materializes_file: FUSE not available"
        );
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "cand-a");
    });

    let manifest = inbox_path(&fx, "cand-a/SKILL.md");
    assert!(manifest.is_file(), "manifest must be warm before the move");

    let slot = inbox_path(&fx, "fresh-candidate");
    let err = std::fs::rename(&manifest, &slot)
        .expect_err("rename of a manifest onto a fresh inbox candidate slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EISDIR),
        "rename of SKILL.md onto /.skillfs-inbox/<fresh> must be EISDIR, got {err:?}"
    );
    assert!(
        !fx.source().join("fresh-candidate").exists(),
        "no entry may be materialized at the source candidate slot"
    );
    assert!(
        fx.source().join("cand-a/SKILL.md").is_file(),
        "the manifest must survive the rejected rename"
    );
}

/// Control: the sanctioned whole-candidate directory rename
/// (`/.skillfs-inbox/<old>` → `/.skillfs-inbox/<new>`, the CLI install
/// flow) keeps working — the gate only rejects FILE-kind old sides.
#[test]
fn inbox_candidate_dir_rename_keeps_working() {
    if !common::fuse_available() {
        eprintln!("SKIP inbox_candidate_dir_rename_keeps_working: FUSE not available");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "cand-a");
    });

    std::fs::rename(inbox_path(&fx, "cand-a"), inbox_path(&fx, "cand-b"))
        .expect("whole-candidate directory rename through the inbox must keep working");
    assert!(
        fx.source().join("cand-b/SKILL.md").is_file(),
        "the candidate must physically land under the new name"
    );
    assert!(
        !fx.source().join("cand-a").exists(),
        "the old candidate directory must be gone"
    );
}

/// Control: an ordinary file rename INSIDE a candidate (file →
/// file-capable passthrough leaf) keeps working.
#[test]
fn inbox_internal_file_rename_keeps_working() {
    if !common::fuse_available() {
        eprintln!("SKIP inbox_internal_file_rename_keeps_working: FUSE not available");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "cand-a");
    });

    let src_file = inbox_path(&fx, "cand-a/notes.txt");
    std::fs::write(&src_file, b"payload\n").expect("create passthrough file through inbox");

    std::fs::rename(&src_file, inbox_path(&fx, "cand-a/notes-renamed.txt"))
        .expect("file rename inside a candidate must keep working");
    assert!(
        fx.source().join("cand-a/notes-renamed.txt").is_file(),
        "the renamed file must land on the source side"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Long-path cached-entry regression (review follow-up).
//
// The inbox file→slot gate must not fold a failed type probe into "not a
// plain file". A cached inbox dentry can sit deep enough that the source
// file's ABSOLUTE physical path exceeds PATH_MAX: `symlink_metadata` on
// that path answers ENAMETOOLONG, the pre-fix gate folded the error to
// false, the rename fell through to the parent-fd rename fallback and
// physically moved the file into the candidate slot — it then vanished
// from the inbox listing exactly like the short-path hole above (real
// FUSE repro with a 4201-char old path: rename=Ok, source/<fresh> was a
// regular file). The gate now types the source with the parent-fd +
// leaf `fstatat(AT_SYMLINK_NOFOLLOW)` pair and propagates probe errors,
// so the overlong cached entry is refused with EISDIR like any other
// file→slot move.
// ─────────────────────────────────────────────────────────────────────────────

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;

const PATH_MAX_LINUX: usize = 4096;

fn c_dir_open(path: &std::path::Path) -> std::io::Result<std::fs::File> {
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

/// `renameat(oldfd, oldleaf, AT_FDCWD, newpath)`: the old side is addressed
/// through the still-cached open directory fd (only the short leaf name
/// reaches the kernel), which is how the overlong cached entry stays
/// reachable after its absolute physical path crossed PATH_MAX.
fn renameat_via_dir_fd(
    old_dir: &std::fs::File,
    old_leaf: &str,
    new_abs: &std::path::Path,
) -> std::io::Result<()> {
    let c_old = CString::new(old_leaf.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let c_new = CString::new(new_abs.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let rc = unsafe {
        libc::renameat(
            old_dir.as_raw_fd(),
            c_old.as_ptr(),
            libc::AT_FDCWD,
            c_new.as_ptr(),
        )
    };
    if rc != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn fstatat_is_regular(dir: &std::fs::File, leaf: &str) -> std::io::Result<bool> {
    let c = CString::new(leaf.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), &mut st, 0) };
    if rc != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(st.st_mode & libc::S_IFREG != 0)
    }
}

#[test]
fn rename_cached_long_path_file_onto_inbox_slot_is_rejected() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP rename_cached_long_path_file_onto_inbox_slot_is_rejected: FUSE not available"
        );
        return;
    }

    // Anchor the SOURCE under a padded parent so the physical prefix is
    // ~200 bytes longer than the mountpoint prefix: the deep chain below
    // then crosses PATH_MAX on the physical side while every mount-side
    // syscall path (including the directory-move target) stays short
    // enough for the kernel to accept.
    let pad_root = tempfile::tempdir().expect("pad root tempdir");
    let pad = pad_root.path().join("p".repeat(200));
    std::fs::create_dir(&pad).expect("padded parent dir");

    // Chain geometry, computed from the real source path so the test is
    // robust to different TMPDIR layouts:
    //   * `source/demo/<chain>/sub` itself fits PATH_MAX → the gate's
    //     parent-fd probe (and the physical rename fallback) can open it
    //   * `source/demo/<chain>/sub/<200-char leaf>` crosses PATH_MAX
    //     → the pre-fix full-path `symlink_metadata` probe failed with
    //       ENAMETOOLONG
    //   * every mount-side path stays under PATH_MAX → the kernel
    //     accepts the userspace syscalls
    let mut deep_dir: Option<std::path::PathBuf> = None;
    let fx = MountFixture::normal_in(&pad, |src| {
        create_skill_dir(src, "demo");
        let component = "a".repeat(120);
        let mut current = src.join("demo");
        loop {
            let next = current.join(&component);
            let sub = next.join("sub");
            if sub.as_os_str().len() + 1 + 200 > PATH_MAX_LINUX {
                // `next/sub` fits but the leaf under it would cross
                // PATH_MAX: stop extending here.
                std::fs::create_dir(&next).expect("seed chain dir");
                deep_dir = Some(next);
                break;
            }
            std::fs::create_dir(&next).expect("seed chain dir");
            current = next;
        }
    });
    let deep = deep_dir.expect("seed loop must terminate with the deep dir");
    let sub_phys = deep.join("sub");
    assert!(
        sub_phys.as_os_str().len() + 1 + 200 > PATH_MAX_LINUX,
        "the leaf's full physical path must exceed PATH_MAX (sub={})",
        sub_phys.as_os_str().len()
    );
    assert!(
        sub_phys.as_os_str().len() < PATH_MAX_LINUX,
        "the moved directory itself must fit PATH_MAX (sub={})",
        sub_phys.as_os_str().len()
    );

    let inbox_root = fx.mountpoint().join(".skillfs-inbox");

    // 1. Cache the entries at a SHALLOW depth: create the directory and
    //    the 200-char regular file through the mount, then pin the
    //    directory with an open fd. Every lookup here has a short
    //    physical path, so the kernel dentries and the daemon inode
    //    mappings are established before anything goes deep.
    let leaf_name = "f".repeat(200);
    let sub_virt = inbox_root.join("demo/sub");
    std::fs::create_dir(&sub_virt).expect("create sub through the inbox");
    std::fs::write(sub_virt.join(&leaf_name), b"deep payload\n")
        .expect("create the 200-char leaf through the inbox");
    let dir_fd = c_dir_open(&sub_virt).expect("pin the sub directory fd");
    assert!(
        fstatat_is_regular(&dir_fd, &leaf_name).expect("sanity fstat of the cached leaf"),
        "the cached long-named entry must be a regular file"
    );

    // 2. Move the directory DEEP through the mount — the sanctioned
    //    inbox-internal directory rename (this doubles as the
    //    directory-move control: the gate must not restrict it). The
    //    pinned fd and the cached leaf dentry survive the move; the
    //    daemon's recursive path rewrite makes the leaf's physical path
    //    cross PATH_MAX while the kernel keeps addressing the entry as
    //    `dirfd + leaf`.
    let deep_virt = inbox_root.join(deep.strip_prefix(fx.source()).expect("deep under source"));
    assert!(
        deep_virt.as_os_str().len() + 4 < PATH_MAX_LINUX,
        "mount-side rename target must fit PATH_MAX (got {})",
        deep_virt.as_os_str().len() + 4
    );
    std::fs::rename(&sub_virt, deep_virt.join("sub"))
        .expect("sanctioned directory rename into the deep chain");
    assert!(
        sub_phys.is_dir(),
        "the directory must physically land at the deep location"
    );

    // 3. The probe: rename the cached overlong leaf onto a fresh inbox
    //    candidate slot, old side addressed through the pinned fd. The
    //    pre-fix gate folded the ENAMETOOLONG type probe to false and
    //    the parent-fd rename fallback then moved the file into the
    //    slot (rename=Ok, `source/<fresh>` a regular file, the entry
    //    gone from the inbox listing). The gate must answer EISDIR like
    //    every other file→slot move.
    let slot = inbox_path(&fx, "fresh-candidate");
    let err = renameat_via_dir_fd(&dir_fd, &leaf_name, &slot)
        .expect_err("long-path file onto inbox slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EISDIR),
        "cached long-path file onto /.skillfs-inbox/<fresh> must be EISDIR, got {err:?}"
    );

    // Nothing at the slot; the file survived untouched at its origin.
    assert!(
        !fx.source().join("fresh-candidate").exists(),
        "no entry may be materialized at the source candidate slot"
    );
    assert!(
        fstatat_is_regular(&dir_fd, &leaf_name).expect("origin fstat after the rejected rename"),
        "the long-named source file must survive the rejected rename"
    );
}
