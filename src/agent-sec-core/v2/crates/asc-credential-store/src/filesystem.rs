//! Descriptor-pinned directories with ownership checks and atomic replacement.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use ring::rand::{SecureRandom as _, SystemRandom};
use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Gid, Mode, OFlags, fchmod, fchown, flock, fstat,
    mkdirat, open, openat, renameat, unlinkat,
};

use crate::CredentialStoreError;
use crate::error::io_error;

const TEMPORARY_PREFIX: &str = ".tmp-";

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Permission requirement for a pinned directory.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DirectoryAccess {
    /// Exactly 0700: only the owner may list or traverse.
    OwnerOnly,
    /// No group/other write bits; readers may traverse to published files.
    OwnerWritable,
}

/// Failed [`PinnedDirectory::write_atomic`].
pub(crate) struct WriteFailure {
    /// The new file already replaced the old one; only its durability is uncertain.
    pub(crate) replaced: bool,
    pub(crate) error: CredentialStoreError,
}

/// A directory opened without following symlinks and owned by the effective user.
pub(crate) struct PinnedDirectory {
    file: File,
    path: PathBuf,
}

impl PinnedDirectory {
    /// Opens an existing directory, or returns `Ok(None)` when it is absent.
    pub(crate) fn open_existing(
        path: &Path,
        access: DirectoryAccess,
    ) -> Result<Option<Self>, CredentialStoreError> {
        let file = match open_no_follow(path) {
            Ok(file) => file,
            Err(CredentialStoreError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let directory = Self {
            file,
            path: path.into(),
        };
        directory.check(access)?;
        Ok(Some(directory))
    }

    /// Creates the final path component as a 0700 directory when missing, then opens it.
    pub(crate) fn create_owner_only(path: &Path) -> Result<Self, CredentialStoreError> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(unsafe_path(
                path,
                "must name a directory below an existing parent",
            ));
        };
        let parent = open_no_follow(parent)?;
        match mkdirat(&parent, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => {
                let created = File::from(
                    openat(&parent, name, DIRECTORY_FLAGS, Mode::empty())
                        .map_err(|error| io_error(path, error))?,
                );
                // mkdir honours the umask; set the mode explicitly before first use.
                fchmod(&created, Mode::from_raw_mode(0o700))
                    .map_err(|error| io_error(path, error))?;
                parent.sync_all().map_err(|error| io_error(path, error))?;
            }
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(io_error(path, error)),
        }
        Self::open_existing(path, DirectoryAccess::OwnerOnly)?
            .ok_or_else(|| unsafe_path(path, "disappeared while being created"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Takes an exclusive advisory lock held until this handle is dropped.
    pub(crate) fn lock_exclusive(&self) -> Result<(), CredentialStoreError> {
        match flock(&self.file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(()),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(CredentialStoreError::Locked {
                path: self.path.clone(),
            }),
            Err(error) => Err(io_error(&self.path, error)),
        }
    }

    /// Returns whether `other` is the same directory inode.
    pub(crate) fn same_directory(&self, other: &Self) -> Result<bool, CredentialStoreError> {
        let ours = fstat(&self.file).map_err(|error| io_error(&self.path, error))?;
        let theirs = fstat(&other.file).map_err(|error| io_error(&other.path, error))?;
        Ok((ours.st_dev, ours.st_ino) == (theirs.st_dev, theirs.st_ino))
    }

    /// Deletes temporary files a crashed writer left behind; they may hold real values.
    pub(crate) fn remove_temporaries(&self) -> Result<(), CredentialStoreError> {
        for entry in Dir::read_from(&self.file).map_err(|error| io_error(&self.path, error))? {
            let entry = entry.map_err(|error| io_error(&self.path, error))?;
            let name = entry.file_name();
            if name.to_bytes().starts_with(TEMPORARY_PREFIX.as_bytes()) {
                match unlinkat(&self.file, name, AtFlags::empty()) {
                    Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                    Err(error) => return Err(io_error(&self.path, error)),
                }
            }
        }
        Ok(())
    }

    fn check(&self, access: DirectoryAccess) -> Result<(), CredentialStoreError> {
        let stat = fstat(&self.file).map_err(|error| io_error(&self.path, error))?;
        if stat.st_uid != rustix::process::geteuid().as_raw() {
            return Err(unsafe_path(
                &self.path,
                "directory is not owned by the effective user",
            ));
        }
        let mode = stat.st_mode & 0o7777;
        let acceptable = match access {
            DirectoryAccess::OwnerOnly => mode == 0o700,
            DirectoryAccess::OwnerWritable => mode & 0o7022 == 0,
        };
        if !acceptable {
            return Err(unsafe_path(&self.path, "directory mode is too permissive"));
        }
        Ok(())
    }

    /// Reads a regular file that must have exactly `mode` and be owned by the effective user.
    pub(crate) fn read_file(
        &self,
        name: &str,
        mode: rustix::fs::RawMode,
        limit: u64,
    ) -> Result<Option<Vec<u8>>, CredentialStoreError> {
        let path = self.path.join(name);
        let file = match openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP) => return Err(unsafe_path(&path, "file is a symlink")),
            Err(error) => return Err(io_error(&path, error)),
        };
        let stat = fstat(&file).map_err(|error| io_error(&path, error))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(unsafe_path(&path, "not a singly linked regular file"));
        }
        if stat.st_uid != rustix::process::geteuid().as_raw() || stat.st_mode & 0o7777 != mode {
            return Err(unsafe_path(&path, "unexpected file owner or mode"));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error(&path, error))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
            return Err(unsafe_path(&path, "file exceeds its size limit"));
        }
        Ok(Some(bytes))
    }

    /// Replaces `name` atomically: readers observe either the old or the new complete file.
    pub(crate) fn write_atomic(
        &self,
        name: &str,
        bytes: &[u8],
        mode: rustix::fs::RawMode,
        group: Option<u32>,
    ) -> Result<(), WriteFailure> {
        let not_replaced = |error| WriteFailure {
            replaced: false,
            error,
        };
        let temporary = temporary_name().map_err(not_replaced)?;
        let temporary_path = self.path.join(&temporary);
        let result = (|| {
            let mut file = File::from(
                openat(
                    &self.file,
                    temporary.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(|error| io_error(&temporary_path, error))?,
            );
            if let Some(group) = group {
                fchown(&file, None, Some(Gid::from_raw(group)))
                    .map_err(|error| io_error(&temporary_path, error))?;
            }
            fchmod(&file, Mode::from_raw_mode(mode))
                .map_err(|error| io_error(&temporary_path, error))?;
            file.write_all(bytes)
                .map_err(|error| io_error(&temporary_path, error))?;
            file.sync_all()
                .map_err(|error| io_error(&temporary_path, error))?;
            renameat(&self.file, temporary.as_str(), &self.file, name)
                .map_err(|error| io_error(self.path.join(name), error))
        })();
        if let Err(error) = result {
            let _ = unlinkat(&self.file, temporary.as_str(), AtFlags::empty());
            return Err(not_replaced(error));
        }
        self.file.sync_all().map_err(|error| WriteFailure {
            replaced: true,
            error: io_error(&self.path, error),
        })
    }
}

/// Opens an absolute directory path one component at a time, refusing every symlink.
fn open_no_follow(path: &Path) -> Result<File, CredentialStoreError> {
    if !path.is_absolute() {
        return Err(unsafe_path(path, "path must be absolute"));
    }
    let mut current = File::from(
        open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|error| io_error(path, error))?,
    );
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = match openat(&current, name, DIRECTORY_FLAGS, Mode::empty()) {
                    Ok(file) => File::from(file),
                    Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                        return Err(unsafe_path(
                            path,
                            "path contains a symlink or non-directory",
                        ));
                    }
                    Err(error) => return Err(io_error(path, error)),
                };
            }
            _ => return Err(unsafe_path(path, "path contains traversal")),
        }
    }
    Ok(current)
}

fn temporary_name() -> Result<String, CredentialStoreError> {
    let mut bytes = [0_u8; 12];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CredentialStoreError::Randomness)?;
    let suffix = bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    });
    Ok(format!("{TEMPORARY_PREFIX}{suffix}"))
}

fn unsafe_path(path: &Path, reason: &'static str) -> CredentialStoreError {
    CredentialStoreError::UnsafePath {
        path: path.into(),
        reason,
    }
}
