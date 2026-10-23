//! Read-only descriptor traversal: every path component refuses symbolic links.

use super::Scope;
use crate::{require, Error, Result};
use aw_exec::PackageLock;
use std::{
    ffi::CString,
    fs::{self, File, Metadata},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path, PathBuf},
};

/// Reserved system database root; system installation is introduced separately.
pub const SYSTEM_DATABASE_ROOT: &str = "/var/lib/aw/packages";

/// Central database location, resolved without touching the filesystem.
#[derive(Clone, Debug)]
pub struct DatabasePaths {
    /// Root containing `installed.json`, `lock` and transaction recovery data.
    pub root: PathBuf,
    pub(super) scope: Scope,
}

impl DatabasePaths {
    /// Resolve the user database using an absolute XDG state directory or HOME fallback.
    ///
    /// # Errors
    /// Rejects a nonempty relative XDG path, missing HOME fallback or an unsafe path.
    pub fn user(state_home: Option<&Path>, home: Option<&Path>) -> Result<Self> {
        let root = match state_home.filter(|path| !path.as_os_str().is_empty()) {
            Some(state) => {
                absolute(state)?;
                state.join("aw/packages")
            }
            None => {
                let home = home.ok_or_else(|| {
                    Error::Invalid("HOME is required when XDG_STATE_HOME is unset".into())
                })?;
                absolute(home)?;
                home.join(".local/state/aw/packages")
            }
        };
        Ok(Self {
            root,
            scope: Scope::User,
        })
    }

    /// Describe the reserved administrator-owned database without reading or creating it.
    pub fn system() -> Self {
        Self {
            root: SYSTEM_DATABASE_ROOT.into(),
            scope: Scope::System,
        }
    }

    /// Return the committed package-record path.
    pub fn installed(&self) -> PathBuf {
        self.root.join("installed.json")
    }

    /// Explicit privilege boundary selected for this database.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Resolve an existing prefix's explicit registration without searching other databases.
    ///
    /// # Errors
    /// Rejects malformed, conflicting or unsafe registration pointers.
    pub fn for_prefix(&self, prefix: &Path) -> Result<Self> {
        match super::RegistrationPointer::read(prefix)? {
            Some(pointer) => pointer.database_paths(),
            None => Ok(self.clone()),
        }
    }
}

pub(crate) fn absolute(path: &Path) -> Result<()> {
    require(
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        format!(
            "expected an absolute path without '.' or '..': {}",
            path.display()
        ),
    )
}

fn open_at(parent: &File, name: &std::ffi::OsStr, directory: bool) -> Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let name =
        CString::new(name.as_bytes()).map_err(|_| Error::Invalid("NUL in package path".into()))?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: name is a terminated C string and parent owns a live directory fd.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(crate) fn directory(path: &Path) -> Result<File> {
    absolute(path)?;
    let mut held = File::open("/")?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            held = open_at(&held, name, true)?;
        }
    }
    Ok(held)
}

pub(crate) fn opened(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("package file has no parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Invalid("package file has no name".into()))?;
    open_at(&directory(parent)?, name, false)
}

pub(crate) fn regular(path: &Path) -> Result<File> {
    let file = opened(path)?;
    require(
        file.metadata()?.is_file(),
        format!("expected a regular package file: {}", path.display()),
    )?;
    Ok(file)
}

pub(crate) fn bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = regular(path)?;
    let before = file.metadata()?;
    require(
        before.len() <= limit,
        format!("package metadata exceeds {limit} bytes: {}", path.display()),
    )?;
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= limit,
        format!("package metadata exceeds {limit} bytes: {}", path.display()),
    )?;
    let after = file.metadata()?;
    require(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        format!("package metadata changed while reading: {}", path.display()),
    )?;
    same(path, &after)?;
    Ok(bytes)
}

pub(crate) fn lock(path: &Path) -> Result<PackageLock> {
    let file = regular(path)?;
    require(
        file.metadata()?.nlink() == 1,
        format!("package lock has multiple links: {}", path.display()),
    )?;
    // SAFETY: file owns the descriptor for the full lock lifetime; LOCK_NB bounds contention.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = PackageLock::from_locked(file);
    same(path, &file.metadata()?)?;
    Ok(file)
}

pub(crate) fn same(path: &Path, held: &Metadata) -> Result<()> {
    let current = regular(path)?.metadata()?;
    require(
        current.ino() == held.ino() && current.dev() == held.dev(),
        format!(
            "package file changed while reading; retry: {}",
            path.display()
        ),
    )
}

pub(crate) fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let held = directory(path)?;
    let before = held.metadata()?;
    let entries = fs::read_dir(path)?
        .take(4097)
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    require(
        entries.len() <= 4096,
        format!("too many package directory entries: {}", path.display()),
    )?;
    let after = directory(path)?.metadata()?;
    require(
        before.ino() == after.ino() && before.dev() == after.dev(),
        format!(
            "package directory changed while reading: {}",
            path.display()
        ),
    )?;
    Ok(entries)
}

pub(crate) fn missing(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}
