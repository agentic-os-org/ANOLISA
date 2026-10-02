//! Capability requests, approval decisions, and execution permits.

use serde::{Deserialize, Serialize};

use crate::{
    common::{ActorRef, BoundedName, BoundedText, Digest, TargetRef},
    ids::{
        ActorId, ApprovalId, CheckpointId, ExecutionId, PermitId, RequestId, RunId,
        RuntimeBindingId, TaskId,
    },
};

/// Versioned typed operation whose side effect must cross a COSH execution target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "input", rename_all = "snake_case")]
pub enum BrokeredOperation {
    /// Creates one checkpoint for the workspace already bound to the Runtime.
    WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1),
}

/// Runtime-visible input for a brokered workspace checkpoint creation.
///
/// Workspace path, daemon socket, presentation metadata, pinning, and timeout
/// remain Gateway-owned execution policy and never cross this input boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCheckpointCreateV1 {
    /// Broker-allocated identity used to correlate the requested checkpoint.
    pub checkpoint_id: CheckpointId,
}

/// Fences a permit to the exact Runtime and Run-lease generation that requested it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeExecutionFence {
    /// Fenced Runtime binding that originated the execution request.
    pub binding_id: RuntimeBindingId,
    /// Runtime generation copied from the durable binding.
    pub runtime_generation: u64,
    /// Lease generation that owned the Run when the permit was issued.
    pub lease_generation: u64,
    /// Lease revision observed at request admission for audit correlation.
    ///
    /// Renewal may advance this revision within the same generation. Execution
    /// separately proves the current exact lease claim before consuming authority.
    pub lease_revision: u64,
}

/// Normalized operation proposed by an Agent Runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationDescriptor {
    /// Operation namespace, such as process, file, package, or service.
    pub namespace: BoundedName,
    /// Operation name within the namespace.
    pub name: BoundedName,
    /// Digest of normalized operation arguments.
    pub arguments_digest: Digest,
}

/// Requested policy scope independent from a provider permission shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityScope {
    /// Resource category governed by policy.
    pub resource: BoundedName,
    /// Access mode requested for the resource.
    pub access: BoundedName,
}

/// Domain request evaluated by the capability broker before a side effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    /// COSH-owned request identity.
    pub request_id: RequestId,
    /// Task owning the request.
    pub task_id: TaskId,
    /// Run that observed the requested operation.
    pub run_id: RunId,
    /// Authenticated actor on whose behalf the Runtime acts.
    pub actor: ActorRef,
    /// Target environment affected by the operation.
    pub target: TargetRef,
    /// Normalized operation proposed by the Runtime.
    pub operation: OperationDescriptor,
    /// Digest of the complete canonical operation, including its namespace,
    /// name, and normalized arguments. A trusted ingress canonicalizes and
    /// hashes the operation before constructing this request.
    pub operation_digest: Digest,
    /// Policy scope requested by the operation.
    pub requested_scope: CapabilityScope,
    /// Digest of the complete original Runtime input.
    pub input_digest: Digest,
    /// Millisecond timestamp after which the request must fail closed.
    pub expires_at_ms: u64,
}

/// Durable approval request produced by capability policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// COSH-owned approval identity.
    pub approval_id: ApprovalId,
    /// Capability request awaiting approval.
    pub request_id: RequestId,
    /// Task owning the decision.
    pub task_id: TaskId,
    /// Run paused for the decision.
    pub run_id: RunId,
    /// Redacted human-readable explanation.
    pub summary: BoundedText,
    /// Millisecond timestamp after which the approval is stale.
    pub expires_at_ms: u64,
}

/// Human or policy response to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve the requested scope once policy issues a permit.
    Approve,
    /// Deny the requested scope.
    Deny,
}

/// Stable policy denial classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialCode {
    /// Requested capability is prohibited by policy.
    PolicyDenied,
    /// Actor lacks access to the Task or target.
    Unauthorized,
    /// Approval was denied or expired.
    ApprovalDenied,
    /// Request is stale or no longer matches active state.
    StaleRequest,
}

/// Single policy authorization bound to one normalized operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPermit {
    /// COSH-owned permit identity.
    pub permit_id: PermitId,
    /// Capability request authorized by this permit.
    pub request_id: RequestId,
    /// Actor authorized to use the permit.
    pub actor_id: ActorId,
    /// Optional approval that authorized the request.
    pub approval_id: Option<ApprovalId>,
    /// Task owning the authorization.
    pub task_id: TaskId,
    /// Run owning the authorization.
    pub run_id: RunId,
    /// Governed execution attempt authorized by the permit.
    pub execution_id: ExecutionId,
    /// Target bound to the permit.
    pub target: TargetRef,
    /// Immutable resolved target identity bound to the permit.
    pub target_identity_digest: Digest,
    /// Exact Runtime binding and Run lease that may consume the permit.
    pub runtime_fence: RuntimeExecutionFence,
    /// Digest of the normalized operation bound to the permit.
    pub operation_digest: Digest,
    /// Digest of the complete Runtime input admitted by policy.
    pub input_digest: Digest,
    /// Policy revision that produced the authorization decision.
    pub policy_revision: u64,
    /// Millisecond timestamp after which the permit is invalid.
    pub valid_until_ms: u64,
    /// Whether successful admission consumes the permit.
    pub single_use: bool,
}

/// Result of evaluating a capability request.
// Keep the established value-owned API: decisions are boundary messages, not
// retained collections where the larger stack representation would compound.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CapabilityDecision {
    /// Policy issued a permit that may authorize execution.
    Permit {
        /// Permit bound to the request and operation.
        permit: ExecutionPermit,
    },
    /// A durable approval must be resolved before a permit can be issued.
    RequireApproval {
        /// Approval request presented to an authorized actor.
        approval: ApprovalRequest,
    },
    /// Policy denied the request without issuing a permit.
    Deny {
        /// Stable reason for denial.
        code: DenialCode,
        /// Redacted human-readable explanation.
        safe_message: BoundedText,
    },
}
