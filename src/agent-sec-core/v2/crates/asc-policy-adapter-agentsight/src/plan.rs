//! Versioned `AgentSight` target Binding plan.

use asc_policy_types::identifiers::{ResourceId, Revision};
use serde::{Deserialize, Serialize};

/// Versioned format of the `AgentSight` Binding plan passed through PCP.
pub const AGENTSIGHT_BINDING_PLAN_FORMAT: &str = "agentsight.actplane.binding.v2";

/// Schema version encoded by [`AgentSightBindingPlan`].
pub const AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION: u16 = 2;

/// Generic structured Binding plan format passed through PCP.
pub const AGENTSIGHT_STRUCTURED_BINDING_PLAN_FORMAT: &str = "agentsight.binding.v3";

/// Schema version encoded by [`AgentSightStructuredBindingPlan`].
pub const AGENTSIGHT_STRUCTURED_BINDING_PLAN_SCHEMA_VERSION: u16 = 3;

/// Structured file-protection payload version supported downstream.
pub const AGENTSIGHT_FILE_PROTECTION_POLICY_VERSION: u16 = 1;

/// Media type of the nested `ActPlane` DSL text.
pub const ACTPLANE_POLICY_MEDIA_TYPE: &str = "application/vnd.actplane.dsl.v1";

/// `AgentSight`-specific target plan consumed by the `AgentSight` Client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSightBindingPlan {
    /// Internal plan schema version.
    pub schema_version: u16,
    /// Immutable Binding and source revision identities.
    pub source: AgentSightSourceBinding,
    /// `ActPlane` policy carried by the `AgentSight` API.
    pub policy: AgentSightPolicyPlan,
    /// Target-specific attachment scope.
    pub scope: AgentSightScopePlan,
}

/// Structured `AgentSight` plan kept separate from the legacy DSL schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSightStructuredBindingPlan {
    /// Internal plan schema version.
    pub schema_version: u16,
    /// Immutable Binding and source revision identities.
    pub source: AgentSightSourceBinding,
    /// Target-specific attachment scope.
    pub scope: AgentSightScopePlan,
    /// Structured payload matching the downstream snake-case wire contract.
    pub file_protection_policy: AgentSightFileProtectionPolicy,
}

/// Versioned structured file protections for one Binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSightFileProtectionPolicy {
    /// Downstream structured policy schema version.
    pub version: u16,
    /// Mount-topology behavior for all targets.
    pub topology_mode: AgentSightFileProtectionTopologyMode,
    /// Independently protected paths in deterministic authored order.
    pub targets: Vec<AgentSightFileProtectionTarget>,
}

/// Downstream mount-topology behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSightFileProtectionTopologyMode {
    /// Resolves mount topology once while applying the policy.
    StaticAtApply,
}

/// One structured downstream target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSightFileProtectionTarget {
    /// Absolute normalized literal path.
    pub path: String,
    /// Scope interpretation for `path`.
    pub kind: AgentSightFileProtectionTargetKind,
    /// Independently selected operation protections.
    pub protections: AgentSightFileProtections,
    /// Descendant entries removed from the target scope.
    pub exclusions: Vec<AgentSightFileProtectionExclusion>,
}

/// Downstream target scope interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSightFileProtectionTargetKind {
    /// Protects exactly one file entry.
    ExactFile,
    /// Protects one directory and all descendants except exclusions.
    DirectoryTree,
}

/// Operations independently denied for one downstream target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSightFileProtections {
    /// Protects entries from deletion or removal.
    pub delete: bool,
    /// Protects content from ordinary write operations.
    pub ordinary_write: bool,
}

/// One structured downstream exclusion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSightFileProtectionExclusion {
    /// Absolute normalized descendant path.
    pub path: String,
    /// Scope interpretation for the excluded path.
    pub kind: AgentSightFileProtectionExclusionKind,
}

/// Downstream exclusion scope interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSightFileProtectionExclusionKind {
    /// Excludes exactly one directory entry or file.
    ExactEntry,
    /// Excludes one directory and all descendants.
    DirectorySubtree,
}

/// Source identities retained for target artifact audit and correlation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSightSourceBinding {
    /// Stable Binding identity.
    pub binding_id: ResourceId,
    /// Immutable Binding revision.
    pub binding_revision: Revision,
    /// Stable source Policy identity.
    pub policy_id: ResourceId,
    /// Immutable source Policy revision.
    pub policy_revision: Revision,
    /// Stable source Scope identity.
    pub scope_id: ResourceId,
}

/// `ActPlane` policy payload nested in the `AgentSight` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSightPolicyPlan {
    /// Text media type of `content`.
    pub media_type: String,
    /// Deterministically generated `ActPlane` DSL.
    pub content: String,
}

/// AgentSight-specific attachment scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AgentSightScopePlan {
    /// Process tree rooted at the PID selected by the immutable Scope.
    ProcessTree {
        /// Positive PID accepted by the `AgentSight` apply protocol.
        root_pid: i32,
        /// Exact instance selected by Scope discovery.
        process: asc_policy_types::process_discovery::ProcessIdentity,
    },
}
