//! `FileAttr` / `FileType` conversion helpers used by `getattr`,
//! `readdir`, and the *at-fallback paths. Centralized so symlink, FIFO,
//! socket, and device identity is reported consistently across all
//! callbacks regardless of which stat source (`libc::stat`,
//! `std::fs::Metadata`, or `std::fs::DirEntry`) produced the input.

use std::os::unix::fs::MetadataExt;
use std::time::{SystemTime, UNIX_EPOCH};

use fuser::{FileAttr, FileType};

/// Convert a libc::stat to a fuser::FileAttr. Mirrors `file_attr_from_metadata`
/// for paths that were stat'd via `fstatat` instead of `symlink_metadata`.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn file_attr_from_stat(st: &libc::stat) -> FileAttr {
    let mode = st.st_mode;
    let kind = match mode & libc::S_IFMT {
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFSOCK => FileType::Socket,
        _ => FileType::RegularFile,
    };
    FileAttr {
        ino: 0,
        size: st.st_size as u64,
        blocks: st.st_blocks as u64,
        atime: system_time_from_secs(st.st_atime as i64, st.st_atime_nsec as i64),
        mtime: system_time_from_secs(st.st_mtime as i64, st.st_mtime_nsec as i64),
        ctime: system_time_from_secs(st.st_ctime as i64, st.st_ctime_nsec as i64),
        crtime: UNIX_EPOCH,
        kind,
        perm: (mode & 0o7777) as u16,
        nlink: st.st_nlink as u32,
        uid: st.st_uid,
        gid: st.st_gid,
        rdev: st.st_rdev as u32,
        flags: 0,
        blksize: st.st_blksize as u32,
    }
}

pub(crate) fn system_time_from_secs(secs: i64, nsecs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + std::time::Duration::new(secs as u64, nsecs as u32)
    } else {
        UNIX_EPOCH - std::time::Duration::new((-secs) as u64, 0)
    }
}

/// Convert std::fs::Metadata to FUSE FileAttr.
///
/// `kind` is derived from `file_type()` so that symlink identity is preserved
/// when the caller supplies metadata from `symlink_metadata()`. Callers that
/// want symlink-following semantics should pass metadata from `metadata()`
/// instead — that path will set `is_symlink()` to `false` because the kernel
/// has already resolved the target.
pub(crate) fn file_attr_from_metadata(meta: &std::fs::Metadata) -> FileAttr {
    let kind = filetype_from_mode(meta.mode());
    FileAttr {
        ino: 0,
        size: meta.len(),
        blocks: meta.blocks(),
        atime: system_time_from_secs(meta.atime(), meta.atime_nsec()),
        mtime: system_time_from_secs(meta.mtime(), meta.mtime_nsec()),
        ctime: system_time_from_secs(meta.ctime(), meta.ctime_nsec()),
        crtime: meta.created().unwrap_or(UNIX_EPOCH),
        kind,
        perm: (meta.mode() & 0o7777) as u16,
        nlink: meta.nlink() as u32,
        uid: meta.uid(),
        gid: meta.gid(),
        rdev: meta.rdev() as u32,
        flags: 0,
        blksize: meta.blksize() as u32,
    }
}

/// Project a `std::fs::DirEntry`'s file type into the FUSE `FileType` we
/// expose in directory listings. Preserves symlink, FIFO, socket, and
/// device identity so callers see the same kind they would over a native
/// passthrough mount.
pub(crate) fn dir_entry_file_type(entry: &std::fs::DirEntry) -> FileType {
    match entry.metadata() {
        Ok(meta) => filetype_from_mode(meta.mode()),
        // `metadata()` here is `lstat`-style on `DirEntry`; fall back to
        // the cheaper `file_type()` if it failed (e.g. EACCES on the leaf
        // inode) so we still surface symlink / dir identity.
        Err(_) => match entry.file_type() {
            Ok(t) if t.is_dir() => FileType::Directory,
            Ok(t) if t.is_symlink() => FileType::Symlink,
            _ => FileType::RegularFile,
        },
    }
}

/// Map a POSIX mode word's `S_IFMT` bits to the corresponding FUSE
/// [`FileType`]. Centralized so `lookup`, `readdir`, and `mknod`'s reply
/// all agree on how special files (FIFO, socket, block/char device) are
/// reported.
pub(crate) fn filetype_from_mode(mode: u32) -> FileType {
    match mode & libc::S_IFMT {
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFSOCK => FileType::Socket,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        _ => FileType::RegularFile,
    }
}
