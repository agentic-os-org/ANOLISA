//! Explicit package lock release across concurrent fork/exec windows.

use std::{
    fs::{File, Metadata, Permissions},
    io,
    os::fd::AsRawFd,
};

/// An acquired flock whose lifetime belongs to the acquiring process.
///
/// Closing a descriptor alone lets an unrelated fork retain the lock until exec.
/// This guard explicitly unlocks before closing; it must remain alive until all
/// protected work ends. It is intentionally neither cloneable nor convertible to File.
#[derive(Debug)]
pub struct PackageLock {
    file: File,
    owner: libc::pid_t,
}

impl PackageLock {
    /// Take sole ownership of a file on which the caller already acquired flock.
    ///
    /// The caller must not retain duplicates for independent lock ownership.
    pub fn from_locked(file: File) -> Self {
        // SAFETY: getpid only reads the current process identity.
        Self {
            file,
            owner: unsafe { libc::getpid() },
        }
    }

    /// Read metadata without exposing a duplicable lock descriptor.
    ///
    /// # Errors
    /// Returns the underlying filesystem metadata error.
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }

    /// Change permissions while retaining the acquired lock.
    ///
    /// # Errors
    /// Returns the underlying filesystem permission error.
    pub fn set_permissions(&self, permissions: Permissions) -> io::Result<()> {
        self.file.set_permissions(permissions)
    }
}

impl Drop for PackageLock {
    fn drop(&mut self) {
        // A fork child must not release its parent's lock when dropping a copy.
        // SAFETY: the owned File remains open; getpid and flock take no Rust pointers.
        unsafe {
            if libc::getpid() == self.owner {
                // LOCK_UN on this valid flock fd cannot block. Close still follows.
                let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
