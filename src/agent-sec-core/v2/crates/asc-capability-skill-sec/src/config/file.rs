//! Root-owned system configuration preserves unrelated settings during exact-path registration.

use super::ManagedSkillDir;
use crate::ledger::storage::{Directory, missing};
use crate::{SkillIdentity, SkillSecError, check_deadline, io_error};
use rustix::fs::{Mode, OFlags, mkdirat, open, openat};
use serde_json::{Value, json};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::time::Instant;

const CONFIG_LIMIT: u64 = 1024 * 1024;

/// Reads the daemon's root-owned configuration without following symlink components.
///
/// # Errors
/// Rejects unsafe ownership, permissions, links, oversized files and invalid JSON.
pub fn read_system_config(path: &Path) -> Result<Value, SkillSecError> {
    let parent = config_parent(path, false)?;
    read(&parent, path).map(|(value, _)| value)
}

/// Provisions root-owned private state through descriptor-relative directory operations.
///
/// # Errors
/// Rejects unsafe ownership, permissions or path components.
pub fn prepare_private_state(path: &Path) -> Result<(), SkillSecError> {
    let directory = trusted_directory(path, true)?;
    let metadata = directory.metadata().map_err(|e| io_error(path, e))?;
    if metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err(unsafe_config());
    }
    Ok(())
}

pub(crate) fn remember(
    path: &Path,
    identity: &SkillIdentity,
    exact: bool,
    deadline: Instant,
) -> Result<Vec<ManagedSkillDir>, SkillSecError> {
    check_deadline(deadline)?;
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(unsafe_config());
    }
    let parent = config_parent(path, true)?;
    let (mut value, mode) = match read(&parent, path) {
        Ok(value) => value,
        Err(error) if missing(&error) => (json!({}), 0o600),
        Err(error) => return Err(error),
    };
    let object = value.as_object_mut().ok_or_else(unsafe_config)?;
    let mut patterns: Vec<ManagedSkillDir> = serde_json::from_value(
        object
            .get("managedSkillDirs")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    let entry = ManagedSkillDir::exact(identity)?;
    if !patterns.iter().any(|pattern| {
        if exact {
            pattern == &entry
        } else {
            pattern.contains(identity)
        }
    }) {
        patterns.push(entry);
        object.insert("managedSkillDirs".into(), serde_json::to_value(&patterns)?);
        let bytes = serde_json::to_vec_pretty(&value)?;
        if bytes.len() as u64 > CONFIG_LIMIT {
            return Err(unsafe_config());
        }
        check_deadline(deadline)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(unsafe_config)?;
        parent.write_atomic_mode(name, &bytes, true, mode)?;
    }
    Ok(patterns)
}

fn read(parent: &Directory, path: &Path) -> Result<(Value, rustix::fs::RawMode), SkillSecError> {
    let name = path.file_name().ok_or_else(unsafe_config)?;
    let file = File::from(
        openat(
            &parent.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io_error(path, e))?,
    );
    let metadata = file.metadata().map_err(|e| io_error(path, e))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.len() > CONFIG_LIMIT
    {
        return Err(unsafe_config());
    }
    let mut bytes = Vec::new();
    file.take(CONFIG_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, e))?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err(unsafe_config());
    }
    // Permission bits fit both Linux's u32 and macOS's u16 mode_t.
    let mode = (metadata.mode() & 0o777) as rustix::fs::RawMode;
    Ok((serde_json::from_slice(&bytes)?, mode))
}

fn config_parent(path: &Path, create: bool) -> Result<Directory, SkillSecError> {
    SkillIdentity::new(path)?;
    let parent = path.parent().ok_or_else(unsafe_config)?;
    Ok(Directory {
        file: trusted_directory(parent, create)?,
        path: parent.into(),
    })
}

fn trusted_directory(path: &Path, create: bool) -> Result<File, SkillSecError> {
    SkillIdentity::new(path)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(open("/", flags, Mode::empty()).map_err(|e| io_error(path, e))?);
    for component in path.components() {
        if let Component::Normal(name) = component {
            if create {
                match mkdirat(&directory, name, Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(io_error(path, error)),
                }
            }
            directory = File::from(
                openat(&directory, name, flags, Mode::empty()).map_err(|e| io_error(path, e))?,
            );
            let metadata = directory.metadata().map_err(|e| io_error(path, e))?;
            if metadata.uid() != 0
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                return Err(unsafe_config());
            }
        }
    }
    Ok(directory)
}

fn unsafe_config() -> SkillSecError {
    SkillSecError::Invalid("unsafe SkillSec system configuration or private state path".into())
}
