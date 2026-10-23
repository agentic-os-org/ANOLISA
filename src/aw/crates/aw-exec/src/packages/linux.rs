//! Nonblocking shared flock over stable installation directories and lock inodes.

use super::{LeaseError, PackageLeases};
use crate::{CommandSpec, PackageLock};
use std::{
    collections::BTreeSet,
    ffi::{CString, OsStr},
    fs::{self, File, Metadata},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, LeaseError>;

fn io(path: &Path, source: io::Error) -> LeaseError {
    LeaseError::Io {
        path: path.to_owned(),
        source,
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(LeaseError::Invalid("an absolute path is required"));
    }
    // Parent components must follow kernel symlink semantics, not lexical cleanup.
    Ok(path.to_owned())
}

fn resolved(command: &CommandSpec) -> Result<(PathBuf, PathBuf)> {
    if !command.cwd.is_absolute() {
        return Err(LeaseError::Invalid("working directory must be absolute"));
    }
    let program = &command.program;
    let selected = if program.is_absolute() {
        program.clone()
    } else if program.as_os_str().as_bytes().contains(&b'/') {
        command.cwd.join(program)
    } else {
        let path = command
            .environment
            .get(OsStr::new("PATH"))
            .ok_or(LeaseError::Invalid(
                "bare executable requires an explicit PATH",
            ))?;
        std::env::split_paths(path)
            .map(|directory| {
                if directory.is_absolute() {
                    directory.join(program)
                } else {
                    command.cwd.join(directory).join(program)
                }
            })
            .find(|path| {
                fs::metadata(path)
                    .is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
            })
            .ok_or_else(|| LeaseError::Executable(program.clone()))?
    };
    match fs::canonicalize(&selected) {
        Ok(path) => Ok((absolute(&selected)?, path)),
        // Preserve the executor's existing missing-absolute-program diagnostic;
        // its existing managed ancestors still need protection before the attempt.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let selected = absolute(&selected)?;
            Ok((selected.clone(), selected))
        }
        Err(error) => Err(io(&selected, error)),
    }
}

pub(super) fn commands<'a>(
    commands: impl IntoIterator<Item = &'a mut CommandSpec>,
) -> Result<PackageLeases> {
    let mut references = Vec::new();
    for command in commands {
        if command.program.is_absolute() {
            references.push(command.program.clone());
        } else if command.program.as_os_str().as_bytes().contains(&b'/') {
            references.push(command.cwd.join(&command.program));
        }
        let (selected, resolved) = resolved(command)?;
        references.push(resolved);
        // Preserve symlink-sensitive invocation, including Python virtualenvs.
        command.program = selected;
        references.push(command.program.clone());
        for argument in &command.args {
            let bytes = argument.as_bytes();
            let value = if bytes.starts_with(b"/") {
                Some(argument.as_os_str())
            } else {
                bytes
                    .iter()
                    .position(|byte| *byte == b'=')
                    .map(|index| &bytes[index + 1..])
                    .filter(|value| value.starts_with(b"/"))
                    .map(OsStr::from_bytes)
            };
            if let Some(path) = value {
                references.push(PathBuf::from(path));
            }
        }
    }
    paths(references)
}

pub(super) fn paths(paths: impl IntoIterator<Item = PathBuf>) -> Result<PackageLeases> {
    let mut candidates = BTreeSet::new();
    for path in paths {
        discover(&absolute(&path)?, &mut candidates)?;
        let resolved = existing_ancestor(&path)?;
        if resolved != path {
            discover(&resolved, &mut candidates)?;
        }
    }
    let mut held = Vec::new();
    for prefix in candidates {
        held.push(acquire(&prefix)?);
    }
    Ok(PackageLeases { _held: held })
}

fn existing_ancestor(path: &Path) -> Result<PathBuf> {
    for ancestor in path.ancestors() {
        match fs::canonicalize(ancestor) {
            Ok(resolved) => return Ok(resolved),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::symlink_metadata(ancestor) {
                    // An existing dangling alias has unknown target ancestry.
                    Ok(_) => return Err(io(ancestor, error)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io(ancestor, error)),
                }
            }
            Err(error) => return Err(io(ancestor, error)),
        }
    }
    Err(LeaseError::Invalid(
        "absolute path has no existing ancestor",
    ))
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => Ok(false),
        Err(error) => Err(io(path, error)),
    }
}

fn discover(path: &Path, prefixes: &mut BTreeSet<PathBuf>) -> Result<()> {
    for prefix in path.ancestors() {
        if exists(&prefix.join(".aw-packages"))? || exists(&prefix.join(".aw-install.lock"))? {
            prefixes.insert(fs::canonicalize(prefix).map_err(|error| io(prefix, error))?);
        }
    }
    Ok(())
}

fn open_at(parent: &File, name: &OsStr, directory: bool, path: &Path) -> Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| LeaseError::Invalid("NUL in a package reference"))?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent retains a live directory fd and name is a terminated string.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io(path, io::Error::last_os_error()));
    }
    // SAFETY: openat returned one new descriptor owned by this File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn metadata(file: &File, path: &Path) -> Result<Metadata> {
    file.metadata().map_err(|error| io(path, error))
}

fn safe(metadata: &Metadata, sticky_ancestor: bool) -> bool {
    // SAFETY: geteuid reads process identity without touching caller memory.
    let uid = unsafe { libc::geteuid() };
    (metadata.uid() == uid || metadata.uid() == 0)
        && (metadata.mode() & 0o022 == 0
            || (sticky_ancestor && metadata.uid() == 0 && metadata.mode() & 0o1000 != 0))
}

fn directory(path: &Path) -> Result<File> {
    let mut current = File::open("/").map_err(|error| io(Path::new("/"), error))?;
    let mut traversed = PathBuf::from("/");
    for part in path.components() {
        if let Component::Normal(name) = part {
            traversed.push(name);
            current = open_at(&current, name, true, &traversed)?;
            if !safe(&metadata(&current, &traversed)?, traversed != path) {
                return Err(LeaseError::Unsafe(traversed));
            }
        }
    }
    Ok(current)
}

fn same(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn acquire(prefix: &Path) -> Result<PackageLock> {
    let root = directory(prefix)?;
    let records_path = prefix.join(".aw-packages");
    let records = open_at(&root, OsStr::new(".aw-packages"), true, &records_path)?;
    if !safe(&metadata(&records, &records_path)?, false) {
        return Err(LeaseError::Unsafe(prefix.to_owned()));
    }
    let lock_path = prefix.join(".aw-install.lock");
    let lock = open_at(&root, OsStr::new(".aw-install.lock"), false, &lock_path)?;
    let before = metadata(&lock, &lock_path)?;
    let root_metadata = metadata(&root, prefix)?;
    if !before.is_file()
        || before.nlink() != 1
        || before.uid() != root_metadata.uid()
        || !safe(&before, false)
    {
        return Err(LeaseError::Unsafe(prefix.to_owned()));
    }
    // SAFETY: the read-only live fd is retained for the entire shared-lock lifetime.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(if error.kind() == io::ErrorKind::WouldBlock {
            LeaseError::Busy(prefix.to_owned())
        } else {
            io(&lock_path, error)
        });
    }
    let lock = PackageLock::from_locked(lock);
    let fresh_root = directory(prefix)?;
    let fresh_lock = open_at(
        &fresh_root,
        OsStr::new(".aw-install.lock"),
        false,
        &lock_path,
    )?;
    let fresh_records = open_at(&fresh_root, OsStr::new(".aw-packages"), true, &records_path)?;
    if !same(&root_metadata, &metadata(&fresh_root, prefix)?)
        || !same(&before, &metadata(&fresh_lock, &lock_path)?)
        || !same(
            &metadata(&records, &records_path)?,
            &metadata(&fresh_records, &records_path)?,
        )
        || lock
            .metadata()
            .map_err(|error| io(&lock_path, error))?
            .nlink()
            != 1
    {
        return Err(LeaseError::Changed(prefix.to_owned()));
    }
    Ok(lock)
}
