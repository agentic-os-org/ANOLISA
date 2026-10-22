//! Central snapshots and explicit-prefix compatibility discovery.

use super::{
    legacy, paths, validation, DatabasePaths, InstalledDatabase, Inventory, InventorySource, Scope,
    MAX_DATABASE_BYTES,
};
use crate::{require, Error, Result};
use std::{collections::BTreeSet, os::unix::fs::MetadataExt, path::Path};

pub(super) fn inventory(paths: &DatabasePaths, prefix: Option<&Path>) -> Result<Inventory> {
    paths::absolute(&paths.root)?;
    if let Some(prefix) = prefix {
        paths::absolute(prefix)?;
    }
    let mut inventory = Inventory {
        database_path: paths.installed(),
        generation: None,
        source: InventorySource::Absent,
        installations: Vec::new(),
        _locks: Vec::new(),
        _database_generation: None,
        _dependency_targets: BTreeSet::new(),
    };
    match paths::directory(&paths.root) {
        Err(error) if paths::missing(&error) => {}
        Err(error) => return Err(error),
        Ok(root) => {
            let metadata = root.metadata()?;
            // SAFETY: geteuid only reads process identity.
            let owner_uid = if paths.scope == Scope::System {
                0
            } else {
                unsafe { libc::geteuid() }
            };
            require(
                metadata.uid() == owner_uid && metadata.mode() & 0o022 == 0,
                format!(
                    "package database must be owned and not group/world writable: {}",
                    paths.root.display()
                ),
            )?;
            reject_pending(&paths.root)?;
            let lock = paths::lock(&paths.root.join("lock"))
                .map_err(|error| incomplete(&paths.root, error))?;
            let lock_metadata = lock.metadata()?;
            require(
                lock_metadata.uid() == owner_uid && lock_metadata.mode() & 0o022 == 0,
                "unsafe package database lock ownership or permissions",
            )?;
            inventory._locks.push(lock);
            reject_pending(&paths.root)?;
            let database: InstalledDatabase = super::decode(
                &paths::bytes(&paths.installed(), MAX_DATABASE_BYTES)
                    .map_err(|error| incomplete(&paths.root, error))?,
            )?;
            let metadata = paths::regular(&paths.installed())?.metadata()?;
            require(
                metadata.uid() == owner_uid && metadata.mode() & 0o022 == 0,
                "unsafe package database record ownership or permissions",
            )?;
            validation::database(&database, paths.scope)?;
            inventory.generation = Some(database.generation);
            inventory._database_generation = Some(database.generation);
            inventory.source = InventorySource::Database;
            for installation in &database.installations {
                for package in &installation.packages {
                    inventory._dependency_targets.insert((
                        installation.installation_id.clone(),
                        package.package_id.clone(),
                        package.version.clone(),
                    ));
                }
            }
            inventory.installations = database
                .installations
                .into_iter()
                .filter(|installation| prefix.is_none_or(|prefix| installation.prefix == prefix))
                .collect();
        }
    }
    if let Some(prefix) = prefix.filter(|_| inventory.installations.is_empty()) {
        require(
            super::RegistrationPointer::read(prefix)?.is_none(),
            "prefix registration has no matching installation in the selected database",
        )?;
        if let Some((installation, lock)) = legacy::installation(prefix)? {
            for package in &installation.packages {
                inventory._dependency_targets.insert((
                    installation.installation_id.clone(),
                    package.package_id.clone(),
                    package.version.clone(),
                ));
            }
            inventory.installations.push(installation);
            inventory._locks.push(lock);
            inventory.source = InventorySource::Legacy;
            inventory.generation = None;
        } else {
            return Err(Error::Invalid(format!(
                "no installed package records for prefix: {}",
                prefix.display()
            )));
        }
    }
    if inventory.source == InventorySource::Database {
        for installation in &inventory.installations {
            // Listing records must survive a missing or inaccessible payload root.
            // Doctor reports those directory failures separately.
            super::check_registered(paths, &installation.prefix, &installation.installation_id)?;
        }
    }
    Ok(inventory)
}

fn incomplete(root: &Path, error: Error) -> Error {
    if paths::missing(&error) {
        Error::Invalid(format!(
            "incomplete package database at {}; missing installed.json or lock",
            root.display()
        ))
    } else {
        error
    }
}

pub(super) fn reject_pending(root: &Path) -> Result<()> {
    let transactions = root.join("transactions");
    match paths::entries(&transactions) {
        Ok(entries) if !entries.is_empty() => Err(Error::NeedsRecovery(transactions)),
        Ok(_) => Ok(()),
        Err(error) if paths::missing(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn recheck(inventory: &Inventory) -> Result<()> {
    if let Some(generation) = inventory._database_generation {
        let root = inventory
            .database_path
            .parent()
            .ok_or_else(|| Error::Invalid("database path has no parent".into()))?;
        reject_pending(root)?;
        let database: InstalledDatabase =
            super::decode(&paths::bytes(&inventory.database_path, MAX_DATABASE_BYTES)?)?;
        require(
            database.schema_version == 1 && database.generation == generation,
            "package database generation changed; retry the query",
        )?;
        if let Some(lock) = inventory._locks.first() {
            paths::same(&root.join("lock"), &lock.metadata()?)?;
        }
    }
    for installation in &inventory.installations {
        if installation.registration == super::Registration::LegacyUnregistered {
            if let Some(lock) = inventory._locks.last() {
                paths::same(
                    &installation.prefix.join(".aw-install.lock"),
                    &lock.metadata()?,
                )?;
            }
        }
    }
    Ok(())
}
