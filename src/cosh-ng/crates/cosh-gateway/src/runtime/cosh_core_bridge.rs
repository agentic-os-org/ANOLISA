//! Gateway-owned bridge from private cosh-core JSONL to neutral Runtime events.
//!
//! Lifecycle, observation mapping, and control are split into cohesive
//! fragments while retaining one private bridge namespace.

use std::collections::{btree_map::Entry, BTreeMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cosh_gateway_contracts::{
    capability::{
        BrokeredOperation, CapabilityRequest, CapabilityScope, OperationDescriptor,
        WorkspaceCheckpointCreateV1,
    },
    common::{
        ActorRef, BoundedName, BoundedOpaque, BoundedText, ContentPart, ContractHeader,
        ContractSchema, Correlation, Digest, RuntimeBindingRef, TargetRef, WorkspaceRef,
    },
    error::{ContractError, ErrorCategory},
    external::{ExternalRef, ExternalRefKind},
    ids::{
        ActorId, AgentSessionId, CheckpointId, InputRequestId, InstallationId, MessageId,
        RequestId, RunId, RuntimeBindingId, RuntimeInstanceId, RuntimeMessageId, TaskId, ToolUseId,
        TurnId,
    },
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, BrokeredExecutionDelivery,
        BrokeredExecutionOutcome, BrokeredOperationResult, CorePermissionCallbackV1,
        ExecutionAuthority, RuntimeEventEnvelope, RuntimeInputOption, RuntimeInputRequest,
        RuntimeInputResponse, RuntimePermissionDecision, ToolInvocationSnapshot,
        ToolInvocationStatus, ToolSummary, TurnOutcome,
    },
};
use sha2::{Digest as _, Sha256};

use super::{
    AgentRuntimePort, AgentRuntimePortError, CoshCoreContentBlockInfo, CoshCoreContentDelta,
    CoshCoreControlRequest, CoshCoreExecutionProfile, CoshCoreJsonlCodec, CoshCoreObservation,
    CoshCoreStreamEvent, CoshCoreUserTurn, RuntimeFrameRead, RuntimeLaunchSpec, RuntimeState,
    RuntimeSupervisor,
};

const READ_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_TOOL_USES_PER_TURN: usize = 1024;

// A shared namespace keeps bridge sequencing helpers private across phases.
include!("cosh_core_bridge/model.rs");
include!("cosh_core_bridge/lifecycle.rs");
include!("cosh_core_bridge/observation.rs");
include!("cosh_core_bridge/control.rs");
include!("cosh_core_bridge/port.rs");
include!("cosh_core_bridge/support.rs");

#[cfg(test)]
mod tests;
