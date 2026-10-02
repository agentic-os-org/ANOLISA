//! Shared Binding aggregate data and atomic storage operations.
//! Callers own lifecycle, retry and observation rules; storage owns atomicity.
#![forbid(unsafe_code)]
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::binding::{BindingStatus, BindingView};
use asc_policy_types::target::{Presence, TargetRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Deployment {
    pub target: TargetRef,
    pub revision: Revision,
    pub presence: Presence,
    pub last_confirmed: Option<Presence>,
}

/// Caller-supplied time is monotonic milliseconds within this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingStateSnapshot {
    pub binding: BindingView,
    pub deployments: Vec<Deployment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A bounded CAS retry loop exhausted its budget; this does not imply a storage outage.
    #[error("reconciliation CAS contention exhausted")]
    Contended,
    #[error("reconciliation storage unavailable")]
    Unavailable,
    #[error("invalid reconciliation transaction")]
    Invalid,
}

/// Field-scoped reconciliation update. There is deliberately no spec field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconciliationPatch {
    pub status: Option<asc_policy_types::binding::BindingLifecycle>,
    pub deployments: Option<Vec<Deployment>>,
}

/// Conditional reconciliation transaction. None removes the whole aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingStateWrite {
    pub write_id: Uuid,
    pub next: Option<ReconciliationPatch>,
}
impl BindingStateWrite {
    pub fn delete() -> Self {
        Self {
            write_id: Uuid::new_v4(),
            next: None,
        }
    }
    /// Selects reconciliation fields only; the supplied spec is never written.
    pub fn new(next: BindingStateSnapshot) -> Self {
        Self::patch(ReconciliationPatch {
            status: Some(next.binding.status),
            deployments: Some(next.deployments),
        })
    }
    pub fn patch(next: ReconciliationPatch) -> Self {
        Self {
            write_id: Uuid::new_v4(),
            next: Some(next),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    Applied,
    AlreadyApplied,
    Conflict,
}

/// Reads are consistent; writes are atomic with PAP and never replace spec.
/// Compare revision/phase and only the status explanation/deployments being written.
/// Removal compares all reconciliation fields. Exact latest-write replay may be
/// acknowledged within the current call, even after an intervening PAP write.
/// Errors never establish target absence; no transaction spans remote I/O.
pub trait BindingStateRepository: Send + Sync {
    /// # Errors
    /// Returns storage failure distinctly from absence.
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError>;
    /// # Errors
    /// Returns storage failure or invalid identity/write data without partial changes.
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError>;
}

/// Lightweight Binding metadata for a bounded stable-ID page, including terminal records.
#[derive(Debug, Clone)]
pub struct ReconcileCandidate {
    pub id: ResourceId,
    pub status: BindingStatus,
}
pub trait BindingReconcileCatalog: Send + Sync {
    /// # Errors
    /// Returns a storage error; callers retain the cursor and retry with backoff.
    /// Pages include terminal records so traversal work is bounded even when
    /// most Bindings are inactive. The scheduler filters executable states.
    fn scan_reconciliation(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<ReconcileCandidate>, StoreError>;
}
