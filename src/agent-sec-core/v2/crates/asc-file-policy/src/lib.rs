//! Backend-neutral file-protection intent shared by execution backends.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// File-protection payload version supported by the Translator and Client.
pub const FILE_PROTECTION_POLICY_VERSION: u16 = 1;

/// Versioned file protections with independent operation and resource scopes.
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
    /// Denies reads; each backend validates its supported target kinds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read: bool,
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

/// A path condition retained for backends that evaluate patterns on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePathPattern {
    /// Selects filesystem component matching or Cedar string matching.
    pub syntax: PatternSyntax,
    /// Source pattern, with Cedar literal stars and backslashes escaped.
    pub value: String,
    /// Inverts the match, for example in a Cedar unless clause.
    pub negated: bool,
}

/// Wildcard languages keep distinct meanings for separators and question marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternSyntax {
    /// Native *, ?, and whole-segment **.
    Glob,
    /// Cedar like with * spanning separators.
    CedarLike,
}

/// Literal targets and the additional predicates for each patterned target index.
pub type FilePlan = (FileProtectionPolicy, BTreeMap<usize, Vec<FilePathPattern>>);
