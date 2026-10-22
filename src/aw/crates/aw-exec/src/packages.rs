//! Shared package-file leases acquired before resolving managed runtime commands.

use crate::CommandSpec;
use std::{io, path::PathBuf};

#[cfg(target_os = "linux")]
mod linux;

/// A managed installation could not be held stable before executing its files.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// A mutation already owns this installation's exclusive lock.
    #[error("installed package files are being changed: {0}")]
    Busy(PathBuf),
    /// Managed records or their existing lock have unsafe filesystem metadata.
    #[error("installed package files cannot be protected safely: {0}")]
    Unsafe(PathBuf),
    /// The prefix or its lock changed during acquisition.
    #[error("installed package records changed; retry: {0}")]
    Changed(PathBuf),
    /// The explicit process context does not select a trustworthy executable path.
    #[error("package lease context invalid: {0}")]
    Invalid(&'static str),
    /// A supplied command cannot be resolved from its explicit PATH.
    #[error("command is unavailable in its explicit PATH: {0}")]
    Executable(PathBuf),
    /// An operating-system error prevented reliable package reference discovery.
    #[error("package reference lookup failed at {path}: {source}")]
    Io {
        /// Path whose metadata could not be checked.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// Shared package leases require Linux flock semantics.
    #[error("package-file protection requires Linux")]
    UnsupportedPlatform,
}

/// Shared locks for the managed installations referenced by known local paths.
///
/// Retain this value until all corresponding owned runtime work has stopped.
/// It protects against cooperating package mutations; it does not prove that
/// older runtimes, shell-generated paths or arbitrary external processes are absent.
/// No records or locks are created, and no installation database is scanned.
#[derive(Debug)]
pub struct PackageLeases {
    #[cfg(target_os = "linux")]
    _held: Vec<std::fs::File>,
}

impl PackageLeases {
    /// Resolve each executable and hold installations referenced by its literal argv.
    ///
    /// Resolution uses only that command's working directory and explicit PATH.
    /// The program is replaced by its resolved absolute path before returning.
    /// Absolute arguments, including `--option=/absolute/path`, are also checked.
    /// Locks are acquired in prefix order without waiting or inheriting descriptors
    /// into children. Relative arguments and executable-generated paths cannot
    /// establish that all runtime references are known.
    ///
    /// # Errors
    /// Rejects unresolved executables, unsafe records/locks, contention, concurrent
    /// metadata replacement, inaccessible ancestors and unsupported platforms.
    pub fn acquire<'a>(
        commands: impl IntoIterator<Item = &'a mut CommandSpec>,
    ) -> Result<Self, LeaseError> {
        #[cfg(target_os = "linux")]
        {
            linux::commands(commands)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = commands;
            Err(LeaseError::UnsupportedPlatform)
        }
    }

    /// Hold known absolute paths, such as the current service executable.
    ///
    /// Existing symbolic aliases are resolved as well as their original ancestors.
    /// The method reads only path ancestors and retains no executable or record data.
    ///
    /// # Errors
    /// Rejects relative paths, unsafe/missing managed locks, contention and lookup errors.
    pub fn for_paths(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self, LeaseError> {
        #[cfg(target_os = "linux")]
        {
            linux::paths(paths)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = paths;
            Err(LeaseError::UnsupportedPlatform)
        }
    }
}
