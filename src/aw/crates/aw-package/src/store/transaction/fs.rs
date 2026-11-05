//! Descriptor-relative mutation refuses symlinks and never overwrites an unclaimed file.

use aw_exec::PackageLock;
#[cfg(test)]
mod faults;
#[cfg(test)]
pub(super) use faults::{fail_database_sync, fail_tmpfile};

use crate::store::{require, Error, Payload, Result};
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
    sync::atomic::{AtomicU64, Ordering},
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

pub(super) fn lock(path: &Path, create: bool) -> Result<PackageLock> {
    lock_flags(path, if create { libc::O_CREAT } else { 0 })
}

pub(super) fn new_lock(path: &Path) -> Result<PackageLock> {
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

fn lock_flags(path: &Path, extra: i32) -> Result<PackageLock> {
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
    let file = PackageLock::from_locked(file);
    let current = regular(path)?.metadata()?;
    require(
        current.dev() == held.dev() && current.ino() == held.ino(),
        "transaction lock replaced",
    )?;
    parent.sync_all()?;
    Ok(file)
}

pub(super) struct Staged {
    pub(super) identity: Identity,
    file: File,
    // Present only when the filesystem rejects O_TMPFILE: the named staging
    // file and the directory handle that holds it. publish renames the name
    // into place; dropping an unpublished staged file removes it again.
    fallback: Option<(File, CString)>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some((directory, name)) = &self.fallback {
            // SAFETY: name is one child of the anchored parent; unlinkat never follows symlinks.
            let _ = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        }
    }
}

pub(super) fn anonymous(
    parent: &Path,
    mode: u32,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<Staged> {
    let directory = directory(parent)?;
    let (mut file, fallback) = anonymous_file(&directory, parent, mode)?;
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
    Ok(Staged {
        identity: from_metadata(&metadata, format!("{:x}", hash.finalize())),
        file,
        fallback,
    })
}

// overlayfs and a few other filesystems reject O_TMPFILE; the caller falls
// back to a named staging file that publish moves into place with a rename.
fn unsupported_tmpfile(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EOPNOTSUPP | libc::EINVAL | libc::ENOSYS | libc::EISDIR)
    )
}

fn anonymous_file(
    directory: &File,
    parent: &Path,
    mode: u32,
) -> Result<(File, Option<(File, CString)>)> {
    let attempt = opened(
        directory,
        OsStr::new("."),
        libc::O_TMPFILE | libc::O_RDWR,
        mode,
    );
    // When armed, the fault stands in for a filesystem that rejects O_TMPFILE,
    // so the fallback runs even on filesystems that support it.
    #[cfg(test)]
    let attempt = faults::check_tmpfile().and(attempt);
    match attempt {
        Ok(file) => Ok((file, None)),
        Err(Error::Io(source)) if unsupported_tmpfile(&source) => {
            let (file, name) = named_anonymous(directory, parent)?;
            let held = directory.try_clone()?;
            Ok((file, Some((held, name))))
        }
        Err(Error::Io(source)) => Err(Error::Filesystem {
            operation: "openat(O_TMPFILE) for anonymous package staging",
            path: parent.to_owned(),
            source,
        }),
        Err(error) => Err(error),
    }
}

static ANONYMOUS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Stage without O_TMPFILE: claim a fresh private name and return it together
/// with the descriptor. The name is removed again when the staged file is
/// dropped, or consumed by the rename that publishes it.
fn named_anonymous(directory: &File, parent: &Path) -> Result<(File, CString)> {
    for _ in 0..64 {
        let sequence = ANONYMOUS_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = cstring(OsStr::new(&format!(
            ".aw-anon-{}-{}",
            std::process::id(),
            sequence
        )))?;
        // SAFETY: parent is a live directory and name is one fresh child;
        // O_EXCL claims it without following symlinks.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_RDWR
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                continue;
            }
            return Err(Error::Filesystem {
                operation: "create named staging file (O_TMPFILE fallback)",
                path: parent.to_owned(),
                source: error,
            });
        }
        // SAFETY: successful openat transferred ownership of a new descriptor.
        return Ok((unsafe { File::from_raw_fd(fd) }, name));
    }
    Err(Error::Filesystem {
        operation: "create named staging file (O_TMPFILE fallback)",
        path: parent.to_owned(),
        source: std::io::Error::from_raw_os_error(libc::EEXIST),
    })
}

pub(super) fn publish(staged: &Staged, destination: &Path) -> Result<()> {
    let (parent, name) = parent(destination)?;
    let name = cstring(name)?;
    match &staged.fallback {
        Some((directory, source_name)) => {
            // The named staging file keeps its private name until this point;
            // renameat2 moves it into place without needing privileges, unlike
            // linking a file that no directory references.
            // SAFETY: both names are one-component children of live anchored directory fds.
            if unsafe {
                libc::renameat2(
                    directory.as_raw_fd(),
                    source_name.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            directory.sync_all()?;
        }
        None => link_descriptor(&staged.file, &parent, &name)?,
    }
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

pub(super) fn write_bytes(parent: &Path, bytes: &[u8], mode: u32) -> Result<Staged> {
    anonymous(parent, mode, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_staging_falls_back_when_tmpfile_is_unsupported() {
        let root = tempfile::tempdir_in("/tmp").expect("tempdir");
        let parent = root.path().join("staging");
        std::fs::create_dir_all(&parent).expect("staging directory");
        let _fault = fail_tmpfile();
        let staged =
            write_bytes(&parent, b"fallback payload", 0o644).expect("staging must fall back");
        assert_eq!(staged.identity.mode, 0o644);
        assert_eq!(
            staged.identity.sha256,
            format!("{:x}", Sha256::digest(b"fallback payload"))
        );
        assert_eq!(
            std::fs::read_to_string(format!("/proc/self/fd/{}", staged.file.as_raw_fd()))
                .expect("read staged bytes"),
            "fallback payload"
        );
        publish(&staged, &parent.join("published")).expect("publish must move the staged file");
        assert_eq!(
            std::fs::read(parent.join("published")).expect("published bytes"),
            b"fallback payload"
        );
        assert_eq!(
            std::fs::read_dir(&parent)
                .expect("list staging directory")
                .count(),
            1,
            "staging file must leave no residue behind"
        );
    }
}
