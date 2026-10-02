use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::Context;
use tracing::warn;

/// Lockfile holder: holds file handle + flock lock
#[derive(Debug)]
pub(crate) struct LockfileHolder {
    _file: std::fs::File,
}

/// Acquire lockfile and perform crash detection.
///
/// - lockfile does not exist → normal startup (first or reboot)
/// - lockfile exists and lock acquired → last crash (process died, kernel released flock, but file remained)
/// - lockfile exists and lock acquisition failed → another instance is running, reject startup
pub(crate) fn acquire(lockfile_path: &Path) -> anyhow::Result<LockfileHolder> {
    // Ensure lockfile directory exists (systemd RuntimeDirectory manages, but fallback creates)
    if let Some(parent) = lockfile_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create lockfile directory: {:?}", parent))?;
    }

    let lockfile_existed = lockfile_path.exists();

    // Open or create lockfile
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lockfile_path)
        .with_context(|| format!("Failed to open lockfile: {:?}", lockfile_path))?;

    // Attempt non-blocking lock acquisition
    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            anyhow::bail!(
                "Another ws-ckpt daemon instance is running (lockfile {:?} is locked)",
                lockfile_path
            );
        }
        return Err(err).with_context(|| format!("flock failed: {:?}", lockfile_path));
    }

    // Lock acquired
    if lockfile_existed {
        warn!(
            "Detected unclean shutdown (lockfile {:?} present from previous run)",
            lockfile_path
        );
    }

    // Write current PID
    let mut file = file;
    file.set_len(0)?;
    write!(file, "{}", std::process::id())?;
    file.sync_all()?;

    Ok(LockfileHolder { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_fresh_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws-ckpt.lock");
        let holder = acquire(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, format!("{}", std::process::id()));
        drop(holder);
    }

    #[test]
    fn acquire_after_crash_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws-ckpt.lock");
        std::fs::write(&path, "99999").unwrap();
        let holder = acquire(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, format!("{}", std::process::id()));
        drop(holder);
    }

    #[test]
    fn double_acquire_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws-ckpt.lock");
        let _holder = acquire(&path).unwrap();
        let err = acquire(&path).unwrap_err();
        assert!(
            format!("{}", err).contains("Another"),
            "expected 'Another instance' error, got: {}",
            err
        );
    }

    #[test]
    fn acquire_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("ws-ckpt.lock");
        let _holder = acquire(&path).unwrap();
        assert!(path.exists());
    }
}
