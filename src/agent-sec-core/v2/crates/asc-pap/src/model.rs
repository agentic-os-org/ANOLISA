use asc_foundation_types::Revision;
use asc_policy_types::policy::PreparedPolicy;
use serde::{Deserialize, Serialize};

/// Consistent durable inputs for one discovery worker; scan caches remain local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDiscoverySeed {
    pub scope: asc_policy_types::scope::PreparedScope,
    pub pinned_process: Option<asc_policy_types::process_discovery::ProcessIdentity>,
    pub instances: Vec<asc_policy_types::process_discovery::ProcessIdentity>,
}

impl std::ops::Deref for ScopeDiscoverySeed {
    type Target = asc_policy_types::scope::PreparedScope;
    fn deref(&self) -> &Self::Target {
        &self.scope
    }
}

/// Durable allocation state for one Policy identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRevisionState {
    /// Highest revision ever allocated, including deleted revisions.
    pub last_allocated_revision: Revision,
    /// Current Policy content, or `None` when the identity is tombstoned.
    pub current: Option<PreparedPolicy>,
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
