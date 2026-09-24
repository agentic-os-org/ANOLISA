//! Cross-process advisory locking for database maintenance.

use std::{fs::File, path::Path};

use fs2::FileExt;

use crate::LifecycleError;

/// Cross-process advisory lock associated with one SQLite database.
///
/// The persistent lock file appends `.maintenance.lock` to the database path.
/// It is retained after release so contenders always coordinate on one inode.
#[derive(Debug)]
pub struct MaintenanceLock {
    file: File,
}

/// Result of a non-blocking maintenance lock attempt.
#[derive(Debug)]
pub enum MaintenanceLockAcquire {
    /// The caller owns the lock until the guard is dropped.
    Acquired(MaintenanceLock),
    /// Another process or thread currently owns the lock.
    Busy,
}

impl MaintenanceLock {
    /// Attempts to acquire the database's maintenance lock without blocking.
    ///
    /// On Unix, a newly created lock file is restricted to mode `0600`. Existing
    /// lock files are not replaced or removed.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock file cannot be opened or the operating
    /// system rejects the lock operation for a reason other than contention.
    pub fn try_acquire(database_path: &Path) -> Result<MaintenanceLockAcquire, LifecycleError> {
        let lock_path = lock_path(database_path);
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }

        let file = options
            .open(&lock_path)
            .map_err(|source| LifecycleError::MaintenanceLock {
                operation: "open",
                path: lock_path.clone(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            let metadata = file
                .metadata()
                .map_err(|source| LifecycleError::MaintenanceLock {
                    operation: "inspect",
                    path: lock_path.clone(),
                    source,
                })?;
            // SAFETY: geteuid has no arguments and only reads process credentials.
            let effective_uid = unsafe { libc::geteuid() };
            if !metadata.file_type().is_file()
                || metadata.nlink() != 1
                || metadata.uid() != effective_uid
            {
                return Err(LifecycleError::MaintenanceLock {
                    operation: "validate",
                    path: lock_path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "lock must be a single-link regular file owned by this process user",
                    ),
                });
            }
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            file.set_permissions(permissions).map_err(|source| {
                LifecycleError::MaintenanceLock {
                    operation: "secure",
                    path: lock_path.clone(),
                    source,
                }
            })?;
        }
        match file.try_lock_exclusive() {
            Ok(()) => Ok(MaintenanceLockAcquire::Acquired(Self { file })),
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(MaintenanceLockAcquire::Busy)
            }
            Err(source) => Err(LifecycleError::MaintenanceLock {
                operation: "acquire",
                path: lock_path,
                source,
            }),
        }
    }
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        // Advisory locks are also released when the file closes; the explicit
        // unlock makes the guard's lifetime contract immediate and portable.
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_path(database_path: &Path) -> std::path::PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(".maintenance.lock");
    path.into()
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;

    fn test_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time must follow Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentsight-maintenance-lock-{}-{nonce}.db",
            std::process::id()
        ))
    }

    #[test]
    fn competing_lock_is_busy_until_guard_drops() {
        let database_path = test_path();
        let first = MaintenanceLock::try_acquire(&database_path).unwrap();
        let MaintenanceLockAcquire::Acquired(first_guard) = first else {
            panic!("first lock attempt must acquire the lock");
        };

        assert!(matches!(
            MaintenanceLock::try_acquire(&database_path).unwrap(),
            MaintenanceLockAcquire::Busy
        ));
        drop(first_guard);
        assert!(matches!(
            MaintenanceLock::try_acquire(&database_path).unwrap(),
            MaintenanceLockAcquire::Acquired(_)
        ));

        fs::remove_file(lock_path(&database_path)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_hardlink_lock_files_are_rejected() {
        use std::os::unix::fs::symlink;

        let database_path = test_path();
        let path = lock_path(&database_path);
        let target = test_path();
        fs::write(&target, b"target").unwrap();
        symlink(&target, &path).unwrap();
        assert!(MaintenanceLock::try_acquire(&database_path).is_err());
        fs::remove_file(&path).unwrap();

        fs::hard_link(&target, &path).unwrap();
        assert!(MaintenanceLock::try_acquire(&database_path).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_file(target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_is_private_even_when_it_already_exists() {
        use std::os::unix::fs::PermissionsExt;

        let database_path = test_path();
        let path = lock_path(&database_path);
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let guard = MaintenanceLock::try_acquire(&database_path).unwrap();
        assert!(matches!(guard, MaintenanceLockAcquire::Acquired(_)));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(guard);
        fs::remove_file(path).unwrap();
    }
}
