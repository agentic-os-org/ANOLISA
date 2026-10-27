//! Validate record shape and file ownership without touching installed payloads.

use super::{paths, InstalledDatabase, Registration, Scope, MAX_INSTALLATIONS, MAX_PACKAGES};
use crate::{
    description::{self, identifier},
    require, Result,
};
use std::collections::BTreeSet;

pub(crate) fn database(database: &InstalledDatabase, scope: Scope) -> Result<()> {
    require(
        database.schema_version == 1,
        "unsupported installed-package schema_version",
    )?;
    require(
        database.installations.len() <= MAX_INSTALLATIONS,
        "too many package installations",
    )?;
    let mut ids = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    let mut owned = BTreeSet::new();
    for installation in &database.installations {
        identifier(&installation.installation_id)?;
        paths::absolute(&installation.prefix)?;
        require(
            ids.insert(&installation.installation_id) && prefixes.insert(&installation.prefix),
            "duplicate installation ID or prefix",
        )?;
        require(
            installation.scope == scope && installation.registration == Registration::Registered,
            "invalid database installation scope or registration",
        )?;
        // SAFETY: geteuid only reads the current process identity.
        let expected_uid = if scope == Scope::System {
            0
        } else {
            unsafe { libc::geteuid() }
        };
        require(
            installation.owner_uid == expected_uid,
            "installation owner does not match database scope",
        )?;
        require(
            installation.packages.len() <= MAX_PACKAGES,
            "invalid installed package count",
        )?;
        let mut packages = BTreeSet::new();
        for package in &installation.packages {
            require(
                packages.insert(&package.package_id),
                "duplicate installed package ID",
            )?;
            description::package_fields(
                &package.package_id,
                "0.0.0",
                &package.os,
                &package.arch,
                &package.files,
                package.provider.as_ref(),
            )?;
            require(
                !package.version.is_empty() && package.version.len() <= 128,
                "invalid stored package version",
            )?;
            if let Some(value) = &package.source.bundle_sha256 {
                description::sha256(value)?;
            }
            if let Some(value) = &package.source.source_commit {
                require(value.len() <= 1024, "stored source evidence exceeds limit")?;
            }
            let mut dependencies = BTreeSet::new();
            require(
                package.dependencies.len() <= MAX_PACKAGES,
                "too many installed package dependencies",
            )?;
            for dependency in &package.dependencies {
                identifier(&dependency.installation_id)?;
                identifier(&dependency.package_id)?;
                require(
                    !dependency.version.is_empty() && dependency.version.len() <= 128,
                    "invalid stored dependency version",
                )?;
                require(
                    dependencies.insert((&dependency.installation_id, &dependency.package_id)),
                    "duplicate installed dependency",
                )?;
                require(
                    dependency.installation_id != installation.installation_id
                        || dependency.package_id != package.package_id,
                    "self installed dependency",
                )?;
            }
            for path in package.files.keys() {
                require(
                    owned.insert(installation.prefix.join(path)),
                    format!("overlapping installed file ownership: {path}"),
                )?;
            }
        }
    }
    for path in &owned {
        for ancestor in path.ancestors().skip(1) {
            require(
                !owned.contains(ancestor),
                format!(
                    "installed payload is another file's parent: {}",
                    path.display()
                ),
            )?;
        }
    }
    Ok(())
}
