//! Types for package management operations.

use serde::{Deserialize, Serialize};

/// Result of a package install operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallResult {
    pub package: String,
    pub version: String,
    pub already_installed: bool,
    pub dependencies_installed: Vec<String>,
}

/// Result of a package remove operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgRemoveResult {
    pub package: String,
    pub version_removed: String,
    pub dependencies_removed: Vec<String>,
}

/// A single entry in package search results.
///
/// # Backward Compatibility (v0.12.0+)
///
/// `version` is `Option<String>` and omitted from JSON when `None`.
/// Previous versions always serialized this field as an empty string.
/// Consumers should use `entry.version.as_deref()` or equivalent
/// optional-aware accessors to handle the missing field gracefully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgSearchEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub summary: String,
    pub installed: bool,
}

/// Result of a package search operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgSearchResult {
    pub packages: Vec<PkgSearchEntry>,
    pub total: usize,
}

/// A single entry in the installed package list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgListEntry {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Result of a package list operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgListResult {
    pub packages: Vec<PkgListEntry>,
    pub total: usize,
}

/// Dry-run preview of what a package install would do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgDryRunResult {
    pub would_install: Vec<String>,
    pub would_upgrade: Vec<String>,
    pub download_size_bytes: Option<u64>,
}

/// Unified package operation enum for the backend trait.
#[derive(Debug, Clone)]
pub enum PkgAction {
    Install { package: String, dry_run: bool },
    Remove { package: String, dry_run: bool },
    Search { query: String },
    List { installed_only: bool },
}
