//! Shared Binding aggregate data and atomic storage operations.
//! Callers own lifecycle, retry and observation rules; storage owns atomicity.
#![forbid(unsafe_code)]
mod reconciliation;
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::binding::{BindingStatus, BindingView};
use asc_policy_types::target::{Presence, TargetRef};
pub use reconciliation::{apply_binding_write, write_digest};
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
    /// Changes only when the lifecycle or its error changes; never a spec revision.
    #[serde(default = "initial_status_version")]
    pub status_version: i64,
    pub deployments: Vec<Deployment>,
}

const fn initial_status_version() -> i64 {
    1
}

/// Versioned PAP intent captured by the transaction that accepted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingIntentReceipt {
    pub binding: BindingView,
    pub status_version: i64,
}

impl std::ops::Deref for BindingIntentReceipt {
    type Target = BindingView;
    fn deref(&self) -> &Self::Target {
        &self.binding
    }
}

impl From<&BindingStateSnapshot> for BindingIntentReceipt {
    fn from(value: &BindingStateSnapshot) -> Self {
        Self {
            binding: value.binding.clone(),
            status_version: value.status_version,
        }
    }
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
    #[error("policy storage busy")]
    Busy,
    #[error("policy storage full")]
    Full,
    #[error("policy storage read only")]
    ReadOnly,
    #[error("policy storage I/O failure")]
    Io,
    #[error("policy storage corrupt")]
    Corrupt,
    #[error("policy storage schema incompatible")]
    Incompatible,
    #[error("policy storage commit outcome unknown")]
    OutcomeUnknown,
}

/// Field-scoped reconciliation update. There is deliberately no spec field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReconciliationPatch {
    pub status: Option<asc_policy_types::binding::BindingLifecycle>,
    pub deployments: Option<Vec<Deployment>>,
    /// Merge registered target observations even after the original status claim changed.
    pub preserve_observations_on_conflict: bool,
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
            preserve_observations_on_conflict: false,
        })
    }
    pub fn patch(next: ReconciliationPatch) -> Self {
        Self {
            write_id: Uuid::new_v4(),
            next: Some(next),
        }
    }
}
/// Result of the status condition, saved with the write for exact replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteReceipt {
    /// Absent when the aggregate was deleted.
    pub status_version: Option<i64>,
    /// False when observations committed without changing a superseded lifecycle.
    pub status_applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    Applied(WriteReceipt),
    AlreadyApplied(WriteReceipt),
    Conflict,
}

/// Reads are consistent; writes are atomic with PAP and never replace spec.
/// Compare revision/phase and only the status explanation/deployments being written.
/// Removal compares all reconciliation fields. Exact latest-write replay may be
/// acknowledged within the current call, even after an intervening PAP write.
/// Errors never establish target absence; no transaction spans remote I/O.
pub trait BindingStateRepository: Send + Sync {
    /// Checks write availability before starting remote work. Durable stores probe
    /// recovery here; successful reads alone do not prove that writes are possible.
    /// # Errors
    /// Returns storage unavailability or a fatal validation failure.
    fn check_writable(&self) -> Result<(), StoreError> {
        Ok(())
    }

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
