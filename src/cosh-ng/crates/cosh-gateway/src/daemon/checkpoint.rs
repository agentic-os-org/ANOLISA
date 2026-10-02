//! Provider-neutral workspace checkpoint boundaries for launch and approval.

use cosh_gateway_contracts::{
    capability::RuntimeExecutionFence,
    common::{BoundedOpaque, BoundedText, Digest, WorkspaceRef},
    error::ContractError,
    ids::{ApprovalId, CheckpointId, RunId, TaskId},
    task::CheckpointPolicy,
};
use serde::{Deserialize, Serialize};

/// Origin of one checkpoint durably owned by a managed Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSnapshotKind {
    /// Baseline created before the first Runtime starts.
    Baseline,
    /// Barrier created immediately before an approved Runtime effect.
    PreEffect,
    /// Checkpoint created through the historical brokered checkpoint tool.
    Brokered,
    /// Recovery point created immediately before a user-confirmed switch.
    SwitchRecovery,
}

/// One proven-created checkpoint owned by a managed Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshotView {
    /// Complete provider snapshot identity; prefixes are never accepted.
    pub snapshot_id: CheckpointId,
    /// Durable Task checkpoint stage that created the snapshot.
    pub kind: TaskSnapshotKind,
    /// Run active when the snapshot was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Approval guarded by a pre-effect snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
}

/// Provider-neutral bounded file change returned by preview and diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshotChange {
    /// Workspace-relative path reported by the checkpoint provider.
    pub path: BoundedText,
    /// Stable provider-neutral change kind.
    pub change: BoundedOpaque,
    /// Optional bounded provider detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<BoundedText>,
}

/// Exact provider request after Task ownership and lifecycle admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshotProviderRequest {
    /// Owning managed Task.
    pub task_id: TaskId,
    /// Exact Task-owned snapshot identity.
    pub snapshot_id: CheckpointId,
    /// Canonical admitted workspace.
    pub workspace: WorkspaceRef,
}

/// Read-only provider preview bound to the current workspace generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshotProviderPreview {
    /// Bounded changes from the target snapshot to the live workspace.
    pub changes: Vec<TaskSnapshotChange>,
    /// Digest over the exact provider binding and ordered changes.
    pub preview_digest: Digest,
}

/// Terminal provider outcome for one recovery-protected switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshotProviderSwitch {
    /// Provider head replaced by the switch.
    pub from: BoundedOpaque,
    /// Exact target snapshot selected by the caller.
    pub to: CheckpointId,
}

/// Classified provider result for one guarded snapshot switch attempt.
#[derive(Debug)]
pub enum TaskSnapshotProviderSwitchResult {
    /// The provider durably proved the exact switch completed.
    Switched(TaskSnapshotProviderSwitch),
    /// The provider proved it rejected the request before any switch effect.
    Rejected {
        /// Stable bounded reason suitable for user presentation.
        reason: BoundedText,
    },
    /// The provider may have applied the switch and must not be called again.
    PossiblyApplied {
        /// Bounded transport or provider failure.
        error: ContractError,
    },
}

/// Provider boundary for Task-owned snapshot inspection and switching.
pub trait TaskSnapshotDriver: Send {
    /// Recomputes a bounded preview against the current live workspace.
    fn preview(
        &mut self,
        request: &TaskSnapshotProviderRequest,
    ) -> Result<TaskSnapshotProviderPreview, ContractError>;

    /// Creates or reconciles the exact recovery checkpoint allocated by Gateway.
    fn create_recovery(
        &mut self,
        request: &TaskSnapshotProviderRequest,
        recovery_id: &CheckpointId,
        preview_digest: &Digest,
    ) -> Result<(), ContractError>;

    /// Performs one guarded, non-replayable switch after recovery evidence exists.
    ///
    /// Returned `Err` values must be validation failures before provider dispatch.
    /// Post-dispatch uncertainty is represented by `PossiblyApplied`.
    fn switch(
        &mut self,
        request: &TaskSnapshotProviderRequest,
        expected_preview_digest: &Digest,
        operation_id: &CheckpointId,
        operation_digest: &Digest,
    ) -> Result<TaskSnapshotProviderSwitchResult, ContractError>;
}

/// Exact workspace baseline evidence returned by a configured provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreRuntimeCheckpointEvidence {
    /// COSH baseline identity supplied before provider invocation.
    pub baseline_id: CheckpointId,
    /// Opaque provider generation used for exact reconciliation.
    pub provider_generation: BoundedOpaque,
    /// Digest of provider evidence retained by the Gateway.
    pub evidence_digest: Digest,
}

/// Exact provider binding persisted before a baseline create can be dispatched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreRuntimeCheckpointBinding {
    /// Opaque provider workspace identity resolved during read-only preparation.
    pub provider_workspace_id: BoundedOpaque,
    /// Opaque provider generation fenced to the prepared workspace identity.
    pub provider_generation: BoundedOpaque,
    /// Exact provider operation digest used by both create and reconciliation.
    pub operation_digest: Digest,
}

/// Immutable request to create one Task baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreRuntimeCheckpointRequest {
    /// COSH baseline identity allocated durably before invocation.
    pub baseline_id: CheckpointId,
    /// Task receiving the baseline.
    pub task_id: TaskId,
    /// Run blocked on the baseline.
    pub run_id: RunId,
    /// Canonical public workspace identity.
    pub workspace: WorkspaceRef,
}

/// Typed create result that distinguishes safe fallback from uncertainty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreRuntimeCheckpointCreateResult {
    /// Provider created the exact baseline and returned durable evidence.
    Created {
        /// Evidence used by recovery and presentation.
        evidence: PreRuntimeCheckpointEvidence,
    },
    /// Provider proved that invocation had no effect.
    KnownNoEffect {
        /// Safe reason suitable for Task presentation.
        reason: BoundedText,
    },
    /// Provider was unavailable before applying any effect.
    Unavailable {
        /// Safe reason suitable for Task presentation.
        reason: BoundedText,
    },
    /// Invocation may have applied but no exact receipt was returned.
    PossiblyApplied {
        /// Bounded provider or transport failure.
        error: ContractError,
    },
}

/// Exact request used after a create may have started.
pub type PreRuntimeCheckpointReconcileRequest = PreRuntimeCheckpointRequest;

/// Evidence-only restart reconciliation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreRuntimeCheckpointReconcileResult {
    /// Exact baseline evidence exists.
    Created {
        /// Reconciled exact evidence.
        evidence: PreRuntimeCheckpointEvidence,
    },
    /// Provider proved the exact baseline identity was never applied.
    NotApplied,
    /// Provider cannot prove either result.
    Unknown {
        /// Safe reason suitable for Task presentation.
        reason: BoundedText,
    },
}

/// Provider-neutral checkpoint adapter owned by the composition root.
pub trait PreRuntimeCheckpointDriver: Send {
    /// Resolves immutable provider identity without dispatching a create effect.
    fn prepare_baseline(
        &mut self,
        request: &PreRuntimeCheckpointRequest,
    ) -> Result<PreRuntimeCheckpointBinding, ContractError>;

    /// Invokes create for a baseline that has never entered `started` before.
    ///
    /// Errors are restricted to validation failures before provider dispatch.
    /// Post-dispatch failures must return
    /// [`PreRuntimeCheckpointCreateResult::PossiblyApplied`].
    fn create_baseline(
        &mut self,
        request: &PreRuntimeCheckpointRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointCreateResult, ContractError>;

    /// Reconciles only exact provider evidence; implementations must not create.
    fn reconcile_baseline(
        &mut self,
        request: &PreRuntimeCheckpointReconcileRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointReconcileResult, ContractError>;

    /// Resolves immutable provider identity for an approval checkpoint.
    fn prepare_approval_checkpoint(
        &mut self,
        _request: &ApprovalCheckpointRequest,
    ) -> Result<ApprovalCheckpointPrepareResult, ContractError> {
        Ok(ApprovalCheckpointPrepareResult::Unavailable {
            reason: static_reason("checkpoint provider is not attached"),
        })
    }

    /// Creates an approval checkpoint whose durable state has never started before.
    fn create_approval_checkpoint(
        &mut self,
        _request: &ApprovalCheckpointRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointCreateResult, ContractError> {
        Ok(ApprovalCheckpointCreateResult::Unavailable {
            reason: static_reason("checkpoint provider is not attached"),
        })
    }

    /// Reconciles exact approval checkpoint evidence without creating.
    fn reconcile_approval_checkpoint(
        &mut self,
        _request: &ApprovalCheckpointRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointReconcileResult, ContractError> {
        Ok(ApprovalCheckpointReconcileResult::Unknown {
            reason: static_reason("checkpoint reconciliation evidence is unavailable"),
        })
    }
}

fn static_reason(value: &'static str) -> BoundedText {
    BoundedText::new(value).unwrap_or_else(|_| unreachable!())
}

/// Immutable approval-bound checkpoint request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCheckpointRequest {
    /// Broker-owned checkpoint identity.
    pub checkpoint_id: CheckpointId,
    /// Runtime Permission approval blocked by the checkpoint.
    pub approval_id: ApprovalId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Canonical admitted workspace.
    pub workspace: WorkspaceRef,
    /// Exact Runtime and Run-lease generation requesting approval.
    pub runtime_fence: RuntimeExecutionFence,
}

/// Read-only preparation outcome before an approval checkpoint can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCheckpointPrepareResult {
    /// Immutable provider binding is ready to persist.
    Prepared(PreRuntimeCheckpointBinding),
    /// Provider proved that no checkpoint effect is needed.
    KnownNoEffect {
        /// Safe reason suitable for durable diagnostics.
        reason: BoundedText,
    },
    /// Provider is unavailable before any checkpoint effect.
    Unavailable {
        /// Safe reason suitable for durable diagnostics.
        reason: BoundedText,
    },
}

/// First-attempt create result for an approval checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCheckpointCreateResult {
    /// Exact evidence proves the checkpoint exists.
    Created {
        /// Evidence bound to the durable checkpoint identity and generation.
        evidence: ApprovalCheckpointEvidence,
    },
    /// Provider proved no effect occurred.
    KnownNoEffect {
        /// Safe reason suitable for durable diagnostics.
        reason: BoundedText,
    },
    /// Provider was unavailable before any effect.
    Unavailable {
        /// Safe reason suitable for durable diagnostics.
        reason: BoundedText,
    },
    /// Dispatch may have applied and requires evidence-only reconciliation.
    PossiblyApplied {
        /// Safe transport or provider failure retained for diagnostics.
        error: ContractError,
    },
}

/// Evidence-only reconciliation result for an approval checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCheckpointReconcileResult {
    /// Exact evidence proves the checkpoint exists.
    Created {
        /// Evidence bound to the durable checkpoint identity and generation.
        evidence: ApprovalCheckpointEvidence,
    },
    /// Exact evidence proves the create was not applied.
    NotApplied,
    /// Neither creation nor non-application can be proven.
    Unknown {
        /// Safe reason suitable for durable diagnostics.
        reason: BoundedText,
    },
}

/// Exact evidence for one approval checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCheckpointEvidence {
    /// Broker-owned checkpoint identity.
    pub checkpoint_id: CheckpointId,
    /// Provider generation fenced by the durable binding.
    pub provider_generation: BoundedOpaque,
    /// Digest of retained provider evidence.
    pub evidence_digest: Digest,
}

/// Durable approval checkpoint lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalCheckpointState {
    /// Intent is durable and no provider binding has started.
    Intent,
    /// Binding is durable and create may have started.
    Started,
    /// Exact evidence proves creation.
    Created,
    /// Auto policy durably skipped a proven no-effect outcome.
    Skipped,
    /// Outcome cannot be proven.
    Unknown,
    /// Required checkpoint failed closed.
    Failed,
}

/// Durable approval checkpoint record used by scheduler recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCheckpointRecord {
    /// Runtime Permission approval guarded by this checkpoint.
    pub approval_id: ApprovalId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Broker-owned checkpoint identity.
    pub checkpoint_id: CheckpointId,
    /// Policy that determines whether explicit no-effect may be skipped.
    pub policy: CheckpointPolicy,
    /// Exact Runtime and lease generation fenced at approval time.
    pub runtime_fence: RuntimeExecutionFence,
    /// Current durable barrier state.
    pub state: ApprovalCheckpointState,
    /// Provider identity persisted before create dispatch.
    pub binding: Option<PreRuntimeCheckpointBinding>,
    /// Exact evidence retained after proven creation.
    pub evidence: Option<ApprovalCheckpointEvidence>,
    /// Safe terminal reason for skipped, unknown, or failed outcomes.
    pub reason: Option<BoundedText>,
}

/// Durable baseline state exposed by Task projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreRuntimeBaselineState {
    /// Checkpoint Outbox is durable but not invoked.
    Pending,
    /// Provider invocation may have started and requires reconciliation.
    Started,
    /// Exact evidence proves the baseline exists.
    Created,
    /// Auto policy durably skipped a known-no-effect or unavailable provider.
    Skipped,
    /// Outcome cannot be proven and the Runtime remains blocked.
    Unknown,
    /// Required baseline failed closed before the Runtime started.
    Failed,
}

/// Safe durable baseline projection attached to a Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreRuntimeBaselineView {
    /// Exact COSH baseline identity.
    pub baseline_id: CheckpointId,
    /// Launch policy that governs fallback.
    pub policy: CheckpointPolicy,
    /// Current durable baseline state.
    pub state: PreRuntimeBaselineState,
    /// Exact evidence when creation succeeded.
    pub evidence: Option<PreRuntimeCheckpointEvidence>,
    /// Safe terminal or skip reason when available.
    pub reason: Option<BoundedText>,
}
