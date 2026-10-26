//! Owned directory and database identity checks, held through daemon drain.
use crate::RepositoryError;
use rustix::fs::{
    AtFlags, Dev, FileType, FlockOperation, Mode, OFlags, Stat, flock, fstat, lstat, mkdirat, open,
    openat, statat,
};
use std::fs::{File, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
mod tests;

/// Selects a Policy database, repairing owned leaf permissions without creating files.
///
/// Validates ancestors before setting an owned leaf to 0700. Only unsafe primary
/// ancestors permit fallback; an existing database must agree with the selection.
/// # Errors
/// Rejects conflicting databases, a selection that would bypass an existing
/// database, unsafe directories, orphaned sidecars and ambiguous I/O failures.
pub fn select_policy_database_path(
    primary: &Path,
    fallback: Option<&Path>,
) -> Result<PathBuf, RepositoryError> {
    validate_database_path(primary)?;
    let primary_directory = primary.parent().ok_or_else(|| invalid_path(primary))?;
    let Some(fallback) = fallback.filter(|path| *path != primary) else {
        directory(primary_directory, Access::Prepare)?;
        return Ok(primary.into());
    };
    validate_database_path(fallback)?;
    let primary_exists = database_exists(primary)?;
    let fallback_exists = database_exists(fallback)?;
    if primary_exists && fallback_exists {
        return Err(unsafe_path(
            primary,
            format!(
                "Policy databases exist at both A '{}' and B '{}'; refusing to choose or merge them; stop the daemon and explicitly resolve the authoritative database",
                primary.display(),
                fallback.display()
            ),
        ));
    }
    let primary_check = directory(primary_directory, Access::Prepare);
    let (selected, reason) = match primary_check {
        Ok(_) => (
            primary,
            "A's directory satisfies the safety requirements".to_owned(),
        ),
        Err(RepositoryError::UnsafePath { path, reason }) if path != primary_directory => {
            let error = RepositoryError::UnsafePath { path, reason };
            directory(
                fallback.parent().ok_or_else(|| invalid_path(fallback))?,
                Access::Prepare,
            ).map_err(|fallback_error| unsafe_path(primary, format!(
                "neither Policy database directory is usable; A '{}': {error}; B '{}': {fallback_error}; no database was created or migrated",
                primary.display(), fallback.display()
            )))?;
            (fallback, format!("A's directory is unsafe: {error}"))
        }
        Err(error) => return Err(error),
    };
    let existing = if primary_exists {
        Some(primary)
    } else if fallback_exists {
        Some(fallback)
    } else {
        None
    };
    if let Some(existing) = existing.filter(|path| *path != selected) {
        return Err(unsafe_path(
            existing,
            format!(
                "Policy database already exists at '{}', but directory checks select '{}' because {reason}; refusing to bypass existing Policy state or create an empty database; restore the previous directory configuration or explicitly migrate the database while the daemon is stopped",
                existing.display(),
                selected.display()
            ),
        ));
    }
    Ok(selected.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Inspect,
    Prepare,
    Create,
}

fn invalid_path(path: &Path) -> RepositoryError {
    unsafe_path(
        path,
        "expected an absolute path without parent traversal, named policy-state.db",
    )
}

fn unsafe_path(path: &Path, reason: impl Into<String>) -> RepositoryError {
    RepositoryError::UnsafePath {
        path: path.into(),
        reason: reason.into(),
    }
}

fn path_io(path: &Path, error: impl Into<io::Error>) -> RepositoryError {
    let error = error.into();
    io::Error::new(error.kind(), format!("'{}': {error}", path.display())).into()
}

fn validate_database_path(path: &Path) -> Result<(), RepositoryError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path.file_name().is_none_or(|n| n != "policy-state.db")
    {
        return Err(invalid_path(path));
    }
    Ok(())
}

fn directory(path: &Path, access: Access) -> Result<Option<File>, RepositoryError> {
    let uid = rustix::process::geteuid().as_raw();
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory =
        File::from(open("/", flags, Mode::empty()).map_err(|e| path_io(Path::new("/"), e))?);
    let mut current = PathBuf::from("/");
    for part in path.components() {
        if let Component::Normal(name) = part {
            current.push(name);
            directory = match openat(&directory, name, flags, Mode::empty()) {
                Ok(fd) => File::from(fd),
                Err(rustix::io::Errno::NOENT) if access != Access::Create => return Ok(None),
                Err(rustix::io::Errno::NOENT) => {
                    mkdirat(&directory, name, Mode::RWXU).map_err(|e| path_io(&current, e))?;
                    directory.sync_all().map_err(|e| path_io(&current, e))?;
                    File::from(
                        openat(&directory, name, flags, Mode::empty())
                            .map_err(|e| path_io(&current, e))?,
                    )
                }
                Err(error) => return Err(path_io(&current, error)),
            };
            if access != Access::Inspect {
                let m = directory.metadata().map_err(|e| path_io(&current, e))?;
                let sticky_root = m.uid() == 0 && m.mode() & 0o1000 != 0;
                let leaf = current == path;
                if if leaf {
                    m.uid() != uid || (m.mode() & 0o777 != 0o700 && access != Access::Prepare)
                } else {
                    (m.uid() != uid && m.uid() != 0) || (m.mode() & 0o022 != 0 && !sticky_root)
                } {
                    let expected = if leaf {
                        format!("private data directory owned by uid {uid}, mode 0700")
                    } else {
                        format!(
                            "ancestor owned by uid {uid} or root, without group/other write permissions (root-owned sticky directories excepted)"
                        )
                    };
                    return Err(unsafe_path(
                        &current,
                        format!(
                            "actual uid={}, gid={}, mode={:04o}; expected {expected}",
                            m.uid(),
                            m.gid(),
                            m.mode() & 0o7777
                        ),
                    ));
                }
                if leaf && access == Access::Prepare && m.mode() & 0o7777 != 0o700 {
                    directory
                        .set_permissions(Permissions::from_mode(0o700))
                        .map_err(|e| path_io(&current, e))?;
                }
            }
        }
    }
    if access != Access::Inspect && path == Path::new("/") {
        return Err(unsafe_path(
            path,
            "the filesystem root cannot be a private Policy data directory",
        ));
    }
    Ok(Some(directory))
}

fn database_exists(path: &Path) -> Result<bool, RepositoryError> {
    let Some(directory) = directory(
        path.parent().ok_or_else(|| invalid_path(path))?,
        Access::Inspect,
    )?
    else {
        return Ok(false);
    };
    match statat(&directory, "policy-state.db", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => return Ok(true),
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(path_io(path, error)),
    }
    for name in ["policy-state.db-wal", "policy-state.db-shm"] {
        let sidecar = path.with_file_name(name);
        match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => {
                return Err(unsafe_path(
                    &sidecar,
                    "SQLite sidecar exists without policy-state.db; refusing to initialize or select another database",
                ));
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(path_io(&sidecar, error)),
        }
    }
    Ok(false)
}

/// Independent database lease; different socket paths must not open the same state.
pub struct DatabaseLease {
    pub(crate) path: PathBuf,
    directory: File,
    _lock: File,
    identity: (Dev, u64),
}

impl DatabaseLease {
    /// Opens an owned private directory without following links and takes its process lock.
    /// # Errors
    /// Rejects unsafe paths, permissions, links, another owner or file I/O failures.
    pub fn acquire(path: &Path) -> Result<Self, RepositoryError> {
        validate_database_path(path)?;
        let directory = directory(
            path.parent().ok_or_else(|| invalid_path(path))?,
            Access::Create,
        )?
        .ok_or_else(|| invalid_path(path))?;
        let lock = owned_file(&directory, &path.with_file_name("policy-state.lock"))?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                RepositoryError::AlreadyOpen
            } else {
                e.into()
            }
        })?;
        let db = owned_file(&directory, path)?;
        let m = fstat(&db)?;
        let lease = Self {
            path: path.to_owned(),
            directory,
            _lock: lock,
            identity: (m.st_dev, m.st_ino),
        };
        lease.verify()?;
        Ok(lease)
    }

    pub(crate) fn verify(&self) -> Result<(), RepositoryError> {
        // Closing any extra DB/SHM descriptor releases this process's SQLite POSIX locks.
        let m = statat(
            &self.directory,
            "policy-state.db",
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        validate_file(&m, &self.path)?;
        let visible = lstat(&self.path)?;
        if (m.st_dev, m.st_ino) != self.identity
            || (visible.st_dev, visible.st_ino) != self.identity
            || FileType::from_raw_mode(visible.st_mode) == FileType::Symlink
        {
            return Err(unsafe_path(
                &self.path,
                "database identity changed or the visible database is a symlink",
            ));
        }
        for name in ["policy-state.db-wal", "policy-state.db-shm"] {
            match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(m) => validate_file(&m, &self.path.with_file_name(name))?,
                Err(rustix::io::Errno::NOENT) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn sync_directory(&self) -> Result<(), RepositoryError> {
        self.directory.sync_all()?;
        Ok(())
    }
}

fn owned_file(directory: &File, path: &Path) -> Result<File, RepositoryError> {
    let flags =
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::CREATE;
    let file = File::from(
        openat(
            directory,
            path.file_name().ok_or_else(|| invalid_path(path))?,
            flags,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|e| path_io(path, e))?,
    );
    validate_file(&fstat(&file)?, path)?;
    Ok(file)
}

fn validate_file(m: &Stat, path: &Path) -> Result<(), RepositoryError> {
    if FileType::from_raw_mode(m.st_mode) != FileType::RegularFile
        || m.st_nlink != 1
        || m.st_uid != rustix::process::geteuid().as_raw()
        || m.st_mode & 0o7777 != 0o600
    {
        return Err(unsafe_path(
            path,
            format!(
                "actual uid={}, gid={}, mode={:04o}, links={}, regular={}; expected a regular file owned by uid {}, mode 0600, with one link",
                m.st_uid,
                m.st_gid,
                m.st_mode & 0o7777,
                m.st_nlink,
                FileType::from_raw_mode(m.st_mode) == FileType::RegularFile,
                rustix::process::geteuid().as_raw()
            ),
        ));
    }
    Ok(())
}
