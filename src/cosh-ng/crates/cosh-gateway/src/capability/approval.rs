//! Durable approval resolution and exact once-only permit issuance.

use cosh_gateway_contracts::{
    capability::{
        ApprovalDecision, ApprovalRequest, BrokeredOperation, CapabilityRequest, ExecutionPermit,
        RuntimeExecutionFence,
    },
    common::{BoundedOpaque, Digest},
    ids::{ExecutionId, PermitId},
    runtime::RuntimePermissionRef,
};
use thiserror::Error;

use crate::storage::{
    ApprovalRecord, ApprovalResolution, ApprovalState, LeaseClaim, LedgerCommand, LedgerOutcome,
    PermitRecord, ProviderPermissionDispatchRecord, SqliteTaskStore, StoreError,
};

/// Result of resolving one durable once-only approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableApprovalOutcome {
    /// The actor denied the request and no executable authority was created.
    NotPermitted(Box<ApprovalRecord>),
    /// The actor approved and an exact single-use permit was persisted.
    Permit(Box<PermitRecord>),
}

/// Complete trusted input for one approval resolution and optional permit issuance.
pub struct DurableApprovalResolution<'a> {
    /// Idempotent command that records the actor decision.
    pub resolution_command: &'a LedgerCommand,
    /// Idempotent command that records executable authority after approval.
    pub permit_command: &'a LedgerCommand,
    /// Optimistic revision of the pending approval.
    pub expected_revision: u64,
    /// Explicit once-only actor decision.
    pub decision: ApprovalDecision,
    /// Non-zero policy revision authorizing permit issuance.
    pub policy_revision: u64,
    /// Latest deadline granted by the current policy revision.
    pub policy_valid_until_ms: u64,
    /// Preallocated permit identity included in the permit command digest.
    pub permit_id: PermitId,
    /// Preallocated execution identity included in the permit command digest.
    pub execution_id: ExecutionId,
}

/// Trusted authority persisted with a COSH-brokered approval request.
pub struct BrokeredApprovalBinding<'a> {
    /// Typed operation admitted by the broker.
    pub operation: &'a BrokeredOperation,
    /// Immutable target identity resolved by trusted ingress.
    pub target_identity_digest: &'a Digest,
    /// Exact Runtime and renewable Run-lease fence requesting authority.
    pub runtime_fence: &'a RuntimeExecutionFence,
    /// Provider-owned data needed to execute or reconcile the exact admission.
    pub provider_binding: Option<&'a BoundedOpaque>,
}

/// Trusted input for one provider-native observed approval resolution.
pub struct DurableProviderApprovalResolution<'a> {
    /// Idempotent command recording the decision and prepared provider response.
    pub command: &'a LedgerCommand,
    /// Optimistic revision of the pending approval.
    pub expected_revision: u64,
    /// Explicit once-only actor decision.
    pub decision: ApprovalDecision,
    /// Exact Runtime callback being resolved.
    pub permission: &'a RuntimePermissionRef,
    /// Current Run lease fencing the live, non-reattachable provider process.
    pub lease: &'a LeaseClaim,
}

/// Fail-closed approval and permit issuance errors.
#[derive(Debug, Error)]
pub enum DurableApprovalError {
    /// Capability and approval identities or parent bindings differ.
    #[error("approval does not match the capability request")]
    BindingMismatch,
    /// Policy revision cannot represent authoritative policy state.
    #[error("approval policy revision must be non-zero")]
    InvalidPolicyRevision,
    /// Policy authority is already expired when the permit would be issued.
    #[error("approval policy authority is expired")]
    PolicyAuthorityExpired,
    /// Durable storage rejected the transition.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Coordinates durable approval evidence and subsequent permit issuance.
pub struct DurableApprovalCoordinator<'a> {
    store: &'a mut SqliteTaskStore,
}

impl<'a> DurableApprovalCoordinator<'a> {
    /// Creates a coordinator around the authoritative Gateway ledger.
    pub fn new(store: &'a mut SqliteTaskStore) -> Self {
        Self { store }
    }

    /// Persists a policy-produced approval with all execution bindings attached.
    ///
    /// # Errors
    ///
    /// Returns when the public approval does not belong to the capability
    /// request, or when the ledger cannot persist the pending decision.
    pub fn record_pending(
        &mut self,
        command: &LedgerCommand,
        request: &CapabilityRequest,
        approval: &ApprovalRequest,
        binding: BrokeredApprovalBinding<'_>,
    ) -> Result<ApprovalRecord, DurableApprovalError> {
        validate_approval_binding(request, approval)?;
        let record = ApprovalRecord {
            approval_id: approval.approval_id.clone(),
            request_id: request.request_id.clone(),
            actor_id: request.actor.actor_id.clone(),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            target: request.target.clone(),
            target_identity_digest: Some(binding.target_identity_digest.clone()),
            runtime_fence: Some(binding.runtime_fence.clone()),
            operation_digest: request.operation_digest.clone(),
            input_digest: request.input_digest.clone(),
            permission: None,
            state: ApprovalState::Pending,
            revision: 1,
            expires_at_ms: approval.expires_at_ms.min(request.expires_at_ms),
            decided_by_actor_id: None,
            created_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
        };
        Ok(ledger_value(self.store.create_brokered_approval(
            command,
            request,
            approval,
            binding.operation,
            binding.provider_binding,
            &record,
        )?))
    }

    /// Persists a provider-native approval without manufacturing COSH authority.
    ///
    /// # Errors
    ///
    /// Returns when the callback does not exactly bind the approval request or
    /// when the ledger cannot persist the pending decision.
    pub fn record_provider_pending(
        &mut self,
        command: &LedgerCommand,
        request: &CapabilityRequest,
        approval: &ApprovalRequest,
        permission: &RuntimePermissionRef,
        binding: &cosh_gateway_contracts::common::RuntimeBindingRef,
        lease: &LeaseClaim,
    ) -> Result<ApprovalRecord, DurableApprovalError> {
        validate_approval_binding(request, approval)?;
        if permission.request_id != request.request_id
            || permission.run_id != request.run_id
            || permission.runtime_generation == 0
            || permission.event_sequence == 0
        {
            return Err(DurableApprovalError::BindingMismatch);
        }
        let record = ApprovalRecord {
            approval_id: approval.approval_id.clone(),
            request_id: request.request_id.clone(),
            actor_id: request.actor.actor_id.clone(),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            target: request.target.clone(),
            target_identity_digest: None,
            runtime_fence: None,
            operation_digest: request.operation_digest.clone(),
            input_digest: request.input_digest.clone(),
            permission: Some(permission.clone()),
            state: ApprovalState::Pending,
            revision: 1,
            expires_at_ms: approval.expires_at_ms.min(request.expires_at_ms),
            decided_by_actor_id: None,
            created_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
        };
        Ok(ledger_value(self.store.create_provider_approval(
            command, approval, &record, binding, lease,
        )?))
    }

    /// Resolves provider-native approval evidence and prepares one response.
    ///
    /// Unlike [`Self::resolve_once`], this method never creates an execution
    /// permit. The provider remains the side-effect authority, and dispatch is
    /// separately moved through its non-replayable started boundary.
    pub fn resolve_provider_native_once(
        &mut self,
        request: &CapabilityRequest,
        approval: &ApprovalRequest,
        resolution: DurableProviderApprovalResolution<'_>,
    ) -> Result<ProviderPermissionDispatchRecord, DurableApprovalError> {
        validate_approval_binding(request, approval)?;
        if resolution.permission.request_id != request.request_id
            || resolution.permission.run_id != request.run_id
        {
            return Err(DurableApprovalError::BindingMismatch);
        }
        Ok(ledger_value(self.store.resolve_provider_permission(
            resolution.command,
            &approval.approval_id,
            resolution.expected_revision,
            ApprovalResolution::Decide(resolution.decision),
            resolution.permission,
            resolution.lease,
        )?))
    }

    /// Resolves an approval and creates executable authority only after approval.
    ///
    /// The approval and permit are separate idempotent ledger commands. A crash
    /// between them leaves a safe approved state with no executable permit; the
    /// same issuance command can resume without repeating the human decision.
    ///
    /// # Errors
    ///
    /// Returns for changed bindings, stale policy authority, invalid revisions,
    /// or a ledger conflict. A denial never issues a permit.
    pub fn resolve_once(
        &mut self,
        request: &CapabilityRequest,
        approval: &ApprovalRequest,
        resolution: DurableApprovalResolution<'_>,
    ) -> Result<DurableApprovalOutcome, DurableApprovalError> {
        validate_approval_binding(request, approval)?;
        let resolved = ledger_value(self.store.resolve_approval(
            resolution.resolution_command,
            &approval.approval_id,
            resolution.expected_revision,
            ApprovalResolution::Decide(resolution.decision),
        )?);
        if resolution.decision == ApprovalDecision::Deny
            || resolved.state != ApprovalState::Approved
        {
            return Ok(DurableApprovalOutcome::NotPermitted(Box::new(resolved)));
        }
        if resolution.policy_revision == 0 {
            return Err(DurableApprovalError::InvalidPolicyRevision);
        }
        let valid_until_ms = resolution
            .policy_valid_until_ms
            .min(request.expires_at_ms)
            .min(resolved.expires_at_ms);
        if valid_until_ms <= resolution.permit_command.committed_at_ms {
            return Err(DurableApprovalError::PolicyAuthorityExpired);
        }
        let permit = ExecutionPermit {
            permit_id: resolution.permit_id,
            request_id: request.request_id.clone(),
            actor_id: request.actor.actor_id.clone(),
            approval_id: Some(approval.approval_id.clone()),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            execution_id: resolution.execution_id,
            target: request.target.clone(),
            target_identity_digest: resolved
                .target_identity_digest
                .clone()
                .ok_or(DurableApprovalError::BindingMismatch)?,
            runtime_fence: resolved
                .runtime_fence
                .clone()
                .ok_or(DurableApprovalError::BindingMismatch)?,
            operation_digest: request.operation_digest.clone(),
            input_digest: request.input_digest.clone(),
            policy_revision: resolution.policy_revision,
            valid_until_ms,
            single_use: true,
        };
        Ok(DurableApprovalOutcome::Permit(Box::new(ledger_value(
            self.store
                .issue_permit(resolution.permit_command, &permit)?,
        ))))
    }
}

fn validate_approval_binding(
    request: &CapabilityRequest,
    approval: &ApprovalRequest,
) -> Result<(), DurableApprovalError> {
    if approval.request_id != request.request_id
        || approval.task_id != request.task_id
        || approval.run_id != request.run_id
        || approval.expires_at_ms > request.expires_at_ms
    {
        return Err(DurableApprovalError::BindingMismatch);
    }
    Ok(())
}

fn ledger_value<T>(outcome: LedgerOutcome<T>) -> T {
    match outcome {
        LedgerOutcome::Applied(value) | LedgerOutcome::Replayed(value) => value,
    }
}

#[cfg(test)]
mod tests;
