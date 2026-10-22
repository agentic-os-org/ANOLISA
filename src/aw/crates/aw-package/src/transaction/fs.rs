//! Descriptor-relative mutation refuses symlinks and never overwrites an unclaimed file.

#[cfg(test)]
mod faults;
#[cfg(test)]
pub(super) use faults::fail_database_sync;

use crate::{require, Error, Payload, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

pub(super) const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub sha256: String,
    pub mode: u32,
}

pub(super) fn absolute(path: &Path) -> Result<()> {
    require(
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        format!("unsafe absolute package path: {}", path.display()),
    )
}

pub(super) fn relative(path: &Path) -> Result<()> {
    require(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        format!("unsafe relative transaction path: {}", path.display()),
    )
}

fn cstring(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| Error::Invalid("NUL in package path".into()))
}

fn opened(parent: &File, name: &OsStr, flags: i32, mode: u32) -> Result<File> {
    let name = cstring(name)?;
    // SAFETY: parent is a live directory and name is terminated; the returned fd is new.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful openat transferred ownership of a new descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn directory(path: &Path) -> Result<File> {
    absolute(path)?;
    let mut held = File::open("/")?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            held = opened(&held, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        }
    }
    Ok(held)
}

fn parent(path: &Path) -> Result<(File, &OsStr)> {
    absolute(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Invalid("transaction path has no name".into()))?;
    Ok((
        directory(
            path.parent()
                .ok_or_else(|| Error::Invalid("transaction path has no parent".into()))?,
        )?,
        name,
    ))
}

pub(super) fn regular(path: &Path) -> Result<File> {
    let (parent, name) = parent(path)?;
    let file = opened(&parent, name, libc::O_RDONLY, 0)?;
    require(
        file.metadata()?.is_file(),
        format!("transaction requires a regular file: {}", path.display()),
    )?;
    Ok(file)
}

pub(super) fn exists(path: &Path) -> Result<bool> {
    match regular(path) {
        Ok(_) => Ok(true),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = regular(path)?;
    let before = file.metadata()?;
    require(
        before.len() <= limit,
        format!(
            "transaction metadata exceeds {limit} bytes: {}",
            path.display()
        ),
    )?;
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= limit,
        "transaction metadata grew beyond limit",
    )?;
    stable(path, &before, &file.metadata()?)?;
    Ok(bytes)
}

pub(super) fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    match read(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn identity(path: &Path) -> Result<Identity> {
    let mut file = regular(path)?;
    let before = file.metadata()?;
    require(
        before.len() <= MAX_FILE_BYTES,
        format!(
            "package file exceeds verification limit: {}",
            path.display()
        ),
    )?;
    let mut hash = Sha256::new();
    let count = std::io::copy(&mut (&mut file).take(MAX_FILE_BYTES + 1), &mut hash)?;
    require(
        count <= MAX_FILE_BYTES,
        "package file grew beyond verification limit",
    )?;
    stable(path, &before, &file.metadata()?)?;
    Ok(from_metadata(&before, format!("{:x}", hash.finalize())))
}

pub(super) fn directory_identity(path: &Path) -> Result<Identity> {
    Ok(from_metadata(&directory(path)?.metadata()?, String::new()))
}

fn from_metadata(metadata: &Metadata, sha256: String) -> Identity {
    Identity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        sha256,
        mode: metadata.mode() & 0o7777,
    }
}

fn stable(path: &Path, before: &Metadata, after: &Metadata) -> Result<()> {
    let current = regular(path)?.metadata()?;
    require(
        before.dev() == current.dev()
            && before.ino() == current.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        format!("package file changed while reading: {}", path.display()),
    )
}

pub(super) fn verify(path: &Path, payload: &Payload) -> Result<Identity> {
    let identity = identity(path)?;
    require(
        identity.sha256 == payload.sha256 && identity.mode == payload.mode,
        format!("owned package file changed: {}", path.display()),
    )?;
    Ok(identity)
}

pub(super) fn same(path: &Path, expected: &Identity) -> Result<bool> {
    match identity(path) {
        Ok(actual) => Ok(&actual == expected),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn mkdir(path: &Path, mode: u32) -> Result<()> {
    let (parent, name) = parent(path)?;
    let name = cstring(name)?;
    // SAFETY: parent and name identify one child; no path traversal is performed.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let created = directory(path)?;
    // SAFETY: the newly-created descriptor is owned; explicit package modes ignore umask.
    if unsafe { libc::fchmod(created.as_raw_fd(), mode) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    created.sync_all()?;
    parent.sync_all()?;
    Ok(())
}

pub(super) fn owned(path: &Path) -> Result<()> {
    let metadata = directory(path)?.metadata()?;
    // SAFETY: geteuid observes the current process identity.
    require(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o022 == 0,
        format!(
            "package directory must be owned and not group/world writable: {}",
            path.display()
        ),
    )
}

pub(super) fn ensure_tree(path: &Path, mode: u32) -> Result<()> {
    absolute(path)?;
    match directory(path) {
        Ok(_) => owned(path),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_tree(
                path.parent()
                    .ok_or_else(|| Error::Invalid("no writable database ancestor".into()))?,
                mode,
            )?;
            mkdir(path, mode)?;
            owned(path)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn lock(path: &Path, create: bool) -> Result<File> {
    lock_flags(path, if create { libc::O_CREAT } else { 0 })
}

pub(super) fn new_lock(path: &Path) -> Result<File> {
    lock_flags(path, libc::O_CREAT | libc::O_EXCL)
}

pub(super) fn make_readable(path: &Path, before: &Identity, after: &Identity) -> Result<bool> {
    if same(path, after).unwrap_or(false) {
        return Ok(true);
    }
    if !same(path, before).unwrap_or(false) {
        return Ok(false);
    }
    let file = regular(path)?;
    let metadata = file.metadata()?;
    require(
        before.mode == 0o600
            && after.mode == 0o644
            && before.dev == after.dev
            && before.ino == after.ino
            && before.sha256 == bytes_payload(b"", 0o600).sha256
            && before.sha256 == after.sha256,
        "invalid system lock permission transition",
    )?;
    if metadata.len() != 0
        || metadata.dev() != before.dev
        || metadata.ino() != before.ino
        || metadata.nlink() != 1
    {
        return Ok(false);
    }
    // SAFETY: this is the EX-locked original empty inode recorded before transaction actions.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o644) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    same(path, after)
}

fn lock_flags(path: &Path, extra: i32) -> Result<File> {
    let (parent, name) = parent(path)?;
    let file = opened(&parent, name, libc::O_RDWR | extra, 0o644)?;
    if extra & libc::O_CREAT != 0 {
        // SAFETY: new locks remain readable by non-owner consumers of system prefixes.
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o644) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        file.sync_all()?;
    }
    let held = file.metadata()?;
    // SAFETY: geteuid observes the process identity; file remains owned while locked.
    require(
        held.is_file()
            && held.uid() == unsafe { libc::geteuid() }
            && held.nlink() == 1
            && held.mode() & 0o022 == 0,
        format!("unsafe transaction lock: {}", path.display()),
    )?;
    // SAFETY: file owns this descriptor for the entire lock lifetime.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
            Error::InUse(path.into())
        } else {
            error.into()
        });
    }
    let current = regular(path)?.metadata()?;
    require(
        current.dev() == held.dev() && current.ino() == held.ino(),
        "transaction lock replaced",
    )?;
    parent.sync_all()?;
    Ok(file)
}

pub(super) fn anonymous(
    parent: &Path,
    mode: u32,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<(File, Identity)> {
    let directory = directory(parent)?;
    let mut file = opened(
        &directory,
        OsStr::new("."),
        libc::O_TMPFILE | libc::O_RDWR,
        mode,
    )
    .map_err(|error| match error {
        Error::Io(source) => Error::Filesystem {
            operation: "openat(O_TMPFILE) for anonymous package staging",
            path: parent.to_owned(),
            source,
        },
        error => error,
    })?;
    write(&mut file)?;
    // SAFETY: the file descriptor is live; mode contains only ordinary permission bits.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    let metadata = file.metadata()?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok((
        file,
        from_metadata(&metadata, format!("{:x}", hash.finalize())),
    ))
}

pub(super) fn publish(file: &File, destination: &Path) -> Result<()> {
    let (parent, name) = parent(destination)?;
    let name = cstring(name)?;
    link_descriptor(file, &parent, &name)?;
    parent.sync_all()?;
    Ok(())
}

fn link_descriptor(file: &File, parent: &File, name: &CString) -> Result<()> {
    let descriptor = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| Error::Invalid("invalid descriptor link".into()))?;
    // SAFETY: procfs resolves our held descriptor; AT_SYMLINK_FOLLOW needs no privileged AT_EMPTY_PATH.
    if unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            descriptor.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub(super) fn move_noreplace(source: &Path, destination: &Path) -> Result<()> {
    let (from, source_name) = parent(source)?;
    let (to, destination_name) = parent(destination)?;
    let source_name = cstring(source_name)?;
    let destination_name = cstring(destination_name)?;
    // SAFETY: both names are one-component children of live anchored directory fds.
    if unsafe {
        libc::renameat2(
            from.as_raw_fd(),
            source_name.as_ptr(),
            to.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    from.sync_all()?;
    to.sync_all()?;
    Ok(())
}

pub(super) fn link_owned(source: &Path, destination: &Path, expected: &Identity) -> Result<()> {
    let source_file = regular(source)?;
    require(same(source, expected)?, "owned file changed before backup")?;
    let held = source_file.metadata()?;
    require(
        held.dev() == expected.dev && held.ino() == expected.ino,
        "owned file replaced before backup",
    )?;
    let (parent, name) = parent(destination)?;
    let name = cstring(name)?;
    link_descriptor(&source_file, &parent, &name)?;
    parent.sync_all()?;
    require(same(destination, expected)?, "backup file changed")
}

pub(super) fn exchange(source: &Path, destination: &Path) -> Result<()> {
    let (from, source_name) = parent(source)?;
    let (to, destination_name) = parent(destination)?;
    let source_name = cstring(source_name)?;
    let destination_name = cstring(destination_name)?;
    // SAFETY: exchange atomically swaps anchored children; neither namespace entry disappears.
    if unsafe {
        libc::renameat2(
            from.as_raw_fd(),
            source_name.as_ptr(),
            to.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    from.sync_all()?;
    to.sync_all()?;
    Ok(())
}

pub(super) fn replace(source: &Path, destination: &Path) -> Result<()> {
    let (from, source_name) = parent(source)?;
    let (to, destination_name) = parent(destination)?;
    let source_name = cstring(source_name)?;
    let destination_name = cstring(destination_name)?;
    // SAFETY: both names are anchored children and the caller has checked replacement authority.
    if unsafe {
        libc::renameat(
            from.as_raw_fd(),
            source_name.as_ptr(),
            to.as_raw_fd(),
            destination_name.as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    sync_parents(&from, &to, destination)
}

/// Confirm a visible replacement before retiring recovery evidence after a failed commit.
pub(super) fn confirm_replacement(source_directory: &Path, destination: &Path) -> Result<()> {
    let from = directory(source_directory)?;
    let (to, _) = parent(destination)?;
    sync_parents(&from, &to, destination)
}

fn sync_parents(from: &File, to: &File, _destination: &Path) -> Result<()> {
    from.sync_all()?;
    #[cfg(test)]
    faults::check_database_sync(_destination)?;
    to.sync_all()?;
    Ok(())
}

pub(super) fn unlink(path: &Path, expected: &Identity) -> Result<()> {
    require(
        same(path, expected)?,
        format!(
            "refusing to remove replaced recovery file: {}",
            path.display()
        ),
    )?;
    let (parent, name) = parent(path)?;
    let name = cstring(name)?;
    // SAFETY: name is one child of the anchored parent; this never follows symlinks.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    parent.sync_all()?;
    Ok(())
}

pub(super) fn rmdir(path: &Path, expected: Option<&Identity>) -> Result<bool> {
    let actual = match directory_identity(path) {
        Ok(identity) => identity,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if expected.is_none_or(|expected| actual.dev != expected.dev || actual.ino != expected.ino) {
        return Ok(false);
    }
    let (parent, name) = parent(path)?;
    let name = cstring(name)?;
    // SAFETY: AT_REMOVEDIR only removes an empty child directory and follows no symlink.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) {
            return Ok(false);
        }
        return Err(error.into());
    }
    parent.sync_all()?;
    Ok(true)
}

pub(super) fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let held = directory_identity(path)?;
    let entries = std::fs::read_dir(path)?
        .take(8193)
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    require(
        entries.len() <= 8192,
        "too many transaction directory entries",
    )?;
    let after = directory_identity(path)?;
    require(
        held.dev == after.dev && held.ino == after.ino,
        "transaction directory replaced",
    )?;
    Ok(entries)
}

pub(super) fn bytes_payload(bytes: &[u8], mode: u32) -> Payload {
    Payload {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        mode,
    }
}

pub(super) fn write_bytes(parent: &Path, bytes: &[u8], mode: u32) -> Result<(File, Identity)> {
    anonymous(parent, mode, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}
