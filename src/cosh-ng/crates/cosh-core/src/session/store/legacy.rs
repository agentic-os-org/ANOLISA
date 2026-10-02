//! Workspace-owned legacy flat-directory discovery, locking, and removal.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{open, openat, unlinkat, AtFlags, Dir, Mode, OFlags};

use super::super::io::{io_error, open_file_time_ms, try_exclusive_lock, unlock_file};
use super::super::listing::ListEntry;
use super::super::{ProviderSessionId, SessionError};

pub(super) struct LegacyDirectory {
    pub(super) path: PathBuf,
    pub(super) file: File,
}

pub(super) struct LegacySessionFile<'a> {
    pub(super) directory: &'a LegacyDirectory,
    pub(super) filename: String,
    pub(super) path: PathBuf,
    pub(super) file: File,
}

/// Collects valid legacy session entries not shadowed by scoped storage.
pub(super) fn collect_legacy_list_entries(
    directory: &LegacyDirectory,
    seen_ids: &mut HashSet<ProviderSessionId>,
    entries: &mut Vec<ListEntry>,
) -> Result<(), SessionError> {
    let dir_entries = Dir::read_from(&directory.file).map_err(|error| {
        io_error(
            "list legacy sessions",
            &directory.path,
            io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    for entry in dir_entries.flatten() {
        let Ok(filename) = std::str::from_utf8(entry.file_name().to_bytes()) else {
            continue;
        };
        let Some(stem) = filename.strip_suffix(".json") else {
            continue;
        };
        let Ok(session_id) = ProviderSessionId::parse(stem) else {
            continue;
        };
        if !seen_ids.insert(session_id.clone()) {
            continue;
        }
        let Ok(Some(legacy)) = open_legacy_session_file(directory, &session_id) else {
            continue;
        };
        entries.push(ListEntry {
            modified_at_ms: open_file_time_ms(&legacy.file),
            session_id,
        });
    }
    Ok(())
}

pub(super) fn workspace_owned_legacy_dir(
    workspace: &Path,
    workspace_directory: &File,
    candidate: &Path,
) -> Option<LegacyDirectory> {
    let canonical = fs::canonicalize(candidate).ok()?;
    let metadata = fs::metadata(&canonical).ok()?;
    if !metadata.is_dir() || !canonical.starts_with(workspace) {
        return None;
    }
    let relative = canonical.strip_prefix(workspace).ok()?;
    let file = open_relative_directory_no_follow(workspace_directory, relative).ok()?;
    file.metadata()
        .ok()
        .is_some_and(|metadata| metadata.is_dir())
        .then_some(LegacyDirectory {
            path: canonical,
            file,
        })
}

pub(super) fn open_directory_path_no_follow(path: &Path) -> io::Result<File> {
    let descriptor = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(rustix_error_to_io)?;
    let root = File::from(descriptor);
    let relative = path
        .strip_prefix("/")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path is not absolute"))?;
    open_relative_directory_no_follow(&root, relative)
}

fn open_relative_directory_no_follow(root: &File, relative: &Path) -> io::Result<File> {
    let mut directory = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path contains a non-normal component",
            ));
        };
        let descriptor = openat(
            &directory,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(rustix_error_to_io)?;
        directory = File::from(descriptor);
    }
    Ok(directory)
}

fn rustix_error_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

pub(super) fn collect_session_ids_from_legacy_directory(
    directory: &LegacyDirectory,
    session_ids: &mut HashSet<ProviderSessionId>,
) -> Result<(), SessionError> {
    let entries = Dir::read_from(&directory.file).map_err(|error| {
        io_error(
            "list legacy IDs",
            &directory.path,
            io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    for entry in entries.flatten() {
        let Ok(filename) = std::str::from_utf8(entry.file_name().to_bytes()) else {
            continue;
        };
        let Some(stem) = filename.strip_suffix(".json") else {
            continue;
        };
        let Ok(session_id) = ProviderSessionId::parse(stem) else {
            continue;
        };
        if open_legacy_session_file(directory, &session_id).is_ok_and(|entry| entry.is_some()) {
            session_ids.insert(session_id);
        }
    }
    Ok(())
}

pub(super) fn open_legacy_session_file<'a>(
    directory: &'a LegacyDirectory,
    session_id: &ProviderSessionId,
) -> Result<Option<LegacySessionFile<'a>>, SessionError> {
    let filename = format!("{session_id}.json");
    let path = directory.path.join(&filename);
    let descriptor = match openat(
        &directory.file,
        filename.as_str(),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP) => {
            return Err(SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: "legacy session entry is a symbolic link".to_string(),
            });
        }
        Err(error) => {
            return Err(io_error(
                "open legacy session",
                &path,
                io::Error::from_raw_os_error(error.raw_os_error()),
            ));
        }
    };
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect legacy session", &path, error))?;
    if !metadata.is_file() {
        return Err(SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: "legacy session entry is not a regular file".to_string(),
        });
    }
    Ok(Some(LegacySessionFile {
        directory,
        filename,
        path,
        file,
    }))
}

pub(super) fn lock_legacy_session_file<'a>(
    legacy: LegacySessionFile<'a>,
    session_id: &ProviderSessionId,
) -> Result<LegacySessionFile<'a>, SessionError> {
    match try_exclusive_lock(&legacy.file) {
        Ok(true) => Ok(legacy),
        Ok(false) => Err(SessionError::Conflict {
            session_id: session_id.to_string(),
        }),
        Err(error) => Err(io_error("lock legacy session", &legacy.path, error)),
    }
}

pub(super) fn remove_locked_legacy_session_file(
    legacy: LegacySessionFile<'_>,
    session_id: &ProviderSessionId,
) -> Result<(), SessionError> {
    let result = unlinkat(
        &legacy.directory.file,
        legacy.filename.as_str(),
        AtFlags::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::NOENT {
            SessionError::NotFound {
                session_id: session_id.to_string(),
            }
        } else {
            io_error(
                "remove",
                &legacy.path,
                io::Error::from_raw_os_error(error.raw_os_error()),
            )
        }
    });
    unlock_file(&legacy.file);
    drop(legacy);
    result
}
