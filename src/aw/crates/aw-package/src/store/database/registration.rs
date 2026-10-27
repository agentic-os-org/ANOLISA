//! Explicit prefix locators close the historical writer and identify the database authority.

use super::{paths, DatabasePaths, Scope, SYSTEM_DATABASE_ROOT};
use crate::{description, require, Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Prefix-local locator; package facts remain solely in the central database.
/// Historical component decoders reject this shape before changing any files.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistrationPointer {
    /// Locator format, currently one.
    pub schema_version: u32,
    /// Persistent installation selected in the database.
    pub installation_id: String,
    /// Explicit user or system authority; never inferred from sudo or a path.
    pub scope: Scope,
    /// Absolute database root, preserving the XDG location used for installation.
    pub database_root: PathBuf,
}

impl RegistrationPointer {
    /// Construct a locator for a validated database and persistent installation ID.
    ///
    /// # Errors
    /// Rejects an invalid ID, scope or database path.
    pub fn new(paths: &DatabasePaths, installation_id: String) -> Result<Self> {
        let value = Self {
            schema_version: 1,
            installation_id,
            scope: paths.scope(),
            database_root: paths.root.clone(),
        };
        value.database_paths()?;
        Ok(value)
    }

    /// Read a sole locator, distinguishing historical component records by their format.
    ///
    /// # Errors
    /// Rejects mixed locator/legacy records, unsupported fields and unsafe traversal.
    pub fn read(prefix: &Path) -> Result<Option<Self>> {
        paths::absolute(prefix)?;
        let directory = prefix.join(".aw-packages");
        let entries = match paths::entries(&directory) {
            Ok(entries) => entries,
            Err(error) if paths::missing(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let marker = directory.join("aw-core.json");
        if !entries.iter().any(|path| path == &marker) {
            return Ok(None);
        }
        let bytes = paths::bytes(&marker, description::MAX_DESCRIPTION_BYTES)?;
        let value: serde_json::Value = super::decode(&bytes)?;
        if value.get("format").and_then(serde_json::Value::as_u64) == Some(1)
            && value.get("schema_version").is_none()
        {
            return Ok(None);
        }
        let pointer: Self = super::decode(&bytes)?;
        pointer.database_paths()?;
        require(
            entries.len() == 1,
            "registration pointer conflicts with older package records",
        )?;
        require(
            !pointer.database_root.starts_with(prefix),
            "package database must be outside its payload prefix",
        )?;
        Ok(Some(pointer))
    }

    /// Select the recorded authority without discovering files or changing identity.
    ///
    /// # Errors
    /// Rejects unknown scopes, unsupported versions or a nonstandard system root.
    pub fn database_paths(&self) -> Result<DatabasePaths> {
        require(
            self.schema_version == 1,
            "unsupported package registration schema_version",
        )?;
        description::identifier(&self.installation_id)?;
        paths::absolute(&self.database_root)?;
        require(
            matches!(self.scope, Scope::User | Scope::System),
            "registration requires an explicit user or system scope",
        )?;
        require(
            self.scope != Scope::System || self.database_root == Path::new(SYSTEM_DATABASE_ROOT),
            "system registration uses an unexpected database root",
        )?;
        Ok(DatabasePaths {
            root: self.database_root.clone(),
            scope: self.scope,
        })
    }
}

pub(crate) fn check_registered(
    paths: &DatabasePaths,
    prefix: &Path,
    installation_id: &str,
) -> Result<()> {
    match RegistrationPointer::read(prefix) {
        Ok(Some(pointer)) => require(
            pointer.installation_id == installation_id
                && pointer.database_root == paths.root
                && pointer.scope == paths.scope(),
            "prefix registration does not match the central package database",
        ),
        Ok(None) => match paths::entries(&prefix.join(".aw-packages")) {
            Ok(entries) => require(
                entries.is_empty(),
                "older package records conflict with the central package database",
            ),
            Err(error) if paths::missing(&error) => Ok(()),
            Err(Error::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        },
        Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn require_registered(
    paths: &DatabasePaths,
    prefix: &Path,
    installation_id: &str,
) -> Result<()> {
    let pointer = RegistrationPointer::read(prefix)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "registered raw installation is missing its registration pointer",
        )
    })?;
    require(
        pointer.installation_id == installation_id
            && pointer.database_root == paths.root
            && pointer.scope == paths.scope(),
        "prefix registration does not match the central package database",
    )
}
