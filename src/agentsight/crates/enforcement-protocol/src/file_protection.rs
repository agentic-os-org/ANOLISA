//! Versioned structured contract for scoped file-protection policies.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Structured file-protection schema version supported by this protocol.
pub const FILE_PROTECTION_POLICY_VERSION: u16 = 1;

/// Structured file protections attached to one policy binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FileProtectionPolicy {
    /// Schema version used to interpret the structured policy.
    pub version: u16,
    /// Mount-topology semantics fixed by the policy author.
    pub topology_mode: FileProtectionTopologyMode,
    /// Independently scoped targets, each with its own exclusions and protections.
    pub targets: Vec<FileProtectionTarget>,
}

/// Mount-topology behavior used while resolving policy targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionTopologyMode {
    /// Resolves the mount topology once when the policy is applied.
    StaticAtApply,
}

/// One protected path and the operations denied within its scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FileProtectionTarget {
    /// Policy-authored path retained without normalization at this protocol layer.
    pub path: String,
    /// Scope interpretation for the target path.
    pub kind: FileProtectionTargetKind,
    /// Operations requested for this target.
    pub protections: FileProtections,
    /// Entries removed from this target's scope before enforcement.
    pub exclusions: Vec<FileProtectionExclusion>,
}

/// Scope interpretation for a file-protection target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionTargetKind {
    /// Protects exactly one file entry.
    ExactFile,
    /// Protects one directory and its descendant tree.
    DirectoryTree,
}

/// Operations independently requested for one target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FileProtections {
    /// Requests protection from entry deletion or removal.
    pub delete: bool,
    /// Requests protection from ordinary content-writing operations.
    pub ordinary_write: bool,
}

/// One path excluded from its owning target's protection scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FileProtectionExclusion {
    /// Policy-authored exclusion path retained without normalization here.
    pub path: String,
    /// Scope interpretation relative to the owning target.
    pub kind: FileProtectionExclusionKind,
}

/// Scope interpretation for a target-owned exclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionExclusionKind {
    /// Excludes exactly one directory entry or file.
    ExactEntry,
    /// Excludes one directory and all of its descendants.
    DirectorySubtree,
}

/// Validation failures for the `ApplyPolicy` payload envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ApplyPolicyValidationError {
    /// Neither the legacy DSL nor a structured policy was supplied.
    #[error("apply policy requires exactly one nonblank policy_dsl or file_protection_policy")]
    MissingPayload,
    /// Both the legacy DSL and a structured policy were supplied.
    #[error("apply policy cannot combine policy_dsl with file_protection_policy")]
    MultiplePayloads,
    /// The structured schema version is not implemented by this protocol.
    #[error("unsupported file protection policy version {actual}; expected {expected}")]
    UnsupportedFileProtectionPolicyVersion {
        /// Structured schema version implemented by this protocol.
        expected: u16,
        /// Structured schema version supplied by the caller.
        actual: u16,
    },
    /// A structured policy must identify at least one target.
    #[error("file protection policy requires at least one target")]
    EmptyTargets,
    /// Every target must request an operation rather than becoming a silent no-op.
    #[error("file protection target {target_index} requests no protections")]
    EmptyProtections {
        /// Zero-based position of the invalid target in the wire payload.
        target_index: usize,
    },
}
