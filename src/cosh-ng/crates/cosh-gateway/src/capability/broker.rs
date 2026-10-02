//! Policy orchestration and permit verification without executing OS effects.

use cosh_gateway_contracts::{
    capability::{
        ApprovalRequest, CapabilityDecision, CapabilityRequest, CapabilityScope, DenialCode,
        ExecutionPermit, OperationDescriptor, RuntimeExecutionFence,
    },
    common::{ActorRef, BoundedText, Digest, TargetRef},
    ids::{ActorId, ExecutionId, PermitId, RunId, TaskId},
};
use thiserror::Error;

/// Authoritative parent identities against which a request is admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentBinding {
    /// Task that owns the capability request.
    pub task_id: TaskId,
    /// Active Run that produced the request.
    pub run_id: RunId,
    /// Complete authenticated actor provenance for policy evaluation.
    pub actor: ActorRef,
}

/// Content pinned by trusted admission before policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritativeRequestBinding {
    /// Exact target selected by trusted admission.
    pub target: TargetRef,
    /// Immutable identity resolved for the selected target.
    pub target_identity_digest: Digest,
    /// Exact Runtime and renewable Run-lease fence requesting authority.
    pub runtime_fence: RuntimeExecutionFence,
    /// Complete normalized operation descriptor shown to policy.
    pub operation: OperationDescriptor,
    /// Digest of the complete canonical operation.
    pub operation_digest: Digest,
    /// Exact resource and access scope shown to policy.
    pub requested_scope: CapabilityScope,
    /// Digest of the complete Runtime input shown to policy.
    pub input_digest: Digest,
}

/// Time and parent state supplied by the Task coordinator at authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    /// Current wall-clock time in milliseconds since the Unix epoch.
    pub now_ms: u64,
    /// Authoritative Task, Run, and actor relationship.
    pub parent: ParentBinding,
    /// Target, operation, digest, and scope pinned by trusted admission.
    pub binding: AuthoritativeRequestBinding,
}

/// Provider-neutral policy result consumed by the capability broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Policy denies the request without issuing executable authority.
    Deny {
        /// Stable denial classification.
        code: DenialCode,
        /// Redacted explanation safe for the requesting Runtime.
        safe_message: BoundedText,
    },
    /// Policy requires a durable actor decision before re-authorization.
    RequireApproval {
        /// Redacted explanation shown to an authorized approver.
        summary: BoundedText,
        /// Policy revision that evaluated the request.
        policy_revision: u64,
        /// Latest millisecond timestamp at which this decision remains valid.
        valid_until_ms: u64,
    },
    /// Policy permits one exact execution within a bounded lifetime.
    Allow {
        /// Policy revision that evaluated the request.
        policy_revision: u64,
        /// Latest millisecond timestamp at which this decision remains valid.
        valid_until_ms: u64,
    },
}

/// Failure returned by a policy adapter before a decision is available.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyError {
    /// Policy could not be loaded or evaluated safely.
    #[error("capability policy is unavailable")]
    Unavailable,
}

/// Evaluates a validated capability request without issuing a permit directly.
pub trait PolicyPort {
    /// Returns the current policy decision for the exact normalized request.
    fn evaluate(&self, request: &CapabilityRequest) -> Result<PolicyDecision, PolicyError>;
}

/// Exact values that a caller must prove before consuming a permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitExpectation {
    /// Permit selected for atomic validation and consumption.
    pub permit_id: PermitId,
    /// Actor attempting to consume the permit.
    pub actor_id: ActorId,
    /// Task under which execution is being admitted.
    pub task_id: TaskId,
    /// Run under which execution is being admitted.
    pub run_id: RunId,
    /// Execution identity presented to the target adapter.
    pub execution_id: ExecutionId,
    /// Exact target selected immediately before execution.
    pub target: TargetRef,
    /// Immutable target identity selected immediately before execution.
    pub target_identity_digest: Digest,
    /// Exact Runtime and Run-lease fence selected immediately before execution.
    pub runtime_fence: RuntimeExecutionFence,
    /// Digest of the normalized operation about to execute.
    pub operation_digest: Digest,
    /// Digest of the complete Runtime input about to execute.
    pub input_digest: Digest,
    /// Policy revision expected by the execution path.
    pub policy_revision: u64,
    /// Current wall-clock time in milliseconds since the Unix epoch.
    pub now_ms: u64,
}

/// A permit that passed exact binding checks and was consumed atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitClaim {
    permit: ExecutionPermit,
}

impl PermitClaim {
    /// Returns the consumed permit for audit and execution correlation.
    #[must_use]
    pub fn permit(&self) -> &ExecutionPermit {
        &self.permit
    }
}

/// Failures produced by the atomic permit ledger boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PermitStoreError {
    /// The generated permit identity already exists.
    #[error("permit already exists")]
    AlreadyExists,
    /// The selected permit is unknown.
    #[error("permit not found")]
    NotFound,
    /// The permit has reached or passed its expiry.
    #[error("permit expired")]
    Expired,
    /// The single-use permit was already consumed.
    #[error("permit already consumed")]
    AlreadyConsumed,
    /// The permit is not restricted to one use.
    #[error("permit is not single-use")]
    NotSingleUse,
    /// The presented actor differs from the permit binding.
    #[error("permit actor mismatch")]
    ActorMismatch,
    /// The presented Task differs from the permit binding.
    #[error("permit Task mismatch")]
    TaskMismatch,
    /// The presented Run differs from the permit binding.
    #[error("permit Run mismatch")]
    RunMismatch,
    /// The presented Execution differs from the permit binding.
    #[error("permit Execution mismatch")]
    ExecutionMismatch,
    /// The presented target differs from the permit binding.
    #[error("permit target mismatch")]
    TargetMismatch,
    /// The presented immutable target identity differs from the permit binding.
    #[error("permit target identity mismatch")]
    TargetIdentityMismatch,
    /// The presented Runtime or Run-lease fence differs from the permit binding.
    #[error("permit Runtime fence mismatch")]
    RuntimeFenceMismatch,
    /// The presented operation digest differs from the permit binding.
    #[error("permit operation digest mismatch")]
    OperationMismatch,
    /// The presented Runtime input digest differs from the permit binding.
    #[error("permit input digest mismatch")]
    InputMismatch,
    /// The request identity was already bound to different authority.
    #[error("capability request already issued with different authority")]
    RequestConflict,
    /// The presented policy revision differs from the permit binding.
    #[error("permit policy revision mismatch")]
    PolicyRevisionMismatch,
    /// The ledger cannot prove a safe state transition.
    #[error("permit store is unavailable")]
    Unavailable,
}

/// Stores issued permits and validates plus consumes them in one atomic step.
pub trait PermitStore {
    /// Returns the first decision for an equivalent request retry, if present.
    fn replay(
        &self,
        request: &CapabilityRequest,
    ) -> Result<Option<CapabilityDecision>, PermitStoreError>;

    /// Atomically records or replays the first authorization decision.
    ///
    /// Implementations must return the first decision when an equivalent retry
    /// reuses a request identity and reject any changed request content.
    fn issue_or_replay(
        &self,
        request: &CapabilityRequest,
        decision: CapabilityDecision,
    ) -> Result<CapabilityDecision, PermitStoreError>;

    /// Validates every expected binding and atomically consumes the permit.
    fn consume(&self, expectation: &PermitExpectation)
        -> Result<ExecutionPermit, PermitStoreError>;
}

/// Fail-closed Capability Broker validation and dependency errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrokerError {
    /// The request has reached or passed its deadline.
    #[error("capability request expired")]
    RequestExpired,
    /// Request Task does not match the authoritative parent binding.
    #[error("capability request Task mismatch")]
    RequestTaskMismatch,
    /// Request Run does not match the authoritative parent binding.
    #[error("capability request Run mismatch")]
    RequestRunMismatch,
    /// Request actor does not match the authenticated parent binding.
    #[error("capability request actor mismatch")]
    RequestActorMismatch,
    /// Request target, operation, digest, or scope differs from trusted admission.
    #[error("capability request content mismatch")]
    RequestContentMismatch,
    /// Policy returned an invalid zero revision.
    #[error("capability policy revision must be non-zero")]
    InvalidPolicyRevision,
    /// Policy authority is already expired at evaluation time.
    #[error("capability policy decision expired")]
    PolicyDecisionExpired,
    /// The policy adapter failed without a decision.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// Permit issuance or consumption failed closed.
    #[error(transparent)]
    Permit(#[from] PermitStoreError),
}

/// Pure orchestration for policy decisions and single-use execution permits.
///
/// The Broker treats `CapabilityRequest::operation_digest` as the trusted
/// ingress digest of the complete canonical operation. It never substitutes
/// the narrower argument-only digest when issuing authority.
#[derive(Debug)]
pub struct CapabilityBroker<P, S> {
    policy: P,
    permits: S,
}

impl<P, S> CapabilityBroker<P, S>
where
    P: PolicyPort,
    S: PermitStore,
{
    /// Creates a broker around explicit policy and permit-ledger boundaries.
    #[must_use]
    pub fn new(policy: P, permits: S) -> Self {
        Self { policy, permits }
    }

    /// Evaluates an admitted request and returns deny, approval, or permit.
    ///
    /// # Errors
    ///
    /// Fails when the request is expired, its parent identities do not match,
    /// policy output is invalid, or the permit cannot be recorded atomically.
    pub fn authorize(
        &self,
        request: &CapabilityRequest,
        context: &RequestContext,
    ) -> Result<CapabilityDecision, BrokerError> {
        validate_request(request, context)?;
        if let Some(decision) = self.permits.replay(request)? {
            return Ok(decision);
        }
        let decision = match self.policy.evaluate(request)? {
            PolicyDecision::Deny { code, safe_message } => {
                CapabilityDecision::Deny { code, safe_message }
            }
            PolicyDecision::RequireApproval {
                summary,
                policy_revision,
                valid_until_ms,
            } => {
                validate_policy_authority(policy_revision, valid_until_ms, context.now_ms)?;
                CapabilityDecision::RequireApproval {
                    approval: ApprovalRequest {
                        approval_id: cosh_gateway_contracts::ids::ApprovalId::new(),
                        request_id: request.request_id.clone(),
                        task_id: request.task_id.clone(),
                        run_id: request.run_id.clone(),
                        summary,
                        expires_at_ms: valid_until_ms.min(request.expires_at_ms),
                    },
                }
            }
            PolicyDecision::Allow {
                policy_revision,
                valid_until_ms,
            } => {
                validate_policy_authority(policy_revision, valid_until_ms, context.now_ms)?;
                let permit = ExecutionPermit {
                    permit_id: PermitId::new(),
                    request_id: request.request_id.clone(),
                    actor_id: request.actor.actor_id.clone(),
                    approval_id: None,
                    task_id: request.task_id.clone(),
                    run_id: request.run_id.clone(),
                    execution_id: ExecutionId::new(),
                    target: request.target.clone(),
                    target_identity_digest: context.binding.target_identity_digest.clone(),
                    runtime_fence: context.binding.runtime_fence.clone(),
                    operation_digest: request.operation_digest.clone(),
                    input_digest: request.input_digest.clone(),
                    policy_revision,
                    valid_until_ms: valid_until_ms.min(request.expires_at_ms),
                    single_use: true,
                };
                CapabilityDecision::Permit { permit }
            }
        };
        self.permits
            .issue_or_replay(request, decision)
            .map_err(Into::into)
    }

    /// Atomically validates and consumes one exact single-use permit.
    ///
    /// This method stops before any OS executor or target adapter is invoked.
    ///
    /// # Errors
    ///
    /// Fails closed for an unknown, expired, already consumed, non-single-use,
    /// or incorrectly bound permit, and when the ledger is unavailable.
    pub fn claim(&self, expectation: &PermitExpectation) -> Result<PermitClaim, BrokerError> {
        let permit = self.permits.consume(expectation)?;
        Ok(PermitClaim { permit })
    }
}

fn validate_request(
    request: &CapabilityRequest,
    context: &RequestContext,
) -> Result<(), BrokerError> {
    if request.expires_at_ms <= context.now_ms {
        return Err(BrokerError::RequestExpired);
    }
    if request.task_id != context.parent.task_id {
        return Err(BrokerError::RequestTaskMismatch);
    }
    if request.run_id != context.parent.run_id {
        return Err(BrokerError::RequestRunMismatch);
    }
    if request.actor != context.parent.actor {
        return Err(BrokerError::RequestActorMismatch);
    }
    let content = AuthoritativeRequestBinding {
        target: request.target.clone(),
        target_identity_digest: context.binding.target_identity_digest.clone(),
        runtime_fence: context.binding.runtime_fence.clone(),
        operation: request.operation.clone(),
        operation_digest: request.operation_digest.clone(),
        requested_scope: request.requested_scope.clone(),
        input_digest: request.input_digest.clone(),
    };
    if content != context.binding {
        return Err(BrokerError::RequestContentMismatch);
    }
    Ok(())
}

fn validate_policy_authority(
    policy_revision: u64,
    valid_until_ms: u64,
    now_ms: u64,
) -> Result<(), BrokerError> {
    if policy_revision == 0 {
        return Err(BrokerError::InvalidPolicyRevision);
    }
    if valid_until_ms <= now_ms {
        return Err(BrokerError::PolicyDecisionExpired);
    }
    Ok(())
}
