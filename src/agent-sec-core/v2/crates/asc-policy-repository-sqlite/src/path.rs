//! Owned directory and database identity checks, held through daemon drain.
use crate::RepositoryError;
use rustix::fs::{
    AtFlags, Dev, FileType, FlockOperation, Mode, OFlags, Stat, flock, fstat, lstat, mkdirat, open,
    openat, statat,
};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

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
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || path.file_name().is_none_or(|n| n != "policy-state.db")
        {
            return Err(RepositoryError::UnsafePath);
        }
        let uid = rustix::process::geteuid().as_raw();
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut directory = File::from(open("/", flags, Mode::empty())?);
        for part in path
            .parent()
            .ok_or(RepositoryError::UnsafePath)?
            .components()
        {
            if let Component::Normal(name) = part {
                match openat(&directory, name, flags, Mode::empty()) {
                    Ok(fd) => directory = File::from(fd),
                    Err(rustix::io::Errno::NOENT) => {
                        mkdirat(&directory, name, Mode::RWXU)?;
                        directory.sync_all()?;
                        directory = File::from(openat(&directory, name, flags, Mode::empty())?);
                    }
                    Err(error) => return Err(error.into()),
                }
                let m = directory.metadata()?;
                let sticky_root = m.uid() == 0 && m.mode() & 0o1000 != 0;
                if (m.uid() != uid && m.uid() != 0) || (m.mode() & 0o022 != 0 && !sticky_root) {
                    return Err(RepositoryError::UnsafePath);
                }
            }
        }
        let m = directory.metadata()?;
        if m.uid() != uid || m.mode() & 0o777 != 0o700 {
            return Err(RepositoryError::UnsafePath);
        }
        let lock = owned_file(&directory, "policy-state.lock", true)?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                RepositoryError::AlreadyOpen
            } else {
                e.into()
            }
        })?;
        let db = owned_file(&directory, "policy-state.db", true)?;
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
        validate_file(&m)?;
        let visible = lstat(&self.path)?;
        if (m.st_dev, m.st_ino) != self.identity
            || (visible.st_dev, visible.st_ino) != self.identity
            || FileType::from_raw_mode(visible.st_mode) == FileType::Symlink
        {
            return Err(RepositoryError::UnsafePath);
        }
        for name in ["policy-state.db-wal", "policy-state.db-shm"] {
            match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(m) => validate_file(&m)?,
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

fn owned_file(directory: &File, name: &str, create: bool) -> Result<File, RepositoryError> {
    let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if create {
        flags |= OFlags::CREATE;
    }
    let file = File::from(openat(directory, name, flags, Mode::RUSR | Mode::WUSR)?);
    validate_file(&fstat(&file)?)?;
    Ok(file)
}

fn validate_file(m: &Stat) -> Result<(), RepositoryError> {
    if FileType::from_raw_mode(m.st_mode) != FileType::RegularFile
        || m.st_nlink != 1
        || m.st_uid != rustix::process::geteuid().as_raw()
        || m.st_mode & 0o7777 != 0o600
    {
        return Err(RepositoryError::UnsafePath);
    }
    Ok(())
}
