//! Adapts one provider-neutral Agent Runtime port to the durable scheduler.

use std::time::{Duration, Instant};

use cosh_gateway_contracts::{
    common::{BoundedText, ContentPart, RuntimeBindingRef, WorkspaceRef},
    error::{ContractError, ErrorCategory},
    ids::{InputRequestId, RuntimeBindingId, TurnId},
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, BrokeredExecutionDelivery, BrokeredExecutionRef,
        BrokeredRequestAcknowledgement, RuntimeEventEnvelope, RuntimeInputRequest,
        RuntimeInputResponse, RuntimePermissionDecision, RuntimePermissionRef, TurnOutcome,
    },
};

use crate::daemon::{
    RuntimeCancellationCause, RuntimeFactory, RuntimeHandle, RuntimePoll, ScheduledRun,
    StartedRuntime,
};
use cosh_gateway_contracts::task::CancelReason;

use super::{AgentRuntimePort, AgentRuntimePortError};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(70);
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(10);

// Factory and handle phases share scheduler adapter invariants in one private
// namespace; the fragments assign file ownership without widening them.
include!("scheduled_adapter/factory.rs");
include!("scheduled_adapter/handle.rs");
include!("scheduled_adapter/runtime_handle.rs");
include!("scheduled_adapter/support.rs");

#[cfg(test)]
#[path = "scheduled_adapter/tests.rs"]
mod tests;
