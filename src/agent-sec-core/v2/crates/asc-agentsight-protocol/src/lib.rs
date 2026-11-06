//! Shared `AgentSight` file-protection wire types for the Translator and HTTP Client.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// File-protection payload version supported by the Translator and Client.
pub const FILE_PROTECTION_POLICY_VERSION: u16 = 1;

/// Media type used to carry the version-1 payload in a target plan.
pub const FILE_PROTECTION_POLICY_MEDIA_TYPE: &str =
    "application/vnd.agentsight.file-protection.v1+json";

/// Versioned file protections submitted as `file_protection_policy` over HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProtectionPolicy {
    /// Version governing the payload's fields and semantics.
    pub version: u16,
    /// Mount-topology behavior used when installing protection.
    pub topology_mode: FileProtectionTopologyMode,
    /// Independently protected targets with their own exclusions.
    pub targets: Vec<FileProtectionTarget>,
}

/// Supported mount-topology behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionTopologyMode {
    /// Resolves the mount topology when the policy is applied.
    StaticAtApply,
}

/// One protected path and the operations denied within its scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProtectionTarget {
    /// Absolute literal path interpreted by `kind`.
    pub path: String,
    /// Selects an exact file or a directory tree.
    pub kind: FileProtectionTargetKind,
    /// Operations denied on matching entries.
    pub protections: FileProtections,
    /// Entries excluded from this target's protection scope.
    pub exclusions: Vec<FileProtectionExclusion>,
}

/// Scope interpretation for a protected path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionTargetKind {
    /// Protects exactly one file entry.
    ExactFile,
    /// Protects a directory and its descendants.
    DirectoryTree,
}

/// Operations independently denied for one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProtections {
    /// Protects entries from deletion or removal.
    pub delete: bool,
    /// Protects file content from ordinary writes.
    pub ordinary_write: bool,
}

/// One path excluded from its owning target's protection scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProtectionExclusion {
    /// Absolute literal descendant path.
    pub path: String,
    /// Scope interpretation for the excluded path.
    pub kind: FileProtectionExclusionKind,
}

/// Scope interpretation for a target-owned exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionExclusionKind {
    /// Excludes exactly one entry.
    ExactEntry,
    /// Excludes a directory and its descendants.
    DirectorySubtree,
}
