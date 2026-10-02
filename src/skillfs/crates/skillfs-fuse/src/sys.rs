//! Thin libc wrappers used by the FUSE callbacks.
//!
//! Two groups of helpers live here:
//!
//! * Error mapping ([`errno`]) plus the `RENAME_NOREPLACE` rename used by
//!   the install-inbox `current` flip.
//! * `*at`-family helpers anchored at a parent directory fd. These let
//!   passthrough callbacks fall back to `openat`/`mkdirat`/`unlinkat`/
//!   `fstatat`/`renameat2` when the full absolute physical path would
//!   exceed `PATH_MAX` — opening the parent fd (which still fits) then
//!   addressing the leaf alone keeps very deep skill subtrees usable.
//!
//! All helpers return `std::io::Error` so call sites can keep using
//! [`errno`] to extract the raw `i32` they hand back to FUSE.

use std::path::Path;

/// Extract raw OS error code, falling back to EIO.
pub(crate) fn errno(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

/// Rename `old` to `new` with Linux `RENAME_NOREPLACE` semantics: fail with
/// `EEXIST` if the target already exists, otherwise rename atomically.
///
/// Implemented via the `renameat2` syscall so the existence check and the
/// rename are performed atomically in the kernel — a userspace
/// "exists?-then-rename" pattern would race if a file appeared between the
/// two steps.
#[cfg(target_os = "linux")]
pub(crate) fn rename_noreplace(old: &Path, new: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let old_c = CString::new(old.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let new_c = CString::new(new.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;

    let ret = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old_c.as_ptr(),
            libc::AT_FDCWD,
            new_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn rename_noreplace(_old: &Path, _new: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
}

// ---------------------------------------------------------------------------
// openat-family helpers for long-path passthrough operations.
//
// SkillFS callbacks normally call `std::fs::*` on the full absolute physical
// path (e.g. `<source>/<skill>/<sandbox>/<comp1>/.../<leaf>`). When the
// caller-supplied relative path approaches `PATH_MAX`, that absolute path can
// exceed the kernel's userspace limit and the daemon's `openat(AT_FDCWD,
// huge_path, …)` syscall fails with `ENAMETOOLONG` even though Linux would
// have accepted the operation against a shorter parent fd. These helpers let
// callbacks fall back to *at syscalls anchored at the parent directory: open
// the parent's physical path (which fits when the leaf alone would not), then
// pass only the leaf component to the syscall.
// ---------------------------------------------------------------------------

/// Open a directory by its absolute physical path with flags suitable for
/// passing the resulting fd to `*at` syscalls.
pub(crate) fn open_dir_path(path: &Path) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
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
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd was just produced by open(2) and is owned exclusively here.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub(crate) fn cstring_from_os_str(s: &std::ffi::OsStr) -> std::io::Result<std::ffi::CString> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    CString::new(s.as_bytes()).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

pub(crate) fn openat_leaf(
    dir: &std::fs::File,
    leaf: &std::ffi::OsStr,
    flags: i32,
    mode: u32,
) -> std::io::Result<std::fs::File> {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let c = cstring_from_os_str(leaf)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub(crate) fn mkdirat_leaf(
    dir: &std::fs::File,
    leaf: &std::ffi::OsStr,
    mode: u32,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let c = cstring_from_os_str(leaf)?;
    let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode as libc::mode_t) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn unlinkat_leaf(
    dir: &std::fs::File,
    leaf: &std::ffi::OsStr,
    flags: i32,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let c = cstring_from_os_str(leaf)?;
    let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), flags) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn fstatat_leaf(
    dir: &std::fs::File,
    leaf: &std::ffi::OsStr,
    follow: bool,
) -> std::io::Result<libc::stat> {
    use std::os::unix::io::AsRawFd;
    let c = cstring_from_os_str(leaf)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    let rc = unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), &mut st, flags) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

#[cfg(target_os = "linux")]
pub(crate) fn renameat2_leaf(
    old_dir: &std::fs::File,
    old_leaf: &std::ffi::OsStr,
    new_dir: &std::fs::File,
    new_leaf: &std::ffi::OsStr,
    flags: u32,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let old_c = cstring_from_os_str(old_leaf)?;
    let new_c = cstring_from_os_str(new_leaf)?;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            old_dir.as_raw_fd(),
            old_c.as_ptr(),
            new_dir.as_raw_fd(),
            new_c.as_ptr(),
            flags,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn renameat2_leaf(
    _old_dir: &std::fs::File,
    _old_leaf: &std::ffi::OsStr,
    _new_dir: &std::fs::File,
    _new_leaf: &std::ffi::OsStr,
    _flags: u32,
) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
}
