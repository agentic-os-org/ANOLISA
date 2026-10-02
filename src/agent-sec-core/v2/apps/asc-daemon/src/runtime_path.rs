//! Deployment-owned runtime directory, singleton lock and stale socket admission.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open, openat};
use rustix::process::geteuid;

/// Owns the runtime-directory lock through application and Tokio shutdown.
///
/// The managed service uses `/run/agent-sec-core`. An explicit socket in a
/// different secured directory selects an isolated development instance.
/// Clients must not have write access to this directory. The service UID and
/// root are trusted; flock cannot fence another process acting as either UID.
pub(super) struct RuntimeLease {
    _lock: File,
    socket_path: PathBuf,
}

impl RuntimeLease {
    /// Validates the existing directory and locks `daemon.lock` without reopening it.
    ///
    /// # Errors
    /// Rejects unsafe paths, owners, modes, linked/non-regular lock files, an
    /// already held lock, and operating-system failures. Never creates directories.
    pub(super) fn acquire(socket_path: &Path) -> Result<Self, RuntimePathError> {
        if !socket_path.is_absolute()
            || socket_path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            || socket_path
                .file_name()
                .is_none_or(|name| name == "daemon.lock")
        {
            return Err(RuntimePathError::UnsafePath);
        }
        let directory_path = socket_path.parent().ok_or(RuntimePathError::UnsafePath)?;
        let uid = geteuid().as_raw();
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut directory = File::from(open("/", flags, Mode::empty())?);
        for part in directory_path.components() {
            if let Component::Normal(name) = part {
                directory = File::from(openat(&directory, name, flags, Mode::empty())?);
                let metadata = directory.metadata()?;
                // Root-owned sticky directories (e.g. /tmp) protect owned children.
                let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
                if (metadata.uid() != 0 && metadata.uid() != uid)
                    || (metadata.mode() & 0o022 != 0 && !sticky_root)
                {
                    return Err(RuntimePathError::UnsafeDirectory);
                }
            }
        }
        let metadata = directory.metadata()?;
        // Directory special bits do not grant clients additional access.
        if metadata.uid() != uid || !matches!(metadata.mode() & 0o777, 0o700 | 0o750 | 0o755) {
            return Err(RuntimePathError::UnsafeDirectory);
        }
        let lock = File::from(openat(
            &directory,
            "daemon.lock",
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )?);
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(RuntimePathError::UnsafeLock);
        }
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == rustix::io::Errno::WOULDBLOCK {
                RuntimePathError::AlreadyRunning
            } else {
                RuntimePathError::Io(error.into())
            }
        })?;
        Ok(Self {
            _lock: lock,
            socket_path: socket_path.into(),
        })
    }

    /// Removes only an owned stale socket with no listener while holding the lock.
    ///
    /// # Errors
    /// Rejects unsafe objects, live listeners, ambiguous probe failures and
    /// replacement inodes. Only `ECONNREFUSED` establishes a stale listener.
    pub(super) async fn prepare_socket(&self) -> Result<(), RuntimePathError> {
        let metadata = match fs::symlink_metadata(&self.socket_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_socket()
            || metadata.uid() != geteuid().as_raw()
            || !matches!(metadata.mode() & 0o7777, 0o600 | 0o660 | 0o666)
            || metadata.nlink() != 1
        {
            return Err(RuntimePathError::UnsafeSocket);
        }
        match tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::UnixStream::connect(&self.socket_path),
        )
        .await
        {
            Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => {}
            _ => return Err(RuntimePathError::SocketInUse),
        }
        let current = fs::symlink_metadata(&self.socket_path)?;
        if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            return Err(RuntimePathError::UnsafeSocket);
        }
        fs::remove_file(&self.socket_path)?;
        Ok(())
    }
}

/// Stable startup failure categories for the managed runtime namespace.
#[derive(Debug, thiserror::Error)]
pub(super) enum RuntimePathError {
    /// The socket must be absolute, without parent traversal or a reserved name.
    #[error("unsafe runtime socket path")]
    UnsafePath,
    /// Runtime directories must be owned by the service and protected from clients.
    #[error("unsafe runtime directory owner or mode (expected service-owned 0700, 0750 or 0755)")]
    UnsafeDirectory,
    /// Lock validation occurs on the same descriptor used for flock.
    #[error("unsafe daemon lock (expected owned regular file, mode 0600, one link)")]
    UnsafeLock,
    /// Another daemon owns this runtime namespace.
    #[error("daemon already running in this runtime directory")]
    AlreadyRunning,
    /// Only an owned socket with an allowed mode may be recovered.
    #[error("unsafe existing daemon socket")]
    UnsafeSocket,
    /// A live or ambiguous listener must never be unlinked.
    #[error("daemon socket is live or could not be safely classified")]
    SocketInUse,
    /// The OS refused path access or a lock/socket operation.
    #[error("runtime path operation failed")]
    Io(#[from] io::Error),
}

impl From<rustix::io::Errno> for RuntimePathError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}
