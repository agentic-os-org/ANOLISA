//! Index accepted packages and classify entries in their root views.

use std::collections::{BTreeMap, btree_map::Entry};
use std::path::{Component, Path};

use super::{LoadedPackage, PluginError};

/// Registry of accepted packages, keyed by their configured ID or manifest name.
#[derive(Debug, Default)]
pub struct PackageStore {
    packages: BTreeMap<String, LoadedPackage>,
}

impl PackageStore {
    /// Create an empty package registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an accepted package without replacing an existing entry.
    ///
    /// If no explicit ID is supplied, use the package's manifest name.
    pub fn register(
        &mut self,
        package: LoadedPackage,
        id: Option<&str>,
    ) -> Result<(), PluginError> {
        let id = id.unwrap_or(&package.manifest.name);
        let mut components = Path::new(id).components();
        if id.is_empty()
            || id.len() > 255
            || id.starts_with('.')
            || id.contains(['/', '\\'])
            || id.chars().any(char::is_control)
            || !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(PluginError::InvalidPackageId(id.to_owned()));
        }
        match self.packages.entry(id.to_owned()) {
            Entry::Vacant(slot) => {
                slot.insert(package);
                Ok(())
            }
            Entry::Occupied(slot) => Err(PluginError::DuplicatePackageId {
                id: slot.key().clone(),
                existing: slot.get().root.clone(),
                incoming: package.root,
            }),
        }
    }

    /// Get an accepted package by its registered ID.
    pub fn get(&self, id: &str) -> Option<&LoadedPackage> {
        self.packages.get(id)
    }

    /// Iterate over all accepted packages in ID order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &LoadedPackage)> {
        self.packages
            .iter()
            .map(|(id, package)| (id.as_str(), package))
    }
}

/// Decide whether a package's top-level entry is visible in its package view.
///
/// Standard entries use the kind and discovery state captured by the loader.
/// Dot-prefixed entries and non-standard extension directories are hidden.
pub fn is_package_root_entry_visible(pkg: &LoadedPackage, name: &str, is_dir: bool) -> bool {
    if name.starts_with('.') {
        return false;
    }
    match name {
        "plugin.json" => return !is_dir,
        "mcp.json" => return !is_dir && pkg.mcp_bytes().is_some(),
        "skills" => return is_dir && pkg.has_skills_dir,
        _ if !is_dir => return true,
        _ => {}
    }
    !pkg.manifest
        .extensions
        .as_ref()
        .is_some_and(|fields| fields.contains_key(name))
        && !is_reverse_domain(name)
}

fn is_reverse_domain(name: &str) -> bool {
    name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
