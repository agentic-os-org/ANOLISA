//! A pidfd pins process lifetime; procfs still supplies live ownership and namespace.

use super::Process;
use crate::{rejected, Result};
use std::{
    fs::File,
    os::{fd::AsRawFd, fd::FromRawFd, unix::fs::MetadataExt},
    sync::Arc,
};

/// Stable process lifetime paired with live procfs identity checks.
pub(super) struct Files {
    /// Exit notification whose lifetime outlasts an individual poll.
    pub(super) exit: Arc<File>,
    directory: File,
    pid: u32,
    start: String,
}

impl Files {
    /// Open close-on-exec descriptors before the caller revalidates identity.
    pub(super) fn open(process: &Process) -> std::io::Result<Self> {
        // SAFETY: pidfd_open takes a numeric PID and zero flags, with no pointers.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, process.pid, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a successful pidfd_open returns a new owned, close-on-exec FD.
        let exit = Arc::new(unsafe { File::from_raw_fd(fd as i32) });
        Ok(Self {
            exit,
            directory: File::open(format!("/proc/{}", process.pid))?,
            pid: process.pid,
            start: process.start.clone(),
        })
    }

    /// Reject termination, identity mutation or loss of procfs access.
    pub(super) fn check(&self, process: &Process) -> Result<()> {
        let mut exit = libc::pollfd {
            fd: self.exit.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the pidfd and pollfd stay valid for this nonblocking call.
        let ready = unsafe { libc::poll(&mut exit, 1, 0) };
        if ready != 0
            || self.pid != process.pid
            || self.start != process.start
            || self.directory.metadata()?.uid() != process.uid
        {
            return Err(rejected("stale_process"));
        }
        let mut namespace = [0u8; 128];
        // SAFETY: the directory FD, NUL-terminated name and writable buffer are valid.
        let length = unsafe {
            libc::readlinkat(
                self.directory.as_raw_fd(),
                c"ns/pid".as_ptr(),
                namespace.as_mut_ptr().cast(),
                namespace.len(),
            )
        };
        if length < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if length as usize == namespace.len()
            || &namespace[..length as usize] != process.namespace.as_bytes()
        {
            return Err(rejected("stale_process"));
        }
        Ok(())
    }
}
