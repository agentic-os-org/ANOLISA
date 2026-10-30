//! Read-only installed-package inventory and file verification, without running package code.

use aw_exec::PackageLock;
mod doctor;
mod json;
mod paths;
mod read;
mod registration;
mod validation;

pub use doctor::{DoctorIssue, DoctorIssueCode, DoctorReport};
pub use paths::{
    DatabasePaths, PackageManager, ANOLISA_SYSTEM_DATABASE_ROOT, SYSTEM_DATABASE_ROOT,
};
pub use registration::RegistrationPointer;

use crate::store::{description::ProviderDeclaration, Payload, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Maximum bytes accepted from one installed-package database.
pub const MAX_DATABASE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum number of installations accepted in one database snapshot.
pub const MAX_INSTALLATIONS: usize = 1024;
/// Maximum number of packages accepted in a single installation.
pub const MAX_PACKAGES: usize = 1024;

/// Storage authority used for this query.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InventorySource {
    /// No package database exists.
    Absent,
    /// Records came from the central installed-package database.
    Database,
}

/// Privilege boundary of an installation; separate user databases are never combined.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Current user's installed packages.
    User,
    /// Administrator-managed packages.
    System,
}

/// Manager responsible for changing this installation's files.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    /// AW manages extracted bundle files.
    Raw,
    /// RPM remains the authority for native package ownership.
    Rpm,
    /// Dpkg remains the authority for native package ownership.
    Deb,
}

/// Whether an installation has been committed to the central database.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    /// Persistent installation ID from the central database.
    #[default]
    Registered,
}

/// Evidence copied from the input; absent facts must not be fabricated during queries.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    /// SHA-256 of the supplied bundle description, whose file digests bind its payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_sha256: Option<String>,
    /// Producer-supplied source commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
}

/// Exact installed dependency selected when the package was committed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstalledDependency {
    /// Database installation containing the dependency.
    pub installation_id: String,
    /// Required package name within that installation.
    pub package_id: String,
    /// Exact installed version; queries never resolve a remote version range.
    pub version: String,
}

/// One independently versioned installed package.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstalledPackage {
    /// Producer-selected package name.
    pub package_id: String,
    /// Producer-selected exact package version.
    pub version: String,
    /// Required operating system.
    pub os: String,
    /// Required CPU architecture.
    pub arch: String,
    /// Relative owned files and the integrity metadata recorded at installation.
    pub files: BTreeMap<String, Payload>,
    /// Exact dependency targets selected at installation time.
    pub dependencies: Vec<InstalledDependency>,
    /// Available provenance evidence.
    pub source: SourceEvidence,
    /// Optional protocol metadata; never executable configuration or secret values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderDeclaration>,
}

/// Installed files sharing one prefix and package-management authority.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    /// Stable database ID.
    pub installation_id: String,
    /// Absolute payload root, independent of the central database location.
    pub prefix: PathBuf,
    /// Recorded owner of the installation prefix.
    pub owner_uid: u32,
    /// User or system privilege boundary.
    pub scope: Scope,
    /// Authority allowed to mutate the files.
    pub manager: Manager,
    /// Registration state; database snapshots must contain only registered records.
    #[serde(default)]
    pub registration: Registration,
    /// Packages with disjoint file ownership; empty registrations retain the prefix locator.
    pub packages: Vec<InstalledPackage>,
}

/// Persisted database envelope. Queries reject unsupported schema versions.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledDatabase {
    /// Version of the stored database format, currently one.
    pub schema_version: u32,
    /// Monotonic commit generation; payload checks do not change it.
    pub generation: u64,
    /// Committed installation records.
    pub installations: Vec<Installation>,
}

/// Immutable inventory snapshot, retaining shared locks until dropped.
#[derive(Debug, Serialize)]
pub struct Inventory {
    /// Selected database's `installed.json`, even when it does not yet exist.
    pub database_path: PathBuf,
    /// Committed generation, absent for uninitialized databases.
    pub generation: Option<u64>,
    /// Record authority used for this query.
    pub source: InventorySource,
    /// Registered installations selected by the query.
    pub installations: Vec<Installation>,
    #[serde(skip)]
    _locks: Vec<PackageLock>,
    #[serde(skip)]
    _database_generation: Option<u64>,
    #[serde(skip)]
    _dependency_targets: BTreeSet<(String, String, String)>,
    #[serde(skip)]
    _manager: PackageManager,
}

#[cfg(test)]
pub(crate) fn read_description(path: &Path, limit: u64) -> Result<Vec<u8>> {
    paths::bytes(path, limit)
}

pub(crate) use json::decode;
pub(crate) use paths::{
    absolute as absolute_path, bytes as safe_bytes, directory as safe_directory,
    lock as shared_file_lock, missing as is_missing, regular as safe_file, same as same_file,
};
pub(crate) use registration::check_registered;
pub(crate) use validation::database as validate_database;

impl Inventory {
    /// Read a database, optionally restricting the view to one absolute prefix.
    ///
    /// This call never creates directories or locks, repairs transactions or runs programs.
    ///
    /// # Errors
    /// Rejects unsafe paths, malformed records, unavailable locks and unfinished transactions.
    pub fn read(paths: &DatabasePaths, prefix: Option<&Path>) -> Result<Self> {
        read::inventory(paths, prefix)
    }

    /// Verify recorded file contents and modes, collecting independent failures.
    ///
    /// # Errors
    /// Returns an error if the database lock or committed generation changed.
    pub fn doctor(&self) -> Result<DoctorReport> {
        doctor::check(self)
    }
}
