//! Versioned `AgentSight` target Binding plan.

use asc_policy_types::identifiers::{ResourceId, Revision};
use serde::{Deserialize, Serialize};

/// Versioned format of the `AgentSight` Binding plan passed through PCP.
pub const AGENTSIGHT_BINDING_PLAN_FORMAT: &str = "agentsight.actplane.binding.v1";

/// Schema version encoded by [`AgentSightBindingPlan`].
pub const AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION: u16 = 1;

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
    /// Source Scope revision retained for provenance; Client use is optional.
    pub scope_revision: Revision,
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
    },
}
