//! Inventory of registered AW Provider extensions without historical record import.

use crate::{require, Result};
use serde::Serialize;
use std::{ops::Deref, path::Path};

pub use crate::store::database::*;
#[cfg(test)]
pub(crate) use crate::store::filesystem::safe_file;

/// A read-only snapshot admitted as current AW Provider records.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct Inventory(crate::store::database::Inventory);

impl Deref for Inventory {
    type Target = crate::store::database::Inventory;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Inventory {
    /// Read only AW-owned Provider records; previous-format receipts are unsupported.
    ///
    /// # Errors
    /// Rejects foreign databases, historical records, ordinary software and unsafe metadata.
    pub fn read(paths: &DatabasePaths, prefix: Option<&Path>) -> Result<Self> {
        crate::require_aw_database(paths)?;
        if let Some(prefix) = prefix {
            reject_legacy_prefix(prefix)?;
        }
        let inventory = crate::store::database::Inventory::read(paths, prefix)?;
        require_provider_installations(&inventory.installations)?;
        Ok(Self(inventory))
    }
}

pub(crate) fn require_provider_installations(installations: &[Installation]) -> Result<()> {
    require(
        installations.iter().all(|installation| {
            installation
                .packages
                .iter()
                .all(|package| package.provider.is_some() && package.package_id != "aw-core")
        }),
        "AW manages only registered Provider extensions",
    )
}

pub(crate) fn reject_legacy_prefix(prefix: &Path) -> Result<()> {
    match std::fs::symlink_metadata(prefix.join(".aw-packages")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(crate::Error::Invalid(
            "previous-format records are unsupported by AW Provider management".into(),
        )),
    }
}
