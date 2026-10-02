//! Task ingress commands and durable lifecycle events.

use serde::{de, Deserialize, Deserializer, Serialize};

use crate::{
    capability::{ApprovalDecision, ApprovalRequest},
    common::{
        ActorRef, BoundedName, BoundedText, ContentPart, ContractHeader, ContractSchema, Digest,
        IdempotencyKey, RuntimeBindingRef, RuntimeSelector, TargetRef, WorkspaceRef,
    },
    error::ContractError,
    ids::{ApprovalId, ExecutionId, InputRequestId, PermitId, RunId, TaskId},
    runtime::RuntimeInputRequest,
};

/// Exact schema version of [`TaskLaunchSpecV1`].
pub const TASK_LAUNCH_SPEC_V1: u16 = 1;

/// Runtime family selected for one durable Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRuntime {
    /// COSH Core with Gateway-brokered capability execution.
    Core,
    /// Codex connected through the Agent Client Protocol.
    Codex,
}

/// Pre-Runtime workspace baseline policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointPolicy {
    /// Create a baseline when available and durably record an explicit skip otherwise.
    Auto,
    /// Require a proven baseline before starting the Runtime.
    On,
    /// Start the Runtime without creating a baseline.
    Off,
}

/// Approval policy durably selected for one Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Resolve supported operations automatically while retaining durable audit events.
    AllowAll,
    /// Require an authenticated actor decision.
    ///
    /// This variant remains for legacy clients; new interactive launch surfaces default
    /// to [`Self::AllowAll`].
    Interactive,
}

/// Strict version-one durable launch description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskLaunchSpecV1 {
    /// Must equal [`TASK_LAUNCH_SPEC_V1`].
    pub schema_version: u16,
    /// Bounded user goal delivered to the selected Runtime.
    pub goal: BoundedText,
    /// Provider-neutral Runtime family.
    pub runtime: TaskRuntime,
    /// Canonical workspace identity admitted by the Gateway catalog.
    pub workspace: WorkspaceRef,
    /// Pre-Runtime checkpoint behavior.
    pub checkpoint: CheckpointPolicy,
    /// Durable approval behavior.
    pub approval: ApprovalPolicy,
}

impl TaskLaunchSpecV1 {
    /// Creates a version-one launch description.
    #[must_use]
    pub const fn new(
        goal: BoundedText,
        runtime: TaskRuntime,
        workspace: WorkspaceRef,
        checkpoint: CheckpointPolicy,
        approval: ApprovalPolicy,
    ) -> Self {
        Self {
            schema_version: TASK_LAUNCH_SPEC_V1,
            goal,
            runtime,
            workspace,
            checkpoint,
            approval,
        }
    }
}

impl<'de> Deserialize<'de> for TaskLaunchSpecV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireLaunchSpec {
            schema_version: u16,
            goal: BoundedText,
            runtime: TaskRuntime,
            workspace: WorkspaceRef,
            checkpoint: CheckpointPolicy,
            approval: ApprovalPolicy,
        }

        let wire = WireLaunchSpec::deserialize(deserializer)?;
        if wire.schema_version != TASK_LAUNCH_SPEC_V1 {
            return Err(de::Error::custom(format!(
                "unsupported Task launch schema {}; expected {TASK_LAUNCH_SPEC_V1}",
                wire.schema_version
            )));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            goal: wire.goal,
            runtime: wire.runtime,
            workspace: wire.workspace,
            checkpoint: wire.checkpoint,
            approval: wire.approval,
        })
    }
}

/// Safe version-one launch projection that omits the private Runtime goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskLaunchDescriptorV1 {
    /// Must equal [`TASK_LAUNCH_SPEC_V1`].
    pub schema_version: u16,
    /// SHA-256 digest of the private goal retained for correlation and audit.
    pub goal_digest: Digest,
    /// Provider-neutral Runtime family.
    pub runtime: TaskRuntime,
    /// Canonical workspace identity admitted by the Gateway catalog.
    pub workspace: WorkspaceRef,
    /// Pre-Runtime checkpoint behavior.
    pub checkpoint: CheckpointPolicy,
    /// Durable approval behavior.
    pub approval: ApprovalPolicy,
}

impl TaskLaunchDescriptorV1 {
    /// Projects safe launch metadata from a full durable launch description.
    #[must_use]
    pub fn from_spec(goal_digest: Digest, launch: &TaskLaunchSpecV1) -> Self {
        Self {
            schema_version: TASK_LAUNCH_SPEC_V1,
            goal_digest,
            runtime: launch.runtime,
            workspace: launch.workspace.clone(),
            checkpoint: launch.checkpoint,
            approval: launch.approval,
        }
    }
}

impl<'de> Deserialize<'de> for TaskLaunchDescriptorV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireLaunchDescriptor {
            schema_version: u16,
            goal_digest: Digest,
            runtime: TaskRuntime,
            workspace: WorkspaceRef,
            checkpoint: CheckpointPolicy,
            approval: ApprovalPolicy,
        }

        let wire = WireLaunchDescriptor::deserialize(deserializer)?;
        if wire.schema_version != TASK_LAUNCH_SPEC_V1 {
            return Err(de::Error::custom(format!(
                "unsupported Task launch descriptor schema {}; expected {TASK_LAUNCH_SPEC_V1}",
                wire.schema_version
            )));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            goal_digest: wire.goal_digest,
            runtime: wire.runtime,
            workspace: wire.workspace,
            checkpoint: wire.checkpoint,
            approval: wire.approval,
        })
    }
}

/// Opaque cursor used to resume a Task event attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventCursor(
    /// Last Task revision observed by the client.
    pub u64,
);

/// Reason supplied when cancellation is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// An authenticated actor requested cancellation.
    UserRequested,
    /// Policy revoked access or terminated the operation.
    PolicyRevoked,
    /// A deadline expired.
    Timeout,
    /// Runtime shutdown requires cancellation.
    RuntimeShutdown,
}

/// Terminal or intermediate stage at which cancellation completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationStage {
    /// Cancellation completed before a Runtime started.
    BeforeRuntime,
    /// Cancellation completed during an Agent turn.
    Runtime,
    /// Cancellation completed during governed execution.
    Execution,
}

/// Stable reason why a Run is suspended instead of completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionCode {
    /// The selected Runtime became unavailable.
    RuntimeUnavailable,
    /// The Run awaits approval.
    AwaitingApproval,
    /// Operator intervention is required.
    OperatorRequired,
}

/// Stable reason why an execution result cannot be determined safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyCode {
    /// Transport failed after side-effect admission.
    TransportLost,
    /// Executor restarted before recording a terminal result.
    ExecutorRestarted,
    /// Reconciliation could not prove the outcome.
    ReconciliationFailed,
}

/// Result of one governed execution attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// Side effect completed successfully.
    Succeeded {
        /// Optional bounded reference to execution evidence.
        evidence_ref: Option<crate::common::BoundedOpaque>,
    },
    /// Side effect failed with a safe domain error.
    Failed {
        /// Bounded failure returned by the executor.
        error: ContractError,
    },
}

/// Runtime progress recorded as a durable Task event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "update", rename_all = "snake_case")]
pub enum RuntimeUpdate {
    /// Bounded progress text safe for presentation.
    Progress {
        /// Redacted progress summary.
        summary: BoundedText,
    },
    /// Runtime observed a tool call without authorizing execution.
    ToolObserved {
        /// Bounded tool name.
        name: BoundedName,
        /// Digest of normalized tool input.
        input_digest: Digest,
    },
}

/// Command admitted through the Gateway domain boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum TaskCommand {
    /// Create a new durable Task.
    CreateTask {
        /// User intent after ingress validation.
        intent: BoundedText,
        /// Environment governed by the Task.
        target: TargetRef,
    },
    /// Start a new Runtime attempt for an existing Task.
    StartRun {
        /// Task to execute.
        task_id: TaskId,
        /// Runtime requested for the attempt.
        runtime: RuntimeSelector,
    },
    /// Supply additional input to an existing Task.
    SubmitInput {
        /// Task receiving the input.
        task_id: TaskId,
        /// Neutral bounded content parts.
        content: Vec<ContentPart>,
    },
    /// Resolve a durable approval request.
    ResolveApproval {
        /// Approval being resolved.
        approval_id: ApprovalId,
        /// Authenticated actor decision.
        decision: ApprovalDecision,
    },
    /// Request cancellation of one active Run.
    CancelRun {
        /// Task owning the Run.
        task_id: TaskId,
        /// Run to cancel.
        run_id: RunId,
        /// Stable cancellation cause.
        reason: CancelReason,
    },
    /// Attach to Task events after an optional cursor.
    Attach {
        /// Task to observe.
        task_id: TaskId,
        /// Last revision already observed.
        cursor: Option<EventCursor>,
    },
    /// Queue a new attempt from one exact nonterminal suspended Run.
    RetryRun {
        /// Task owning the suspended Run.
        task_id: TaskId,
        /// Exact previous attempt that may be retried.
        previous_run_id: RunId,
    },
}

/// Authenticated and idempotent Gateway command envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayCommandEnvelope {
    /// Versioned envelope metadata.
    pub header: ContractHeader,
    /// Authenticated actor resolved by the ingress boundary.
    pub actor: ActorRef,
    /// Caller-scoped replay key.
    pub idempotency_key: IdempotencyKey,
    /// Optional optimistic concurrency precondition.
    pub expected_task_revision: Option<u64>,
    /// Neutral Task command.
    pub command: TaskCommand,
}

impl GatewayCommandEnvelope {
    /// Rejects a header that does not declare the Gateway command schema.
    pub fn validate_schema(&self) -> Result<(), crate::common::EnvelopeSchemaError> {
        self.header.validate_schema(ContractSchema::GatewayCommand)
    }
}

impl<'de> Deserialize<'de> for GatewayCommandEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEnvelope {
            header: ContractHeader,
            actor: ActorRef,
            idempotency_key: IdempotencyKey,
            expected_task_revision: Option<u64>,
            command: TaskCommand,
        }

        let wire = WireEnvelope::deserialize(deserializer)?;
        let envelope = Self {
            header: wire.header,
            actor: wire.actor,
            idempotency_key: wire.idempotency_key,
            expected_task_revision: wire.expected_task_revision,
            command: wire.command,
        };
        envelope.validate_schema().map_err(de::Error::custom)?;
        Ok(envelope)
    }
}

/// Durable Task lifecycle state used by projections and API responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Task exists but no Run is active.
    Submitted,
    /// Task is queued for a Runtime.
    Queued,
    /// A Run is active.
    Running,
    /// A Run is paused while an actor resolves an approval.
    WaitingApproval,
    /// A Run is paused while an actor supplies additional input.
    WaitingInput,
    /// A Run is suspended pending an external condition.
    Suspended,
    /// Task completed successfully.
    Succeeded,
    /// Task completed with failure.
    Failed,
    /// Task was cancelled.
    Cancelled,
}

/// Stable discriminator for a [`TaskEvent`] without inspecting its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    /// A Task was accepted.
    TaskSubmitted,
    /// A Run was queued.
    TaskQueued,
    /// A Run started.
    RunStarted,
    /// A Runtime session became bound to a Run.
    RuntimeBound,
    /// Runtime progress was recorded.
    RuntimeEventRecorded,
    /// Runtime requested durable user input.
    InputRequested,
    /// Authorized user input was accepted for dispatch.
    InputSubmitted,
    /// Capability policy requested approval.
    ApprovalRequested,
    /// Approval was resolved.
    ApprovalResolved,
    /// Approval became impossible to deliver without an actor decision.
    ApprovalAbandoned,
    /// A governed execution was planned.
    ExecutionPlanned,
    /// A governed execution completed.
    ExecutionResultRecorded,
    /// Execution outcome became uncertain.
    ExecutionUncertain,
    /// Cancellation intent was recorded.
    CancellationRequested,
    /// A Run completed cancellation.
    RunCancelled,
    /// A Run was suspended.
    RunSuspended,
    /// A Run succeeded.
    RunSucceeded,
    /// A Run failed.
    RunFailed,
    /// A retry Run was queued.
    RunRetryQueued,
    /// The Task succeeded.
    TaskSucceeded,
    /// The Task failed.
    TaskFailed,
    /// The Task was cancelled.
    TaskCancelled,
}

/// Non-decision reason that closes a pending approval interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAbandonCause {
    /// The provider cancelled the callback before accepting a response.
    ProviderCancelled,
}

/// Immutable fact in a Task lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TaskEvent {
    /// A validated intent was accepted as a Task.
    TaskSubmitted {
        /// Digest of the admitted intent payload.
        intent_digest: Digest,
        /// Environment governed by the Task.
        target: TargetRef,
    },
    /// A new Run was queued for a Runtime.
    TaskQueued {
        /// New execution attempt.
        run_id: RunId,
        /// Selected Runtime.
        runtime: RuntimeSelector,
    },
    /// A queued Run started.
    RunStarted {
        /// Active Run.
        run_id: RunId,
    },
    /// A fenced Runtime session became active for a Run.
    RuntimeBound {
        /// Active Run.
        run_id: RunId,
        /// Fenced Runtime binding.
        binding: RuntimeBindingRef,
    },
    /// Neutral Runtime progress was accepted by the coordinator.
    RuntimeEventRecorded {
        /// Run that produced the update.
        run_id: RunId,
        /// Recorded progress update.
        update: RuntimeUpdate,
    },
    /// A Runtime request became the one pending input for the active Run.
    InputRequested {
        /// Complete bounded request presentation and identity.
        request: RuntimeInputRequest,
    },
    /// A response was accepted without storing its raw value in Task history.
    InputSubmitted {
        /// Exact pending request resolved by this response.
        request_id: InputRequestId,
        /// Active Run owning the pending request.
        run_id: RunId,
        /// Digest of the typed response stored in the private dispatch ledger.
        response_digest: Digest,
    },
    /// Capability policy requires an actor decision.
    ApprovalRequested {
        /// Durable approval request.
        approval: ApprovalRequest,
    },
    /// An authorized actor resolved an approval.
    ApprovalResolved {
        /// Resolved approval.
        approval_id: ApprovalId,
        /// Actor decision.
        decision: ApprovalDecision,
    },
    /// A pending approval became undeliverable without an actor decision.
    ApprovalAbandoned {
        /// Pending approval whose provider callback disappeared.
        approval_id: ApprovalId,
        /// Typed reason no decision could be delivered.
        cause: ApprovalAbandonCause,
    },
    /// A permit was bound to an execution identity.
    ExecutionPlanned {
        /// Governed execution attempt.
        execution_id: ExecutionId,
        /// Permit authorizing the attempt.
        permit_id: PermitId,
    },
    /// An execution reached a known terminal result.
    ExecutionResultRecorded {
        /// Governed execution attempt.
        execution_id: ExecutionId,
        /// Known terminal result.
        outcome: ExecutionOutcome,
    },
    /// An execution may have produced a side effect but has no proven result.
    ExecutionUncertain {
        /// Governed execution attempt.
        execution_id: ExecutionId,
        /// Stable uncertainty cause.
        reason: UncertaintyCode,
    },
    /// Cancellation intent was persisted for a Run.
    CancellationRequested {
        /// Run being cancelled.
        run_id: RunId,
        /// Stable cancellation cause.
        cause: CancelReason,
    },
    /// A Run completed cancellation.
    RunCancelled {
        /// Cancelled Run.
        run_id: RunId,
        /// Lifecycle stage that observed cancellation.
        stage: CancellationStage,
    },
    /// A Run paused without becoming terminal.
    RunSuspended {
        /// Suspended Run.
        run_id: RunId,
        /// Stable suspension cause.
        reason: SuspensionCode,
    },
    /// A Run completed successfully.
    RunSucceeded {
        /// Successful Run.
        run_id: RunId,
    },
    /// A Run completed with failure.
    RunFailed {
        /// Failed Run.
        run_id: RunId,
        /// Bounded terminal error.
        error: ContractError,
    },
    /// A failed or suspended attempt produced a new Run.
    RunRetryQueued {
        /// Previous attempt.
        previous_run_id: RunId,
        /// New retry attempt.
        next_run_id: RunId,
    },
    /// The Task completed successfully.
    TaskSucceeded,
    /// The Task completed with failure.
    TaskFailed {
        /// Bounded terminal error.
        error: ContractError,
    },
    /// The Task completed cancellation.
    TaskCancelled,
}

impl TaskEvent {
    /// Returns the stable payload-independent event discriminator.
    #[must_use]
    pub const fn kind(&self) -> TaskEventKind {
        match self {
            Self::TaskSubmitted { .. } => TaskEventKind::TaskSubmitted,
            Self::TaskQueued { .. } => TaskEventKind::TaskQueued,
            Self::RunStarted { .. } => TaskEventKind::RunStarted,
            Self::RuntimeBound { .. } => TaskEventKind::RuntimeBound,
            Self::RuntimeEventRecorded { .. } => TaskEventKind::RuntimeEventRecorded,
            Self::InputRequested { .. } => TaskEventKind::InputRequested,
            Self::InputSubmitted { .. } => TaskEventKind::InputSubmitted,
            Self::ApprovalRequested { .. } => TaskEventKind::ApprovalRequested,
            Self::ApprovalResolved { .. } => TaskEventKind::ApprovalResolved,
            Self::ApprovalAbandoned { .. } => TaskEventKind::ApprovalAbandoned,
            Self::ExecutionPlanned { .. } => TaskEventKind::ExecutionPlanned,
            Self::ExecutionResultRecorded { .. } => TaskEventKind::ExecutionResultRecorded,
            Self::ExecutionUncertain { .. } => TaskEventKind::ExecutionUncertain,
            Self::CancellationRequested { .. } => TaskEventKind::CancellationRequested,
            Self::RunCancelled { .. } => TaskEventKind::RunCancelled,
            Self::RunSuspended { .. } => TaskEventKind::RunSuspended,
            Self::RunSucceeded { .. } => TaskEventKind::RunSucceeded,
            Self::RunFailed { .. } => TaskEventKind::RunFailed,
            Self::RunRetryQueued { .. } => TaskEventKind::RunRetryQueued,
            Self::TaskSucceeded => TaskEventKind::TaskSucceeded,
            Self::TaskFailed { .. } => TaskEventKind::TaskFailed,
            Self::TaskCancelled => TaskEventKind::TaskCancelled,
        }
    }
}

/// Versioned event with a monotonic per-Task revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskEventEnvelope {
    /// Versioned envelope metadata.
    pub header: ContractHeader,
    /// Task owning the event stream.
    pub task_id: TaskId,
    /// Monotonic revision assigned by Task storage.
    pub revision: u64,
    /// Immutable Task lifecycle fact.
    pub event: TaskEvent,
}

impl TaskEventEnvelope {
    /// Rejects a header that does not declare the Task event schema.
    pub fn validate_schema(&self) -> Result<(), crate::common::EnvelopeSchemaError> {
        self.header.validate_schema(ContractSchema::TaskEvent)
    }
}

impl<'de> Deserialize<'de> for TaskEventEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEnvelope {
            header: ContractHeader,
            task_id: TaskId,
            revision: u64,
            event: TaskEvent,
        }

        let wire = WireEnvelope::deserialize(deserializer)?;
        let envelope = Self {
            header: wire.header,
            task_id: wire.task_id,
            revision: wire.revision,
            event: wire.event,
        };
        envelope.validate_schema().map_err(de::Error::custom)?;
        Ok(envelope)
    }
}
