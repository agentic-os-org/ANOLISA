//! Bounded file I/O, private permissions, and advisory session locks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{flock, FlockOperation};

use super::{ProviderSessionId, SessionError};

pub(super) const MAX_SESSION_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    static SESSION_FILE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_session_file_read_count() {
    SESSION_FILE_READS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn session_file_read_count() -> usize {
    SESSION_FILE_READS.with(std::cell::Cell::get)
}

pub(super) fn read_bounded_session_file(
    path: &Path,
    session_id: &ProviderSessionId,
) -> Result<Vec<u8>, SessionError> {
    let file = File::open(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            SessionError::NotFound {
                session_id: session_id.to_string(),
            }
        } else {
            io_error("open session", path, error)
        }
    })?;
    read_bounded_open_session_file(file, path, session_id)
}

pub(super) fn read_bounded_open_session_file(
    file: File,
    path: &Path,
    session_id: &ProviderSessionId,
) -> Result<Vec<u8>, SessionError> {
    #[cfg(test)]
    SESSION_FILE_READS.with(|count| count.set(count.get().saturating_add(1)));
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect session size", path, error))?;
    if metadata.len() > MAX_SESSION_FILE_BYTES {
        return Err(SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: format!(
                "session file exceeds the {} byte safety limit",
                MAX_SESSION_FILE_BYTES
            ),
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SESSION_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read session", path, error))?;
    if bytes.len() as u64 > MAX_SESSION_FILE_BYTES {
        return Err(SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: format!(
                "session file exceeds the {} byte safety limit",
                MAX_SESSION_FILE_BYTES
            ),
        });
    }
    Ok(bytes)
}

pub(super) fn expand_persist_dir(value: &str, workspace: &Path) -> PathBuf {
    if value == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(relative) = value.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(relative);
    }
    let path = PathBuf::from(value);
    if path.is_relative() {
        workspace.join(path)
    } else {
        path
    }
}

pub(super) fn create_private_dir(path: &Path) -> Result<(), SessionError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|error| io_error("create private directory", path, error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("set private directory permissions", path, error))?;
    Ok(())
}

pub(super) fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

pub(super) fn enforce_private_file_mode(file: &File, path: &Path) -> Result<(), SessionError> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error("set private file permissions", path, error))?;
    Ok(())
}

pub(super) fn write_atomic_file(
    temp_path: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), SessionError> {
    let mut file = private_open_options()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .map_err(|error| io_error("create temporary file", temp_path, error))?;
    file.write_all(bytes)
        .map_err(|error| io_error("write temporary file", temp_path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync temporary file", temp_path, error))?;
    fs::rename(temp_path, destination).map_err(|error| io_error("replace", destination, error))?;
    if let Some(parent) = destination.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_error("sync directory", parent, error))?;
    }
    Ok(())
}

pub(super) fn io_error(operation: &'static str, path: &Path, error: io::Error) -> SessionError {
    SessionError::Io {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

pub(super) fn file_time_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_ms)
        .unwrap_or_default()
}

pub(super) fn open_file_time_ms(file: &File) -> u64 {
    file.metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_ms)
        .unwrap_or_default()
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Attempts a non-blocking exclusive advisory lock; `Ok(false)` means another holder.
pub(super) fn try_exclusive_lock(file: &File) -> io::Result<bool> {
    match flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(false),
        Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
    }
}

pub(super) fn unlock_file(file: &File) {
    let _ = flock(file, FlockOperation::Unlock);
}

pub(super) struct SessionLock {
    pub(super) file: File,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // Explicit unlock also releases locks temporarily inherited across a concurrent fork.
        unlock_file(&self.file);
    }
}
