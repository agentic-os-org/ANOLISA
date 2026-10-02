//! Durable approval, permit, execution, runtime binding, and run lease ledger.

use cosh_gateway_contracts::capability::{
    ApprovalDecision, ApprovalRequest, BrokeredOperation, CapabilityRequest, DenialCode,
    ExecutionPermit, RuntimeExecutionFence,
};
use cosh_gateway_contracts::common::RuntimeBindingRef;
use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, BoundedText, Digest, IdempotencyKey, TargetRef,
};
use cosh_gateway_contracts::error::{ContractError, ErrorCategory};
use cosh_gateway_contracts::ids::{
    ActorId, ApprovalId, ExecutionId, InputRequestId, PermitId, RequestId, RunId, RuntimeBindingId,
    RuntimeInstanceId, TaskId,
};
use cosh_gateway_contracts::runtime::{
    BrokeredExecutionDelivery, BrokeredExecutionOutcome, BrokeredExecutionRef,
    BrokeredOperationResult, RuntimeInputRequest, RuntimeInputResponse, RuntimePermissionRef,
};
use cosh_gateway_contracts::task::{
    ApprovalAbandonCause, ExecutionOutcome, TaskEvent, TaskState, UncertaintyCode,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::task::TaskAggregate;

use super::{
    task_store::{append_internal_task_event, load_verified_projection},
    SqliteTaskStore, StoreError,
};

const MAX_RUNTIME_INPUT_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RUNTIME_INPUT_RESPONSE_BYTES: usize = 64 * 1024;

// Ledger domains share transaction and corruption-checking internals. Keep one
// private namespace while assigning each durable record family its own file.
include!("ledger/types.rs");
include!("ledger/runtime_input.rs");
include!("ledger/brokered.rs");
include!("ledger/records.rs");
include!("ledger/approval.rs");
include!("ledger/permit.rs");
include!("ledger/execution.rs");
include!("ledger/runtime_binding.rs");
include!("ledger/run_lease.rs");
include!("ledger/recovery.rs");
include!("ledger/runtime_input_helpers.rs");
include!("ledger/brokered_helpers.rs");
include!("ledger/authority_helpers.rs");
include!("ledger/receipt_helpers.rs");
include!("ledger/record_loaders.rs");
include!("ledger/execution_loaders.rs");
include!("ledger/runtime_loaders.rs");
include!("ledger/value_helpers.rs");

fn immediate(store: &mut SqliteTaskStore) -> Result<Transaction<'_>, StoreError> {
    Ok(store
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?)
}

#[cfg(test)]
mod tests;
