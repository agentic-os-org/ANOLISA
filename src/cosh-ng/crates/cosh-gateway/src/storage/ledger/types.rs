/// Idempotent command metadata shared by ledger mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerCommand {
    /// Authenticated actor owning the idempotency namespace.
    pub actor_id: ActorId,
    /// Caller-scoped replay key.
    pub idempotency_key: IdempotencyKey,
    /// Canonical digest of the complete command.
    pub command_digest: Digest,
    /// Durable mutation timestamp in Unix milliseconds.
    pub committed_at_ms: u64,
}

/// Result of an idempotent ledger mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerOutcome<T> {
    /// A new durable mutation was applied.
    Applied(T),
    /// An identical command returned its original durable result.
    Replayed(T),
}

/// Durable approval lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// Waiting for the bound actor's decision.
    Pending,
    /// Approved for subsequent permit issuance.
    Approved,
    /// Explicitly denied.
    Denied,
    /// Deadline passed before a decision.
    Expired,
    /// Owning run cancelled the request.
    Cancelled,
}

/// Durable approval row with all authorization bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Approval identity.
    pub approval_id: ApprovalId,
    /// Capability request identity.
    pub request_id: RequestId,
    /// Actor authorized to resolve the approval.
    pub actor_id: ActorId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Bound target.
    pub target: TargetRef,
    /// Immutable resolved target identity for COSH-brokered authority.
    pub target_identity_digest: Option<Digest>,
    /// Runtime and Run-lease fence for COSH-brokered authority.
    pub runtime_fence: Option<RuntimeExecutionFence>,
    /// Bound normalized operation digest.
    pub operation_digest: Digest,
    /// Bound complete Runtime input digest.
    pub input_digest: Digest,
    /// Exact provider callback binding, absent for legacy or COSH-brokered approvals.
    pub permission: Option<RuntimePermissionRef>,
    /// Current lifecycle state.
    pub state: ApprovalState,
    /// Optimistic revision.
    pub revision: u64,
    /// Fail-closed decision deadline.
    pub expires_at_ms: u64,
    /// Actor that made an explicit decision.
    pub decided_by_actor_id: Option<ActorId>,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Requested approval resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResolution {
    /// Apply an explicit allow-once decision.
    Decide(ApprovalDecision),
    /// Cancel because the owning Run is no longer active.
    Cancel,
}

/// Provider-native decision prepared for exactly one Runtime callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPermissionDispatchDecision {
    /// Select the provider's one-shot allow option without issuing a COSH permit.
    AllowOnce,
    /// Select the provider's one-shot rejection option.
    Deny,
}

/// Durable provider-native resolution dispatch lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPermissionDispatchState {
    /// Approval resolution committed and zero response bytes have been written.
    Prepared,
    /// The non-replayable boundary committed before the first response byte.
    WriteStarted,
    /// The live transport write returned; this does not prove provider acknowledgement.
    Written,
    /// Zero bytes were written, but the original provider session cannot be recovered.
    Abandoned,
    /// Restart or transport failure made delivery indeterminate.
    Unknown,
}

/// Durable provider-native response bound to one exact Runtime callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPermissionDispatchRecord {
    /// Approval whose resolution created this dispatch.
    pub approval_id: ApprovalId,
    /// Actor authorized to resolve the approval.
    pub actor_id: ActorId,
    /// Task owning the provider callback.
    pub task_id: TaskId,
    /// Run owning the provider callback.
    pub run_id: RunId,
    /// Complete Runtime generation, Turn, tool, and request binding.
    pub permission: RuntimePermissionRef,
    /// Provider-native one-shot response.
    pub decision: ProviderPermissionDispatchDecision,
    /// Current dispatch lifecycle state.
    pub state: ProviderPermissionDispatchState,
    /// Optimistic dispatch revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Durable permit lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermitState {
    /// Available for one exact execution.
    Issued,
    /// Atomically consumed when execution started.
    Consumed,
    /// Deadline passed before consumption.
    Expired,
    /// Revoked before consumption.
    Revoked,
}

/// Durable execution-permit row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermitRecord {
    /// Complete immutable permit contract.
    pub permit: ExecutionPermit,
    /// Current lifecycle state.
    pub state: PermitState,
    /// Consumption timestamp.
    pub consumed_at_ms: Option<u64>,
    /// Creation timestamp.
    pub created_at_ms: u64,
}

/// Durable execution lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    /// Permit was issued but the side effect has not started.
    Planned,
    /// Permit consumption and execution start committed atomically.
    Started,
    /// A success receipt was committed.
    Succeeded,
    /// A failure receipt was committed.
    Failed,
    /// Recovery found a started execution without a conclusive receipt.
    Uncertain,
}

/// Durable governed execution row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    /// Execution identity.
    pub execution_id: ExecutionId,
    /// Actor authorized by the permit.
    pub actor_id: ActorId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Bound target.
    pub target: TargetRef,
    /// Immutable resolved target identity, absent only for pre-v6 legacy rows.
    pub target_identity_digest: Option<Digest>,
    /// Runtime and Run-lease fence, absent only for pre-v6 legacy rows.
    pub runtime_fence: Option<RuntimeExecutionFence>,
    /// Broker-specific pre-effect lifecycle, absent for provider-native rows.
    pub broker_state: Option<BrokerExecutionState>,
    /// Timestamp at which exact authority was claimed before any external effect.
    pub claimed_at_ms: Option<u64>,
    /// Required security-boundary audit proof persisted before target invocation.
    pub start_audit_proof_digest: Option<Digest>,
    /// Durable availability of a typed successful brokered result.
    pub typed_result_state: TypedExecutionResultState,
    /// Bound operation digest.
    pub operation_digest: Digest,
    /// Bound Runtime input digest.
    pub input_digest: Digest,
    /// Current lifecycle state.
    pub state: ExecutionState,
    /// Optimistic revision.
    pub revision: u64,
    /// Start timestamp.
    pub started_at_ms: Option<u64>,
    /// Terminal or uncertainty timestamp.
    pub completed_at_ms: Option<u64>,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Started brokered effect plus the exact request needed for read-only recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokeredExecutionRecoveryCandidate {
    /// Started durable execution.
    pub execution: ExecutionRecord,
    /// Original request including its provider-owned opaque binding.
    pub request: BrokeredRequestRecord,
}

/// Availability of the typed result associated with an execution row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedExecutionResultState {
    /// The execution has no successful typed result.
    NotApplicable,
    /// A validated typed result is durable in the result ledger.
    Available,
    /// A pre-v8 successful row cannot be reconstructed safely.
    LegacyUnavailable,
}

/// Exact bindings presented when consuming a permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionClaim {
    /// Permit to consume.
    pub permit_id: PermitId,
    /// Execution authorized by the permit.
    pub execution_id: ExecutionId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Exact target.
    pub target: TargetRef,
    /// Immutable resolved target identity.
    pub target_identity_digest: Digest,
    /// Exact Runtime and renewable Run-lease fence authorized by the permit.
    pub runtime_fence: RuntimeExecutionFence,
    /// Exact normalized operation digest.
    pub operation_digest: Digest,
    /// Exact complete Runtime input digest.
    pub input_digest: Digest,
    /// Policy revision expected by the executor.
    pub policy_revision: u64,
    /// Current coordinator lease fencing the owning Task and Run.
    pub lease: LeaseClaim,
}

/// Durable COSH-brokered lifecycle around the external-effect boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerExecutionState {
    /// Permit exists but has not been consumed.
    Planned,
    /// Authority is consumed, while the external effect is still known not to have started.
    Claimed,
    /// Security audit proof committed and the external effect may have started.
    Started,
    /// Recovery conclusively established that a claimed effect never started.
    KnownNoEffect,
}

/// Proof returned by a security audit boundary before target invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityAuditProof {
    /// Digest of the complete durable audit record.
    pub proof_digest: Digest,
    /// Time at which the security-boundary record became durable.
    pub persisted_at_ms: u64,
}

/// Complete durable COSH-brokered request admitted before approval or execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokeredRequestRecord {
    /// Provider-neutral capability request.
    pub request: CapabilityRequest,
    /// Typed operation that the COSH target adapter may execute.
    pub operation: BrokeredOperation,
    /// Storage-owned digest of the canonical typed operation JSON.
    pub typed_operation_digest: Digest,
    /// Immutable resolved target identity.
    pub target_identity_digest: Digest,
    /// Exact Runtime and renewable Run-lease fence.
    pub runtime_fence: RuntimeExecutionFence,
    /// Provider-owned versioned admission binding, absent for legacy rows.
    pub provider_binding: Option<BoundedOpaque>,
    /// Optional approval created with this request.
    pub approval_id: Option<ApprovalId>,
    /// Durable creation timestamp.
    pub created_at_ms: u64,
}

/// Runtime callback message represented by a durable brokered dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokeredRuntimeDispatchKind {
    /// Gateway accepted ownership of a request awaiting approval.
    Acknowledgement,
    /// Gateway is returning a terminal denial or execution outcome.
    Result,
}

/// Non-replayable brokered callback dispatch lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokeredRuntimeDispatchState {
    /// All prerequisites and the complete outbound payload digest are durable.
    Prepared,
    /// Dispatch intent is durable and the transport write may have happened.
    Started,
    /// The live Runtime accepted the callback message.
    Delivered,
    /// Delivery is indeterminate and must never be retried.
    Unknown,
}

/// Durable source proving a brokered callback is ready to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum BrokeredRuntimeDispatchSource {
    /// Pending approval and WaitingApproval Task state back an acknowledgement.
    ApprovalPending {
        /// Durable pending approval.
        approval_id: ApprovalId,
    },
    /// Durable explicit denial backs a terminal denied result.
    ApprovalDenied {
        /// Durable denied approval.
        approval_id: ApprovalId,
    },
    /// A terminal, uncertain, or known-no-effect execution backs a result.
    Execution {
        /// Durable governed execution.
        execution_id: ExecutionId,
    },
}

/// Exact durable Runtime callback dispatch binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokeredRuntimeDispatchRecord {
    /// Complete callback reference, including Runtime generation and event sequence.
    pub brokered: BrokeredExecutionRef,
    /// Authenticated Task owner.
    pub actor_id: ActorId,
    /// Owning Task inferred from the durable request.
    pub task_id: TaskId,
    /// Callback message kind.
    pub kind: BrokeredRuntimeDispatchKind,
    /// Digest of the complete acknowledgement or result payload.
    pub payload_digest: Digest,
    /// Durable fact authorizing preparation.
    pub source: BrokeredRuntimeDispatchSource,
    /// Current non-replayable dispatch state.
    pub state: BrokeredRuntimeDispatchState,
    /// Optimistic dispatch revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Conclusive execution result persisted after a started side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCompletion {
    /// Execution to complete.
    pub execution_id: ExecutionId,
    /// Expected execution revision.
    pub expected_revision: u64,
    /// Whether the governed operation succeeded.
    pub succeeded: bool,
    /// Digest of the complete evidence receipt.
    pub receipt_digest: Digest,
    /// Optional redacted bounded detail.
    pub safe_detail: Option<BoundedText>,
    /// Typed successful result; required exactly when `succeeded` is true.
    pub typed_result: Option<BrokeredOperationResult>,
}

/// Durable typed result and all authority bindings used to validate it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokeredExecutionResultRecord {
    /// Governed execution that produced the result.
    pub execution_id: ExecutionId,
    /// Brokered capability request owning the callback.
    pub request_id: RequestId,
    /// Authenticated owner.
    pub actor_id: ActorId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Validated provider-neutral typed result.
    pub result: BrokeredOperationResult,
    /// Storage-owned digest of the complete typed result JSON.
    pub result_digest: Digest,
    /// Exact typed operation whose result shape was validated.
    pub operation: BrokeredOperation,
    /// Bound normalized operation digest.
    pub operation_digest: Digest,
    /// Bound complete Runtime input digest.
    pub input_digest: Digest,
    /// Immutable resolved target identity.
    pub target_identity_digest: Digest,
    /// Runtime and lease-generation authority fence.
    pub runtime_fence: RuntimeExecutionFence,
    /// Atomic completion timestamp.
    pub committed_at_ms: u64,
}

/// Durable runtime binding lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBindingState {
    /// Runtime generation may emit events.
    Active,
    /// Runtime was closed cleanly.
    Closed,
    /// Recovery fenced a runtime whose liveness was not proven.
    Lost,
}

/// Durable fenced runtime binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBindingRecord {
    /// Complete binding contract.
    pub binding: RuntimeBindingRef,
    /// Authenticated Task owner.
    pub actor_id: ActorId,
    /// Current binding state.
    pub state: RuntimeBindingState,
    /// Last accepted monotonic event sequence.
    pub last_sequence: u64,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Durable lifecycle of one exact Runtime input request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeInputRequestState {
    /// Waiting for the authenticated actor's response.
    Pending,
    /// A typed response and digest were committed atomically.
    Resolved,
    /// The response deadline elapsed before resolution.
    Expired,
    /// Run convergence cancelled the pending request.
    Cancelled,
}

/// Durable bounded Runtime input request and its exact authority fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuntimeInputRequestRecord {
    /// Complete bounded request presentation.
    pub request: RuntimeInputRequest,
    /// Authenticated Task owner.
    pub actor_id: ActorId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Runtime binding that emitted the request.
    pub binding_id: RuntimeBindingId,
    /// Exact Runtime process instance.
    pub runtime_instance_id: RuntimeInstanceId,
    /// Exact Runtime process generation.
    pub runtime_generation: u64,
    /// Monotonic Runtime event sequence consumed atomically with the request.
    pub runtime_sequence: u64,
    /// Run lease generation authoritative at request admission.
    pub lease_generation: u64,
    /// Run lease revision authoritative at request admission.
    pub lease_revision: u64,
    /// Current durable request lifecycle.
    pub state: RuntimeInputRequestState,
    /// Digest of the private typed response after resolution.
    pub response_digest: Option<Digest>,
    /// Optimistic request revision.
    pub revision: u64,
    /// Fail-closed response deadline.
    pub expires_at_ms: u64,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Durable lifecycle of one private Runtime input response dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeInputDispatchState {
    /// Raw typed response is durable but no transport write has started.
    Prepared,
    /// The non-replayable transport boundary was committed.
    Started,
    /// Runtime transport acknowledged the one-shot response.
    Delivered,
    /// Delivery became indeterminate and must never be retried.
    Unknown,
}

/// Private typed input response held only in the dispatch ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuntimeInputDispatchRecord {
    /// Exact Runtime request being resolved.
    pub request_id: InputRequestId,
    /// Authenticated Task owner.
    pub actor_id: ActorId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Owning Run.
    pub run_id: RunId,
    /// Typed bounded response excluded from Task history and receipts.
    pub response: RuntimeInputResponse,
    /// Canonical digest recorded in the Task event.
    pub response_digest: Digest,
    /// Current dispatch lifecycle.
    pub state: RuntimeInputDispatchState,
    /// Optimistic dispatch revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RuntimeInputDispatchReceipt {
    request_id: InputRequestId,
    response_digest: Digest,
    state: RuntimeInputDispatchState,
    revision: u64,
}

/// Run-lease mutation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseCommand {
    /// Common idempotent command metadata.
    pub command: LedgerCommand,
    /// Task protected by the lease.
    pub task_id: TaskId,
    /// Run protected by the lease.
    pub run_id: RunId,
    /// Bounded coordinator instance identity.
    pub lease_owner: BoundedOpaque,
    /// Requested lease deadline.
    pub expires_at_ms: u64,
}

/// Exact fencing claim required to release a Run lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseClaim {
    /// Task protected by the lease.
    pub task_id: TaskId,
    /// Run protected by the lease.
    pub run_id: RunId,
    /// Coordinator instance holding the lease.
    pub lease_owner: BoundedOpaque,
    /// Expected fencing generation.
    pub generation: u64,
    /// Expected optimistic revision.
    pub revision: u64,
}

/// Durable fenced lease for one Run coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLeaseRecord {
    /// Owning Task.
    pub task_id: TaskId,
    /// Protected Run.
    pub run_id: RunId,
    /// Authenticated Task owner.
    pub actor_id: ActorId,
    /// Coordinator instance holding the lease.
    pub lease_owner: BoundedOpaque,
    /// Monotonic fencing generation.
    pub generation: u64,
    /// Optimistic mutation revision.
    pub revision: u64,
    /// Lease deadline.
    pub expires_at_ms: u64,
    /// Last mutation timestamp.
    pub updated_at_ms: u64,
}

/// Counts of fail-closed transitions applied during restart recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Pending approvals expired by their deadline.
    pub approvals_expired: u64,
    /// Unexpired pending approvals cancelled because stdio cannot reattach.
    pub approvals_cancelled: u64,
    /// Zero-byte provider responses abandoned with their non-reattachable sessions.
    pub permission_dispatches_abandoned: u64,
    /// Started or written provider responses made non-replayable by restart.
    pub permission_dispatches_unknown: u64,
    /// Started brokered callbacks made permanently non-replayable by restart.
    pub brokered_dispatches_unknown: u64,
    /// Pending input requests cancelled while their Tasks were suspended.
    pub runtime_input_requests_cancelled: u64,
    /// Prepared or started input responses made permanently non-replayable.
    pub runtime_input_dispatches_unknown: u64,
    /// Issued permits expired by their deadline.
    pub permits_expired: u64,
    /// Started executions marked uncertain.
    pub executions_uncertain: u64,
    /// Claimed executions conclusively recovered before any external effect.
    pub executions_known_no_effect: u64,
    /// Active runtime bindings fenced as lost.
    pub runtime_bindings_lost: u64,
}

/// Counts of brokered execution transitions recovered for one fenced Run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrokeredExecutionRecoveryReport {
    /// Claimed executions proven not to have crossed the effect boundary.
    pub executions_known_no_effect: u64,
    /// Started executions requiring reconciliation.
    pub executions_uncertain: u64,
}
