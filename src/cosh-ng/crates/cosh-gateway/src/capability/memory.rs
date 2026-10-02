//! In-memory permit ledger for deterministic local coordination and tests.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use cosh_gateway_contracts::{
    capability::{CapabilityDecision, CapabilityRequest, ExecutionPermit},
    ids::{PermitId, RequestId},
};

use super::{PermitExpectation, PermitStore, PermitStoreError};

#[derive(Debug, Clone)]
struct StoredPermit {
    permit: ExecutionPermit,
    consumed: bool,
}

#[derive(Debug, Clone)]
struct StoredDecision {
    request: CapabilityRequest,
    decision: CapabilityDecision,
}

/// Process-local permit store with mutex-atomic validation and consumption.
#[derive(Debug, Clone, Default)]
pub struct MemoryPermitStore {
    ledger: Arc<Mutex<PermitLedger>>,
}

#[derive(Debug, Default)]
struct PermitLedger {
    permits: HashMap<PermitId, StoredPermit>,
    requests: HashMap<RequestId, StoredDecision>,
}

impl PermitStore for MemoryPermitStore {
    fn replay(
        &self,
        request: &CapabilityRequest,
    ) -> Result<Option<CapabilityDecision>, PermitStoreError> {
        let ledger = self
            .ledger
            .lock()
            .map_err(|_| PermitStoreError::Unavailable)?;
        let Some(stored) = ledger.requests.get(&request.request_id) else {
            return Ok(None);
        };
        if stored.request != *request {
            return Err(PermitStoreError::RequestConflict);
        }
        Ok(Some(stored.decision.clone()))
    }

    fn issue_or_replay(
        &self,
        request: &CapabilityRequest,
        decision: CapabilityDecision,
    ) -> Result<CapabilityDecision, PermitStoreError> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| PermitStoreError::Unavailable)?;
        if let Some(stored) = ledger.requests.get(&request.request_id) {
            if stored.request != *request {
                return Err(PermitStoreError::RequestConflict);
            }
            return Ok(stored.decision.clone());
        }
        if let CapabilityDecision::Permit { permit } = &decision {
            if ledger.permits.contains_key(&permit.permit_id) {
                return Err(PermitStoreError::AlreadyExists);
            }
            ledger.permits.insert(
                permit.permit_id.clone(),
                StoredPermit {
                    permit: permit.clone(),
                    consumed: false,
                },
            );
        }
        ledger.requests.insert(
            request.request_id.clone(),
            StoredDecision {
                request: request.clone(),
                decision: decision.clone(),
            },
        );
        Ok(decision)
    }

    fn consume(
        &self,
        expectation: &PermitExpectation,
    ) -> Result<ExecutionPermit, PermitStoreError> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| PermitStoreError::Unavailable)?;
        let stored = ledger
            .permits
            .get_mut(&expectation.permit_id)
            .ok_or(PermitStoreError::NotFound)?;

        // Validate under the same lock as the state transition so a mismatch
        // cannot consume authority and two correct callers cannot both win.
        validate_expectation(&stored.permit, expectation)?;
        if stored.consumed {
            return Err(PermitStoreError::AlreadyConsumed);
        }
        stored.consumed = true;
        Ok(stored.permit.clone())
    }
}

fn validate_expectation(
    permit: &ExecutionPermit,
    expectation: &PermitExpectation,
) -> Result<(), PermitStoreError> {
    if !permit.single_use {
        return Err(PermitStoreError::NotSingleUse);
    }
    if permit.valid_until_ms <= expectation.now_ms {
        return Err(PermitStoreError::Expired);
    }
    if permit.actor_id != expectation.actor_id {
        return Err(PermitStoreError::ActorMismatch);
    }
    if permit.task_id != expectation.task_id {
        return Err(PermitStoreError::TaskMismatch);
    }
    if permit.run_id != expectation.run_id {
        return Err(PermitStoreError::RunMismatch);
    }
    if permit.execution_id != expectation.execution_id {
        return Err(PermitStoreError::ExecutionMismatch);
    }
    if permit.target != expectation.target {
        return Err(PermitStoreError::TargetMismatch);
    }
    if permit.target_identity_digest != expectation.target_identity_digest {
        return Err(PermitStoreError::TargetIdentityMismatch);
    }
    if permit.runtime_fence != expectation.runtime_fence {
        return Err(PermitStoreError::RuntimeFenceMismatch);
    }
    if permit.operation_digest != expectation.operation_digest {
        return Err(PermitStoreError::OperationMismatch);
    }
    if permit.input_digest != expectation.input_digest {
        return Err(PermitStoreError::InputMismatch);
    }
    if permit.policy_revision != expectation.policy_revision {
        return Err(PermitStoreError::PolicyRevisionMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
