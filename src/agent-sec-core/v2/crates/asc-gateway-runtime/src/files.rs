//! Small filesystem helpers: no-follow reads with size limits and atomic replacement.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::Path;

use crate::GatewayError;

const TEMPORARY_PREFIX: &str = ".tmp-";

/// Owner applied to created files and directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Owner {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

/// Reads a regular file without following a final symlink; `Ok(None)` when absent.
pub(crate) fn read_limited(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, GatewayError> {
    read_checked(path, limit, None)
}

/// Like [`read_limited`], but the file must be owned by `uid` and have a single link.
///
/// Used for files written by the less privileged gateway account, which could
/// otherwise hard-link a file it does not own into its directory.
pub(crate) fn read_owned(
    path: &Path,
    limit: u64,
    uid: u32,
) -> Result<Option<Vec<u8>>, GatewayError> {
    read_checked(path, limit, Some(uid))
}

fn read_checked(
    path: &Path,
    limit: u64,
    owner: Option<u32>,
) -> Result<Option<Vec<u8>>, GatewayError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc_flags::NOFOLLOW_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GatewayError::io("cannot open", path, &error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| GatewayError::io("cannot inspect", path, &error))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(GatewayError::internal(format!(
            "{} is not a regular file within {limit} bytes",
            path.display()
        )));
    }
    if let Some(uid) = owner
        && (metadata.uid() != uid || metadata.nlink() != 1)
    {
        return Err(GatewayError::internal(format!(
            "{} must be owned by uid {uid} and have a single link",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| GatewayError::io("cannot read", path, &error))?;
    if bytes.len() as u64 > limit {
        return Err(GatewayError::internal(format!(
            "{} exceeds {limit} bytes",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// Reads a root-owned (or own) system file, following symlinks, such as a distribution trust store.
pub(crate) fn read_system_file(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, GatewayError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GatewayError::io("cannot open", path, &error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| GatewayError::io("cannot inspect", path, &error))?;
    let trusted_owner =
        metadata.uid() == 0 || metadata.uid() == rustix::process::geteuid().as_raw();
    if !metadata.is_file()
        || !trusted_owner
        || metadata.mode() & 0o022 != 0
        || metadata.len() > limit
    {
        return Err(GatewayError::internal(format!(
            "{} must be a root-owned file without group/other write access within {limit} bytes",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| GatewayError::io("cannot read", path, &error))?;
    Ok(Some(bytes))
}

/// Replaces `directory/name` atomically with `bytes`, durably, with the given mode and owner.
pub(crate) fn write_atomic(
    directory: &Path,
    name: &str,
    bytes: &[u8],
    mode: u32,
    owner: Option<Owner>,
) -> Result<(), GatewayError> {
    let temporary = directory.join(format!("{TEMPORARY_PREFIX}{name}"));
    let target = directory.join(name);
    let _ = std::fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc_flags::NOFOLLOW)
        .open(&temporary)
        .map_err(|error| GatewayError::io("cannot create", &temporary, &error))?;
    let written = (|| {
        if let Some(owner) = owner {
            rustix::fs::fchown(
                &file,
                Some(rustix::fs::Uid::from_raw(owner.uid)),
                Some(rustix::fs::Gid::from_raw(owner.gid)),
            )
            .map_err(std::io::Error::from)?;
        }
        // The umask may have narrowed the creation mode.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(GatewayError::io("cannot write", &temporary, &error));
    }
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(GatewayError::io("cannot replace", &target, &error));
    }
    sync_directory(directory)
}

/// Creates `path` when missing, then enforces a real directory with `mode` and `owner`.
///
/// An existing symlink, non-directory, or directory owned by someone else
/// than `owner` (or the effective user) is refused rather than repaired.
pub(crate) fn ensure_directory(
    path: &Path,
    mode: u32,
    owner: Option<Owner>,
) -> Result<(), GatewayError> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {
            if let Some(owner) = owner {
                rustix::fs::chown(
                    path,
                    Some(rustix::fs::Uid::from_raw(owner.uid)),
                    Some(rustix::fs::Gid::from_raw(owner.gid)),
                )
                .map_err(|error| GatewayError::io("cannot chown", path, &error.into()))?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(GatewayError::io("cannot create", path, &error)),
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| GatewayError::io("cannot inspect", path, &error))?;
    let expected_uid = owner.map_or_else(|| rustix::process::geteuid().as_raw(), |owner| owner.uid);
    if !metadata.is_dir() || metadata.uid() != expected_uid {
        return Err(GatewayError::internal(format!(
            "{} must be a directory owned by uid {expected_uid}",
            path.display()
        )));
    }
    if let Some(owner) = owner
        && metadata.gid() != owner.gid
    {
        rustix::fs::chown(path, None, Some(rustix::fs::Gid::from_raw(owner.gid)))
            .map_err(|error| GatewayError::io("cannot chown", path, &error.into()))?;
    }
    if metadata.mode() & 0o7777 != mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|error| GatewayError::io("cannot chmod", path, &error))?;
    }
    Ok(())
}

/// Removes a file; a missing file is not an error.
pub(crate) fn remove_file(path: &Path) -> Result<(), GatewayError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GatewayError::io("cannot remove", path, &error)),
    }
}

fn sync_directory(directory: &Path) -> Result<(), GatewayError> {
    File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| GatewayError::io("cannot sync", directory, &error))
}

mod libc_flags {
    // Raw open(2) flags: std exposes `custom_flags` but not the constants.
    pub(super) const NOFOLLOW: i32 = rustix::fs::OFlags::NOFOLLOW.bits().cast_signed();
    pub(super) const NOFOLLOW_NONBLOCK: i32 = rustix::fs::OFlags::NOFOLLOW
        .union(rustix::fs::OFlags::NONBLOCK)
        .bits()
        .cast_signed();
}
