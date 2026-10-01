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
        atime: fuser_wire_time(system_time_from_secs(
            st.st_atime as i64,
            st.st_atime_nsec as i64,
        )),
        mtime: fuser_wire_time(system_time_from_secs(
            st.st_mtime as i64,
            st.st_mtime_nsec as i64,
        )),
        ctime: fuser_wire_time(system_time_from_secs(
            st.st_ctime as i64,
            st.st_ctime_nsec as i64,
        )),
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

/// Re-express a pre-epoch instant as the time fuser's wire encoder turns
/// into the correct normalized timespec for the true instant.
///
/// fuser 0.15–0.18 `time_from_system_time` (`ll/reply.rs`) splits a
/// pre-epoch instant by truncating toward zero: -1.5 s becomes
/// `{sec: -1, nsec: 500_000_000}`, which the kernel reads as -0.5 s. The
/// FUSE timespec convention requires flooring (`{sec: -2,
/// nsec: 500_000_000}`). Feeding the encoder a time whose distance from the
/// epoch is `(S' + 1, 1e9 - N')` — one second further back with the
/// nanoseconds mirrored — makes its truncated pair coincide with the
/// normalized form of the true time. Whole seconds and post-epoch instants
/// pass through unchanged. Remove together with [`fuser_decoded_time`]
/// (write.rs) once the dependency ships fuser's fixed conversions (master,
/// post-0.18).
pub(crate) fn fuser_wire_time(t: SystemTime) -> SystemTime {
    match t.duration_since(UNIX_EPOCH) {
        Ok(_) => t,
        Err(e) if e.duration().subsec_nanos() == 0 => {
            // Whole second at the extreme edge (tv_sec: i64::MIN): fuser
            // 0.15's `-(as_secs() as i64)` negation overflows for distances
            // of 2^63 s, so clamp to the lowest encodable instant.
            if e.duration().as_secs() > i64::MAX as u64 {
                return UNIX_EPOCH - std::time::Duration::new(i64::MAX as u64, 0);
            }
            t
        }
        Err(e) => {
            let d = e.duration();
            // The compensated instant may fall below the representable
            // minimum for pathological stat values (tv_sec: i64::MIN with
            // nsec > 0); saturate instead of panicking the daemon.
            UNIX_EPOCH
                .checked_sub(std::time::Duration::new(
                    d.as_secs().saturating_add(1),
                    1_000_000_000 - d.subsec_nanos(),
                ))
                .unwrap_or_else(|| UNIX_EPOCH - std::time::Duration::new(i64::MAX as u64, 0))
        }
    }
}

pub(crate) fn system_time_from_secs(secs: i64, nsecs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + std::time::Duration::new(secs as u64, nsecs as u32)
    } else {
        // Pre-epoch timespec: Linux stores -1.5 s as
        // { tv_sec: -2, tv_nsec: 500_000_000 } — the exact shape
        // `setattr_impl` normalizes into (write.rs). Mirror that
        // normalization or the sub-second part is silently dropped and
        // the reported mtime is a full second off. i128 math keeps
        // secs == i64::MIN from overflowing the negation.
        let total_ns = secs as i128 * 1_000_000_000 + nsecs as i128;
        let abs_ns = -total_ns;
        UNIX_EPOCH
            - std::time::Duration::new(
                (abs_ns / 1_000_000_000) as u64,
                (abs_ns % 1_000_000_000) as u32,
            )
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
        atime: fuser_wire_time(system_time_from_secs(meta.atime(), meta.atime_nsec())),
        mtime: fuser_wire_time(system_time_from_secs(meta.mtime(), meta.mtime_nsec())),
        ctime: fuser_wire_time(system_time_from_secs(meta.ctime(), meta.ctime_nsec())),
        crtime: fuser_wire_time(meta.created().unwrap_or(UNIX_EPOCH)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pre_epoch_timespec_keeps_sub_second_part() {
        // -1.5 s in Linux timespec shape: { -2, 500_000_000 }.
        let t = system_time_from_secs(-2, 500_000_000);
        assert_eq!(
            UNIX_EPOCH.duration_since(t).unwrap(),
            Duration::from_millis(1500)
        );
    }

    #[test]
    fn pre_epoch_whole_seconds_unchanged() {
        let t = system_time_from_secs(-2, 0);
        assert_eq!(
            UNIX_EPOCH.duration_since(t).unwrap(),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn post_epoch_timespec_keeps_sub_second_part() {
        let t = system_time_from_secs(2, 500_000_000);
        assert_eq!(
            t.duration_since(UNIX_EPOCH).unwrap(),
            Duration::from_millis(2500)
        );
    }

    /// fuser 0.15–0.18's actual buggy encode, verbatim from `ll/reply.rs`
    /// `time_from_system_time`, as the round-trip adversary.
    fn fuser_0_15_time_from_system_time(system_time: &SystemTime) -> (i64, u32) {
        match system_time.duration_since(UNIX_EPOCH) {
            Ok(duration) => (duration.as_secs() as i64, duration.subsec_nanos()),
            Err(before_epoch_error) => (
                -(before_epoch_error.duration().as_secs() as i64),
                before_epoch_error.duration().subsec_nanos(),
            ),
        }
    }

    /// Through fuser 0.15's encoder, the compensated instant produces the
    /// timespec the kernel reads back as the true time.
    #[test]
    fn wire_compensation_yields_normalized_timespec() {
        let true_time = system_time_from_secs(-2, 500_000_000); // -1.5 s
        let (sec, nsec) = fuser_0_15_time_from_system_time(&fuser_wire_time(true_time));
        assert_eq!((sec, nsec), (-2, 500_000_000));
        // Uncompensated, the buggy encoder emits {-1, 500_000_000} (-0.5 s).
        let (sec, nsec) = fuser_0_15_time_from_system_time(&true_time);
        assert_eq!((sec, nsec), (-1, 500_000_000));
    }

    #[test]
    fn wire_compensation_covers_near_epoch_and_whole_second_boundaries() {
        // -1 ns: timespec {-1, 999_999_999}; the compensated instant must
        // encode to exactly that pair through fuser 0.15's buggy splitter.
        let true_time = system_time_from_secs(-1, 999_999_999);
        let (sec, nsec) = fuser_0_15_time_from_system_time(&fuser_wire_time(true_time));
        assert_eq!((sec, nsec), (-1, 999_999_999));
        // -0.5 s: timespec {-1, 500_000_000}.
        let true_time = system_time_from_secs(-1, 500_000_000);
        let (sec, nsec) = fuser_0_15_time_from_system_time(&fuser_wire_time(true_time));
        assert_eq!((sec, nsec), (-1, 500_000_000));
    }

    #[test]
    fn wire_compensation_saturates_at_the_representable_minimum() {
        // tv_sec: i64::MIN — fuser 0.15's negation overflows at this
        // distance; the compensation must saturate without panicking and
        // stay within fuser's encodable range.
        let extreme = system_time_from_secs(i64::MIN, 0);
        let d = UNIX_EPOCH.duration_since(fuser_wire_time(extreme)).unwrap();
        assert!(d.as_secs() <= i64::MAX as u64);
        // Fractional {i64::MIN, nsec > 0}: the compensated instant falls
        // below the representable minimum — saturate, don't panic.
        let extreme = system_time_from_secs(i64::MIN, 999_999_999);
        let d = UNIX_EPOCH.duration_since(fuser_wire_time(extreme)).unwrap();
        assert!(d.as_secs() <= i64::MAX as u64);
    }

    #[test]
    fn wire_compensation_keeps_whole_seconds_and_post_epoch() {
        let (sec, nsec) =
            fuser_0_15_time_from_system_time(&fuser_wire_time(system_time_from_secs(-2, 0)));
        assert_eq!((sec, nsec), (-2, 0));
        let post = UNIX_EPOCH + Duration::new(2, 500_000_000);
        let (sec, nsec) = fuser_0_15_time_from_system_time(&fuser_wire_time(post));
        assert_eq!((sec, nsec), (2, 500_000_000));
    }

    /// The full setattr → getattr round trip: what `setattr_impl`
    /// normalizes into a timespec must read back as the same instant.
    #[test]
    fn setattr_getattr_roundtrip() {
        let original = UNIX_EPOCH - Duration::from_millis(1500);
        let Err(e) = original.duration_since(UNIX_EPOCH) else {
            panic!("expected pre-epoch");
        };
        let mut sec = -(e.duration().as_secs() as i64);
        let mut nsec = -(e.duration().subsec_nanos() as i64);
        if nsec < 0 {
            sec -= 1;
            nsec += 1_000_000_000;
        }
        let read_back = system_time_from_secs(sec, nsec);
        assert_eq!(read_back, original);
    }
}
