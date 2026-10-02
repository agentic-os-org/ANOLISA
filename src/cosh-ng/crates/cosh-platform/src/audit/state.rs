//! Last-observer operational state for audit health diagnostics.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosh_types::audit::AuditSettings;
use cosh_types::error::{CoshError, ErrorCode};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::reader::open_private_read;
use super::store::{create_private_file, ensure_private_dir};

/// Safe error summary persisted in operational state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditStateError {
    /// Bounded operation category.
    pub operation: String,
    /// Stable error code without event or path content.
    pub code: String,
    /// Observation timestamp.
    pub occurred_at: DateTime<Utc>,
}

/// Non-authoritative last-observer cache for audit health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditOperationalState {
    /// Operational-state schema version.
    pub schema_version: u16,
    /// Effective settings and their sources.
    pub settings: AuditSettings,
    /// Last successful durable record timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_write: Option<DateTime<Utc>>,
    /// Last bounded write failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_write_error: Option<AuditStateError>,
    /// Last bounded retention failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_retention_error: Option<AuditStateError>,
    /// Last successful retention pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_retention_success: Option<DateTime<Utc>>,
    /// Last bounded export failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_export_error: Option<AuditStateError>,
}

impl AuditOperationalState {
    /// Creates an empty version 1 operational cache.
    pub fn new(settings: AuditSettings) -> Self {
        Self {
            schema_version: 1,
            settings,
            last_successful_write: None,
            last_write_error: None,
            last_retention_error: None,
            last_retention_success: None,
            last_export_error: None,
        }
    }
}

/// Returns the fixed operational-state path below one audit root.
pub fn state_path(root: &Path) -> PathBuf {
    root.join("v1/state.json")
}

/// Reads operational state if it exists.
///
/// # Errors
///
/// Returns a stable corrupt-state error for unreadable, oversized, or invalid
/// JSON. State is diagnostic only and is never used as authorization input.
pub fn read_state(root: &Path) -> Result<Option<AuditOperationalState>, CoshError> {
    let path = state_path(root);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(state_io_error("inspect state", &path, error)),
    }
    let mut file = open_private_read(&path)?;
    let metadata = file
        .metadata()
        .map_err(|error| state_io_error("inspect state", &path, error))?;
    if metadata.len() > 256 * 1024 {
        return Err(state_error("state file exceeds 256 KiB"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| state_io_error("read state", &path, error))?;
    let state: AuditOperationalState =
        serde_json::from_slice(&bytes).map_err(|_| state_error("state JSON is invalid"))?;
    if state.schema_version != 1 {
        return Err(state_error("state schema version is unsupported"));
    }
    Ok(Some(state))
}

/// Atomically replaces private operational state.
///
/// # Errors
///
/// Returns a safe audit error for serialization, create-new, write, sync, or
/// rename failures.
pub fn write_state(root: &Path, state: &AuditOperationalState) -> Result<(), CoshError> {
    update_state(root, state.settings.clone(), |current| {
        *current = state.clone();
    })
}

/// Updates operational state while holding a lock on the stable version directory.
///
/// The directory descriptor remains stable across atomic `state.json` replaces,
/// preventing independent writers from losing fields owned by another subsystem.
///
/// # Errors
///
/// Returns a safe audit error when locking, reading, or publishing state fails.
pub fn update_state(
    root: &Path,
    settings: AuditSettings,
    update: impl FnOnce(&mut AuditOperationalState),
) -> Result<(), CoshError> {
    let directory = root.join("v1");
    ensure_private_dir(root)?;
    ensure_private_dir(&directory)?;
    let lock_file = open_state_directory(&directory)?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, error)| state_error(format!("lock state directory: {error}")))?;
    let mut state = read_state(root)?.unwrap_or_else(|| AuditOperationalState::new(settings));
    update(&mut state);
    write_state_unlocked(root, &state)
}

fn write_state_unlocked(root: &Path, state: &AuditOperationalState) -> Result<(), CoshError> {
    let directory = root.join("v1");
    let final_path = state_path(root);
    let temporary = directory.join(format!(".state-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let bytes =
            serde_json::to_vec(state).map_err(|_| state_error("state serialization failed"))?;
        let mut file = create_private_file(&temporary)?;
        file.write_all(&bytes)
            .map_err(|error| state_io_error("write state", &temporary, error))?;
        file.sync_data()
            .map_err(|error| state_io_error("sync state", &temporary, error))?;
        std::fs::rename(&temporary, &final_path)
            .map_err(|error| state_io_error("publish state", &temporary, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn open_state_directory(path: &Path) -> Result<File, CoshError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW);
    options
        .open(path)
        .map_err(|error| state_io_error("open state directory", path, error))
}

fn state_error(message: impl Into<String>) -> CoshError {
    CoshError::new(ErrorCode::AuditCorrupt, message, "audit")
}

fn state_io_error(operation: &str, path: &Path, error: std::io::Error) -> CoshError {
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    state_error(format!("{operation} {basename}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn state_round_trip_uses_private_file() {
        use std::os::unix::fs::MetadataExt;

        let directory = crate::audit::AuditTestDir::create();
        let state = AuditOperationalState::new(AuditSettings::default());
        write_state(directory.path(), &state).unwrap();
        assert_eq!(read_state(directory.path()).unwrap(), Some(state));
        let metadata = std::fs::metadata(state_path(directory.path())).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }
}
