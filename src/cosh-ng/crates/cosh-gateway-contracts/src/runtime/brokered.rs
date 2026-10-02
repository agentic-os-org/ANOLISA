/// Bounded token accounting reported by an Agent Runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeUsage {
    /// Input tokens consumed during the Run.
    pub input_tokens: u64,
    /// Output tokens produced during the Run.
    pub output_tokens: u64,
}

/// Redacted description of a Runtime-observed tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSummary {
    /// Provider-independent tool name.
    pub name: BoundedName,
    /// Safe bounded description suitable for presentation.
    pub summary: BoundedText,
}

/// Receipt proving Gateway durably took ownership of a brokered Runtime request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokeredRequestAcknowledgement {
    /// Capability request now owned by Gateway.
    pub request_id: RequestId,
    /// Durable approval created before the Runtime may release its callback.
    pub approval_id: ApprovalId,
}

/// Terminal delivery for one brokered Runtime request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokeredExecutionDelivery {
    /// Capability request whose callback receives this result.
    pub request_id: RequestId,
    /// Typed terminal outcome; no permit is exposed to the Runtime.
    pub outcome: BrokeredExecutionOutcome,
}

/// Provider-neutral terminal outcome of a COSH-brokered execution request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BrokeredExecutionOutcome {
    /// Policy or approval denied the request before any effect was executed.
    Denied {
        /// Stable denial classification.
        code: DenialCode,
        /// Redacted explanation safe to return to the Runtime.
        safe_message: BoundedText,
    },
    /// The COSH execution target completed the typed operation successfully.
    Succeeded {
        /// Governed execution that produced the result.
        execution_id: ExecutionId,
        /// Operation-specific bounded result.
        result: BrokeredOperationResult,
    },
    /// The COSH execution target produced a known terminal failure.
    Failed {
        /// Governed execution that failed.
        execution_id: ExecutionId,
        /// Safe failure that excludes raw target output.
        error: ContractError,
    },
    /// Recovery cannot prove whether the side effect completed.
    Uncertain {
        /// Governed execution requiring reconciliation.
        execution_id: ExecutionId,
        /// Safe reason that does not expose target internals.
        error: ContractError,
    },
}

/// Typed successful result returned by a COSH execution target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "result", rename_all = "snake_case")]
pub enum BrokeredOperationResult {
    /// Result of creating a checkpoint for the bound workspace.
    WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1Result),
}

/// Result of a brokered workspace checkpoint creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCheckpointCreateV1Result {
    /// Broker-allocated checkpoint identity from the request.
    pub checkpoint_id: CheckpointId,
    /// Target-reported checkpoint creation outcome.
    pub outcome: WorkspaceCheckpointCreateV1Outcome,
}

/// Target-reported outcome for a workspace checkpoint creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkspaceCheckpointCreateV1Outcome {
    /// The target created a new snapshot.
    Created {
        /// Opaque bounded snapshot identity allocated by the checkpoint target.
        snapshot_id: crate::common::BoundedOpaque,
    },
    /// The target safely skipped creation without producing a snapshot.
    Skipped {
        /// Redacted bounded reason for the skip.
        reason: BoundedText,
    },
}

/// Describes where a tool side effect is ultimately enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAuthority {
    /// COSH observes an Agent-native execution but cannot enforce permit consumption.
    ProviderNativeObserved,
    /// A COSH execution target validates and consumes a broker-issued permit.
    CoshBrokered,
}

/// Provider-owned identities bound to one ACP permission callback.
///
/// Only digests cross the Runtime boundary. Raw provider session, request,
/// tool-call, option, and callback values remain confined to the live adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPermissionCallbackV2 {
    /// Opaque ACP session identity bound by the codec.
    pub provider_session_digest: crate::common::Digest,
    /// Type-preserving digest of the JSON-RPC request identifier.
    pub provider_request_id_digest: crate::common::Digest,
    /// Digest of the provider-owned tool-call identifier.
    pub provider_tool_call_id_digest: crate::common::Digest,
    /// Order-sensitive digest of every offered permission option.
    pub ordered_option_set_digest: crate::common::Digest,
    /// Digest of the complete normalized callback carrier.
    pub callback_payload_digest: crate::common::Digest,
    /// Trusted normalizer's operation digest for this callback.
    pub normalized_operation_digest: crate::common::Digest,
}

/// Private cosh-core identities bound to one approval-gated tool callback.
///
/// Raw Core request and provider tool-use identifiers remain confined to the
/// live bridge. This fence is deliberately distinct from the ACP callback
/// contract because Core does not emit an ACP permission callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorePermissionCallbackV1 {
    /// Digest of the private Core control-request identifier.
    pub private_request_id_digest: crate::common::Digest,
    /// Digest of the provider-owned tool-use identifier carried by Core.
    pub private_tool_use_id_digest: crate::common::Digest,
    /// Digest of the complete normalized private callback carrier.
    pub callback_payload_digest: crate::common::Digest,
    /// Trusted bridge normalizer's operation digest for this callback.
    pub normalized_operation_digest: crate::common::Digest,
}

/// Exact Runtime identity of one pending permission callback.
///
/// Resolution callers must reproduce every field. A request identity alone is
/// insufficient because it does not fence a restarted Runtime or a later turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePermissionRef {
    /// Fenced Runtime binding that emitted the callback.
    pub binding_id: RuntimeBindingId,
    /// Runtime generation copied from the durable binding.
    pub runtime_generation: u64,
    /// Monotonic event sequence carrying the callback.
    pub event_sequence: u64,
    /// Run that owns the callback.
    pub run_id: RunId,
    /// Prompt turn waiting for the decision.
    pub turn_id: TurnId,
    /// Stable COSH tool identity when the callback belongs to a tool snapshot.
    pub tool_use_id: Option<ToolUseId>,
    /// COSH capability request being resolved.
    pub request_id: RequestId,
    /// Complete provider callback fence emitted by new Runtime bridges.
    ///
    /// Legacy durable records deserialize with `None` for recovery only. They
    /// must never regain permission-dispatch authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<ProviderPermissionCallbackV2>,
    /// Complete private Core callback fence emitted by workspace-write bridges.
    ///
    /// Legacy durable records deserialize with `None`. A live permission must
    /// carry exactly one of `callback` or `core_callback` before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_callback: Option<CorePermissionCallbackV1>,
}

/// Exact Runtime identity of one pending COSH-brokered execution callback.
///
/// The typed operation is part of the fence, so a callback cannot be rebound
/// to another checkpoint identity after Gateway durably accepts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokeredExecutionRef {
    /// Fenced Runtime binding that emitted the request.
    pub binding_id: RuntimeBindingId,
    /// Runtime generation copied from the durable binding.
    pub runtime_generation: u64,
    /// Monotonic event sequence carrying the request.
    pub event_sequence: u64,
    /// Run that owns the request.
    pub run_id: RunId,
    /// Prompt turn waiting for takeover and a terminal result.
    pub turn_id: TurnId,
    /// Stable COSH tool identity when the request belongs to a tool snapshot.
    pub tool_use_id: Option<ToolUseId>,
    /// COSH capability request being brokered.
    pub request_id: RequestId,
    /// Closed typed operation and its broker-allocated identity.
    pub operation: BrokeredOperation,
}
