//! Retain read locks while planning, then leave only immutable evidence for execution.

use super::{Action, FileChange, PackagePlan, PackageSummary};
use crate::{
    database::{
        self, DatabasePaths, Installation, InstalledDatabase, Inventory, InventorySource, Manager,
        Registration, RegistrationPointer, Scope,
    },
    require, Error, Payload, Result,
};
use aw_exec::PackageLock;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, os::unix::fs::MetadataExt, path::Path};

pub(super) struct Snapshot {
    pub database: InstalledDatabase,
    pub before: Option<Vec<u8>>,
    pub selected: Option<Installation>,
    pub legacy: BTreeMap<String, Vec<u8>>,
    pub marker: Option<Vec<u8>>,
    _inventories: Vec<Inventory>,
    _prefix_lock: Option<PackageLock>,
}

impl Snapshot {
    pub fn read(paths: &DatabasePaths, prefix: &Path) -> Result<Self> {
        database::absolute_path(prefix)?;
        require(
            !paths.root.starts_with(prefix) && !prefix.starts_with(&paths.root),
            "package prefix and database must be separate directories",
        )?;
        let central = Inventory::read(paths, None)?;
        let before = central
            .generation
            .map(|_| database::safe_bytes(&paths.installed(), database::MAX_DATABASE_BYTES))
            .transpose()?;
        let database = if let Some(bytes) = &before {
            let database: InstalledDatabase = database::decode(bytes)?;
            database::validate_database(&database, paths.scope())?;
            require(
                Some(database.generation) == central.generation
                    && database.installations == central.installations,
                "package database changed while planning; retry",
            )?;
            database
        } else {
            InstalledDatabase {
                schema_version: 1,
                generation: 0,
                installations: Vec::new(),
            }
        };
        let mut selected = database
            .installations
            .iter()
            .find(|installation| installation.prefix == prefix)
            .cloned();
        let mut inventories = vec![central];
        let mut legacy = BTreeMap::new();
        let pointer = RegistrationPointer::read(prefix)?;
        if let Some(pointer) = &pointer {
            require(
                pointer.database_root == paths.root && pointer.scope == paths.scope(),
                "prefix is registered in a different package database; select its recorded scope",
            )?;
            require(
                selected.as_ref().is_some_and(|installation| {
                    installation.installation_id == pointer.installation_id
                }),
                "prefix registration has no matching installation",
            )?;
        }
        let mut prefix_lock = None;
        match database::safe_directory(prefix) {
            Ok(directory) => {
                let metadata = directory.metadata()?;
                // SAFETY: geteuid only reads process identity.
                let expected = if paths.scope() == Scope::System {
                    0
                } else {
                    unsafe { libc::geteuid() }
                };
                require(
                    metadata.uid() == expected && metadata.mode() & 0o022 == 0,
                    "prefix must be owned by the selected scope and not group/world writable",
                )?;
                let entries = match database::safe_entries(&prefix.join(".aw-packages")) {
                    Ok(entries) => entries,
                    Err(error) if database::is_missing(&error) => Vec::new(),
                    Err(error) => return Err(error),
                };
                if selected.is_none() && !entries.is_empty() {
                    let view = Inventory::read(paths, Some(prefix))?;
                    require(
                        view.source == InventorySource::Legacy,
                        "unexpected package record authority",
                    )?;
                    selected = view.installations.first().cloned();
                    for path in database::safe_entries(&prefix.join(".aw-packages"))? {
                        let name = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or_else(|| Error::Invalid("non-UTF-8 legacy record name".into()))?;
                        legacy.insert(
                            name.to_owned(),
                            database::safe_bytes(&path, crate::description::MAX_DESCRIPTION_BYTES)?,
                        );
                    }
                    inventories.push(view);
                } else if selected.is_some() {
                    prefix_lock = Some(database::shared_file_lock(
                        &prefix.join(".aw-install.lock"),
                    )?);
                }
            }
            Err(error) if database::is_missing(&error) => crate::filesystem::parent(prefix)?,
            Err(error) => return Err(error),
        }
        let marker = match database::safe_bytes(
            &prefix.join(".aw-packages/aw-core.json"),
            crate::description::MAX_DESCRIPTION_BYTES,
        ) {
            Ok(bytes) => Some(bytes),
            Err(error) if database::is_missing(&error) => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            database,
            before,
            selected,
            legacy,
            marker,
            _inventories: inventories,
            _prefix_lock: prefix_lock,
        })
    }

    pub fn plan(&self, paths: &DatabasePaths, prefix: &Path, action: Action) -> PackagePlan {
        PackagePlan {
            format: 1,
            action,
            prefix: prefix.into(),
            scope: paths.scope(),
            expected_generation: self.before.as_ref().map(|_| self.database.generation),
            installation_id: self
                .selected
                .as_ref()
                .filter(|installation| installation.registration == Registration::Registered)
                .map(|installation| installation.installation_id.clone()),
            register_legacy: !self.legacy.is_empty(),
            added: Vec::new(),
            removed: Vec::new(),
            files: Vec::new(),
            dependencies: Vec::new(),
            problems: Vec::new(),
            changes: None,
            database_root: paths.root.clone(),
        }
    }

    pub fn installation(&self, paths: &DatabasePaths, prefix: &Path) -> Installation {
        let mut installation = self.selected.clone().unwrap_or_else(|| Installation {
            installation_id: stable_id(paths, prefix),
            prefix: prefix.into(),
            // SAFETY: geteuid only reads process identity.
            owner_uid: if paths.scope() == Scope::System {
                0
            } else {
                unsafe { libc::geteuid() }
            },
            scope: paths.scope(),
            manager: Manager::Raw,
            registration: Registration::Registered,
            packages: Vec::new(),
        });
        if installation.registration == Registration::LegacyUnregistered {
            let previous = installation.installation_id.clone();
            installation.installation_id = stable_id(paths, prefix);
            installation.scope = paths.scope();
            installation.registration = Registration::Registered;
            for package in &mut installation.packages {
                for dependency in &mut package.dependencies {
                    if dependency.installation_id == previous {
                        dependency.installation_id = installation.installation_id.clone();
                    }
                }
            }
        }
        installation
    }

    pub fn after(
        &self,
        installation: Option<Installation>,
        prefix: &Path,
    ) -> Result<InstalledDatabase> {
        let mut after = self.database.clone();
        after.generation = after
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("package database generation exhausted".into()))?;
        after
            .installations
            .retain(|installation| installation.prefix != prefix);
        if let Some(installation) = installation {
            after.installations.push(installation);
        }
        require(
            serde_json::to_vec(&after)?.len() as u64 <= database::MAX_DATABASE_BYTES,
            "planned package database exceeds its byte limit",
        )?;
        Ok(after)
    }
}

fn stable_id(paths: &DatabasePaths, prefix: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(paths.root.as_os_str().as_encoded_bytes());
    hash.update([0]);
    hash.update(prefix.as_os_str().as_encoded_bytes());
    format!("aw-{:x}", hash.finalize())
}

pub(super) fn summary(package: &database::InstalledPackage) -> PackageSummary {
    PackageSummary {
        package_id: package.package_id.clone(),
        version: package.version.clone(),
    }
}

pub(super) fn files(plan: &mut PackagePlan, action: Action, package: &database::InstalledPackage) {
    for (path, payload) in &package.files {
        plan.files.push(FileChange {
            action,
            path: path.clone(),
            sha256: payload.sha256.clone(),
            mode: payload.mode,
        });
    }
}

pub(super) fn verify(
    plan: &mut PackagePlan,
    installation: &Installation,
) -> (BTreeMap<String, Payload>, u64) {
    let mut verify = BTreeMap::new();
    let mut budget = crate::open_bundle::MAX_PAYLOAD_BYTES;
    if installation.manager != Manager::Raw {
        plan.problem(
            "native_manager",
            "native package files must be changed through their package manager",
            None,
            Some(installation.prefix.clone()),
        );
    }
    for package in &installation.packages {
        for (name, metadata) in &package.files {
            let path = installation.prefix.join(name);
            if let Err(error) = crate::open_bundle::verify(&path, metadata, &mut budget) {
                plan.problem(
                    "file_changed",
                    error.to_string(),
                    Some(&package.package_id),
                    Some(path),
                );
            }
            verify.insert(name.clone(), metadata.clone());
        }
    }
    (verify, crate::open_bundle::MAX_PAYLOAD_BYTES - budget)
}
