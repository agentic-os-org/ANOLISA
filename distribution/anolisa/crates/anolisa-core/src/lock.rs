//! Advisory install lock.
//!
//! Every write path that touches `installed.toml`, install files,
//! backups, or the central log must hold an exclusive
//! [`InstallLock`] (launch spec §8.5). The lock is implemented via
//! `fs2::FileExt::try_lock_exclusive`, which maps to `flock(LOCK_EX |
//! LOCK_NB)` on Linux and a non-blocking equivalent elsewhere.
//!
//! Acquisition fails fast (no blocking) so a second ANOLISA invocation
//! sees a clear error rather than hanging.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

/// Held advisory lock on the ANOLISA state directory.
#[derive(Debug)]
pub struct InstallLock {
    file: File,
    path: PathBuf,
}

/// Errors raised by [`InstallLock`].
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Filesystem access failed while creating, opening, or locking the
    /// lock file.
    #[error("io error while accessing lock file {path}: {source}")]
    Io {
        /// Lock path involved in the failed filesystem operation.
        path: PathBuf,
        /// Original I/O error from the OS.
        #[source]
        source: io::Error,
    },
    /// A different process currently holds the advisory lock.
    #[error("install lock at {path} is already held by another process")]
    Held {
        /// Lock file path that reported contention.
        path: PathBuf,
    },
}

impl InstallLock {
    /// Try to take an exclusive non-blocking lock on `lock_path`.
    /// Returns [`LockError::Held`] if another process holds the lock.
    pub fn acquire(lock_path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = lock_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| LockError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|source| LockError::Io {
                path: lock_path.to_path_buf(),
                source,
            })?;

        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self {
                file,
                path: lock_path.to_path_buf(),
            }),
            Err(err) if would_block(&err) => Err(LockError::Held {
                path: lock_path.to_path_buf(),
            }),
            Err(source) => Err(LockError::Io {
                path: lock_path.to_path_buf(),
                source,
            }),
        }
    }

    /// Path the lock was acquired on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Explicitly release the lock. Dropping also releases it via the
    /// [`Drop`] implementation, but callers can be explicit.
    pub fn release(self) {
        // Best-effort: ignore unlock errors (drop will retry).
        let _ = FileExt::unlock(&self.file);
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        // Release before closing the fd so immediate same-process handoff
        // does not depend on platform-specific close-time lock behavior.
        let _ = FileExt::unlock(&self.file);
    }
}

fn would_block(err: &io::Error) -> bool {
    // fs2 returns ErrorKind::WouldBlock on contention. Some platforms
    // surface raw EAGAIN/EWOULDBLOCK codes instead.
    if err.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    matches!(err.raw_os_error(), Some(code) if code == libc_eagain() || code == libc_ewouldblock())
}

#[cfg(unix)]
fn libc_eagain() -> i32 {
    11 // EAGAIN on Linux/macOS
}

#[cfg(unix)]
fn libc_ewouldblock() -> i32 {
    // On Linux EWOULDBLOCK == EAGAIN. macOS differs only academically.
    libc_eagain()
}

#[cfg(not(unix))]
fn libc_eagain() -> i32 {
    -1
}

#[cfg(not(unix))]
fn libc_ewouldblock() -> i32 {
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_succeeds_on_fresh_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("anolisa.lock");
        let lock = InstallLock::acquire(&path).expect("first acquire should succeed");
        assert_eq!(lock.path(), path.as_path());
        assert!(path.exists());
    }

    #[test]
    fn second_acquire_while_held_returns_held_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anolisa.lock");

        let _first = InstallLock::acquire(&path).expect("first acquire should succeed");
        let err = InstallLock::acquire(&path).expect_err("second acquire should fail");
        assert!(matches!(err, LockError::Held { .. }));
    }

    #[test]
    fn release_lets_subsequent_acquire_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anolisa.lock");

        let first = InstallLock::acquire(&path).expect("first acquire should succeed");
        first.release();
        let _second = InstallLock::acquire(&path).expect("re-acquire after release should succeed");
    }

    #[test]
    fn drop_lets_subsequent_acquire_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anolisa.lock");

        {
            let _first = InstallLock::acquire(&path).expect("first acquire should succeed");
        }

        let _second = InstallLock::acquire(&path).expect("re-acquire after drop should succeed");
    }
}
