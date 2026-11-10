//! Descriptor-pinned directories with ownership checks and atomic replacement.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use ring::rand::{SecureRandom as _, SystemRandom};
use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Gid, Mode, OFlags, fchmod, fchown, flock, fstat,
    mkdirat, open, openat, renameat, statat, unlinkat,
};

use crate::error::io_error;
use crate::secret::validate_token_value;
use crate::{CredentialStoreError, MAX_SECRET_BYTES, Secret};

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
        let file = match open_no_follow(path, None) {
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
        let parent = open_no_follow(parent, None)?;
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
        read_regular(
            file,
            &path,
            rustix::process::geteuid().as_raw(),
            Some(mode),
            limit,
        )
        .map(Some)
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

/// Reads a private token through descriptor-pinned, non-writable ancestors.
///
/// Only root or the expected owner may call this helper. The file must be regular,
/// singly linked and private, and its owner must equal `owner_uid`. A single trailing
/// CRLF, CR or LF is removed; internal whitespace is rejected, never trimmed.
///
/// # Errors
/// Returns path, ownership, permission, bounded-read or token validation errors.
pub fn read_token_file(path: &Path, owner_uid: u32) -> Result<Secret, CredentialStoreError> {
    let euid = rustix::process::geteuid().as_raw();
    if euid != 0 && euid != owner_uid {
        return Err(unsafe_path(
            path,
            "reader is neither root nor the expected owner",
        ));
    }
    let parent = token_parent(path, owner_uid)?;
    let name = path
        .file_name()
        .ok_or_else(|| unsafe_path(path, "must name a file"))?;
    let file = File::from(
        openat(
            &parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| io_error(path, error))?,
    );
    let limit = u64::try_from(MAX_SECRET_BYTES + 2)
        .map_err(|_| unsafe_path(path, "token size limit is out of range"))?;
    let bytes = read_regular(file, path, owner_uid, None, limit)?;
    let value = String::from_utf8(bytes).map_err(|_| CredentialStoreError::Invalid {
        field: "token file",
        reason: "must contain only visible ASCII characters",
    })?;
    let token = value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .or_else(|| value.strip_suffix('\r'))
        .unwrap_or(&value);
    Secret::new(token.to_owned())
}

/// Creates a new 0600 fake file without replacing any existing path.
///
/// The caller is the local CLI, not a daemon. All ancestors must be owned by
/// root or the effective user and must not be group/other writable. Validation
/// precedes creation; successful writes fsync both the file and parent directory.
///
/// # Errors
/// Returns validation, unsafe path, existing destination or write/durability errors.
pub fn write_fake_file(path: &Path, fake: &str) -> Result<(), CredentialStoreError> {
    validate_token_value(fake, "fake value")?;
    let parent = token_parent(path, rustix::process::geteuid().as_raw())?;
    let name = path
        .file_name()
        .ok_or_else(|| unsafe_path(path, "must name a file"))?;
    let mut file = File::from(
        openat(
            &parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| io_error(path, error))?,
    );
    let result = (|| {
        // Enforce the promised mode even with a restrictive CLI umask.
        fchmod(&file, Mode::from_raw_mode(0o600)).map_err(|error| io_error(path, error))?;
        file.write_all(fake.as_bytes())
            .map_err(|error| io_error(path, error))?;
        file.sync_all().map_err(|error| io_error(path, error))?;
        parent.sync_all().map_err(|error| io_error(path, error))
    })();
    if result.is_err() {
        let ours = fstat(&file).map_err(|error| io_error(path, error))?;
        let current = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| io_error(path, error))?;
        if (ours.st_dev, ours.st_ino) == (current.st_dev, current.st_ino) {
            unlinkat(&parent, name, AtFlags::empty()).map_err(|error| io_error(path, error))?;
            parent.sync_all().map_err(|error| io_error(path, error))?;
        }
    }
    result
}

fn token_parent(path: &Path, owner_uid: u32) -> Result<File, CredentialStoreError> {
    // Path::components normalizes dots and repeated separators; compare raw spelling too.
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
            _ => return Err(unsafe_path(path, "path contains traversal")),
        }
    }
    if !path.is_absolute() || normalized.as_os_str() != path.as_os_str() {
        return Err(unsafe_path(path, "must be a normalized absolute file path"));
    }
    if path.file_name().is_none() {
        return Err(unsafe_path(path, "must name a file below a directory"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_path(path, "must have a parent"))?;
    open_no_follow(parent, Some(owner_uid))
}

fn read_regular(
    mut file: File,
    path: &Path,
    owner_uid: u32,
    exact_mode: Option<rustix::fs::RawMode>,
    limit: u64,
) -> Result<Vec<u8>, CredentialStoreError> {
    let check = |stat: &rustix::fs::Stat| {
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(unsafe_path(path, "not a singly linked regular file"));
        }
        let mode = stat.st_mode & 0o7777;
        let valid_mode = exact_mode.map_or(mode & 0o7077 == 0, |expected| mode == expected);
        if stat.st_uid != owner_uid || !valid_mode {
            return Err(unsafe_path(path, "unexpected file owner or mode"));
        }
        Ok(())
    };
    let before = fstat(&file).map_err(|error| io_error(path, error))?;
    check(&before)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    let size =
        u64::try_from(bytes.len()).map_err(|_| unsafe_path(path, "file exceeds its size limit"))?;
    if size > limit {
        return Err(unsafe_path(path, "file exceeds its size limit"));
    }
    let after = fstat(&file).map_err(|error| io_error(path, error))?;
    check(&after)?;
    if before.st_size != after.st_size || u64::try_from(after.st_size).ok() != Some(size) {
        return Err(unsafe_path(path, "file changed during read"));
    }
    Ok(bytes)
}

/// Walks pinned descriptors, optionally enforcing trusted ownership on every ancestor.
fn open_no_follow(path: &Path, trusted_owner: Option<u32>) -> Result<File, CredentialStoreError> {
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
        if let Some(owner_uid) = trusted_owner {
            let stat = fstat(&current).map_err(|error| io_error(path, error))?;
            if (stat.st_uid != 0 && stat.st_uid != owner_uid) || stat.st_mode & 0o7022 != 0 {
                return Err(unsafe_path(
                    path,
                    "ancestor has untrusted ownership or write permissions",
                ));
            }
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
