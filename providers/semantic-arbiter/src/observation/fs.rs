//! Descriptor-relative observation; unsupported kernel confinement fails closed.

use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

pub(super) const FILE_LIMIT: usize = 64 * 1024;
pub(super) const TOTAL_LIMIT: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Identity {
    dev: u64,
    ino: u64,
}

impl Identity {
    pub(super) fn of(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
}

pub(super) fn timely(deadline: Instant) -> Result<(), &'static str> {
    if Instant::now() >= deadline {
        Err("observation_timeout")
    } else {
        Ok(())
    }
}

fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// Resolve the whole relative path in one kernel operation: a sequence of
/// openat calls alone can escape when an intermediate directory is moved.
pub(super) fn open_beneath(root: &File, path: &str, flags: i32) -> io::Result<File> {
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let path = CString::new(path).map_err(|_| denied())?;
    // openat2 rejects O_NONBLOCK with O_PATH. Metadata-only descriptors cannot
    // block on FIFO I/O; every descriptor that can read bytes uses O_NONBLOCK.
    let nonblock = if flags & libc::O_PATH == 0 {
        libc::O_NONBLOCK
    } else {
        0
    };
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | nonblock) as u64,
        mode: 0,
        // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS.
        resolve: 0x08 | 0x04 | 0x02,
    };
    // SAFETY: live directory fd, NUL-terminated path and correctly sized Linux
    // open_how layout. On success ownership of the fd is transferred once.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

pub(super) fn root(workspace: &str, deadline: Instant) -> Result<File, &'static str> {
    timely(deadline)?;
    if !workspace.starts_with('/') || workspace == "/" || workspace.len() > 4096 {
        return Err("workspace_unavailable");
    }
    let path = relative(&workspace[1..], "").ok_or("workspace_unavailable")?;
    let slash = File::open("/").map_err(|_| "workspace_unavailable")?;
    let root = open_beneath(&slash, &path, libc::O_RDONLY | libc::O_DIRECTORY)
        .map_err(|_| "workspace_unavailable")?;
    timely(deadline)?;
    Ok(root)
}

pub(super) fn relative(path: &str, workspace: &str) -> Option<String> {
    if path.len() > 4096 || path.as_bytes().contains(&0) {
        return None;
    }
    let path = if path.starts_with('/') {
        path.strip_prefix(workspace.trim_end_matches('/'))?
            .strip_prefix('/')?
    } else {
        path
    };
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            ".." => return None,
            "" | "." => continue,
            _ => parts.push(part),
        }
    }
    if parts.is_empty() || parts.len() > 12 {
        return None;
    }
    Some(parts.join("/"))
}

pub(super) fn metadata(root: &File, path: &str) -> io::Result<Metadata> {
    // O_PATH never opens devices or FIFOs for I/O; a final link is inspected,
    // not dereferenced. Intermediate links are rejected by openat2.
    open_beneath(root, path, libc::O_PATH)?.metadata()
}

pub(super) fn stable(a: &Metadata, b: &Metadata) -> bool {
    Identity::of(a) == Identity::of(b)
        && a.mode() == b.mode()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

pub(super) fn read_stable(
    root: &File,
    path: &str,
    before: &Metadata,
    remaining: &mut usize,
    deadline: Instant,
    after_read: impl FnOnce(),
) -> Result<Vec<u8>, &'static str> {
    timely(deadline)?;
    if !before.is_file() {
        return Err("not_regular_file");
    }
    if before.len() > FILE_LIMIT as u64 {
        return Err("file_too_large");
    }
    let size = before.len() as usize;
    if size > *remaining {
        return Err("read_budget_exceeded");
    }
    let mut file = open_beneath(root, path, libc::O_RDONLY).map_err(|_| "file_open_failed")?;
    timely(deadline)?;
    if !stable(before, &file.metadata().map_err(|_| "file_stat_failed")?) {
        return Err("file_changed");
    }
    let mut bytes = vec![0; size];
    let mut offset = 0;
    while offset < size {
        timely(deadline)?;
        let end = (offset + 8192).min(size);
        let count = file
            .read(&mut bytes[offset..end])
            .map_err(|_| "file_read_failed")?;
        *remaining -= count;
        if count == 0 {
            return Err("file_changed");
        }
        offset += count;
    }
    after_read();
    timely(deadline)?;
    let after = file.metadata().map_err(|_| "file_stat_failed")?;
    timely(deadline)?;
    let named = metadata(root, path).map_err(|_| "file_changed")?;
    timely(deadline)?;
    if !stable(before, &after) || !stable(&after, &named) {
        return Err("file_changed");
    }
    Ok(bytes)
}
