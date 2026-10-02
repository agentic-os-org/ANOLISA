use crate::{BindingStateSnapshot, BindingStateWrite, Failure, Observation};
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::binding::{BindingStatus, BindingView};
use asc_policy_types::target::PreparedApply;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedBinding {
    pub id: ResourceId,
    pub revision: Revision,
    pub status: BindingStatus,
}

impl ExpectedBinding {
    pub fn from_binding(binding: &BindingView) -> Self {
        Self {
            id: binding.spec.binding_id.clone(),
            revision: binding.spec.binding_revision,
            status: binding.status.phase,
        }
    }

    pub fn matches(&self, binding: &BindingView) -> bool {
        self.id == binding.spec.binding_id
            && self.revision == binding.spec.binding_revision
            && self.status == binding.status.phase
    }
}

/// Temporary completion data owned by one reconcile call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AttemptOutcome {
    pub expected: ExpectedBinding,
    pub observations: Vec<Observation>,
    pub next_status: BindingStatus,
    pub next_attempt_at: Option<u64>,
    pub error: Option<Failure>,
}

/// Scheduling outcome of one reconcile call, not the Binding's lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Disposition {
    /// No due attempt ran: the Binding is missing, is not pending after recovery,
    /// or its retry deadline is still in the future. The caller must preserve any
    /// future deadline; skipping does not establish successful delivery.
    Skipped,
    /// This call could not claim or complete the expected lifecycle, for example
    /// after a CAS conflict or a newer intent. Reload repository state on the next
    /// call; remote effects and recorded observations may already exist.
    Superseded,
    /// Successful completion was committed: Apply/Update reached Ready, or Delete
    /// confirmed all targets absent and removed the Binding aggregate.
    Completed,
    /// A retryable Pending/error was committed. The deadline is only in memory. The caller
    /// schedules a fresh attempt; no intermediate results survive this call.
    RetryAt {
        /// Deadline in milliseconds in the injected Clock's domain, not Unix time.
        at: u64,
    },
    /// A terminal failure was committed because the error was rejected or the
    /// attempt budget was exhausted. Deployment cleanup responsibility is retained.
    Failed { error: Failure },
}

/// Retained before the storage call, including across post-commit unwind.
#[derive(Debug, Clone)]
pub(crate) struct PendingWrite {
    pub expected: BindingStateSnapshot,
    pub write: BindingStateWrite,
    pub matched: bool,
}
/// Call-local outcome and write receipt retained only for panic bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct ExecutionSlot {
    pub(crate) pending: Option<AttemptOutcome>,
    pub(crate) completion: Option<PendingWrite>,
}

/// Preparation used only by the current attempt; never serialized or cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAttempt {
    pub revision: Revision,
    pub is_update: bool,
    pub prepared: PreparedApply,
}

/// Process-local attempt progress, owned by the scheduling caller, never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptSchedule {
    pub attempts_started: u32,
    pub next_attempt_at: Option<u64>,
    intent: Option<(Revision, bool)>,
}
impl AttemptSchedule {
    pub(crate) fn observe(&mut self, binding: &BindingView) {
        let deleting = matches!(
            binding.status.phase,
            BindingStatus::PendingDelete | BindingStatus::Deleting | BindingStatus::DeleteFailed
        );
        let intent = (binding.spec.binding_revision, deleting);
        if self.intent != Some(intent)
            || (matches!(
                binding.status.phase,
                BindingStatus::PendingApply | BindingStatus::PendingDelete
            ) && binding.status.error.is_none())
        {
            self.attempts_started = 0;
            self.next_attempt_at = None;
        }
        self.intent = Some(intent);
    }
}
