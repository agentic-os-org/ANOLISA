use asc_foundation_types::Revision;
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::PreparedScope;
use serde::{Deserialize, Serialize};

/// Durable allocation state for one Policy identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRevisionState {
    /// Highest revision ever allocated, including deleted revisions.
    pub last_allocated_revision: Revision,
    /// Current Policy content, or `None` when the identity is tombstoned.
    pub current: Option<PreparedPolicy>,
}

/// Durable allocation state for one Scope identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRevisionState {
    /// Highest revision ever allocated, including deleted revisions.
    pub last_allocated_revision: Revision,
    /// Current Scope content, or `None` when the identity is tombstoned.
    pub current: Option<PreparedScope>,
}

/// Bounded query result with the total before pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Page<T> {
    /// Selected records.
    pub items: Vec<T>,
    /// Total matching records before pagination.
    pub total: u64,
}
