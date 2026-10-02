//! Provider-neutral Runtime port backed by one supervised ACP v1 session.
//!
//! Lifecycle, mapping, and port ownership are split into cohesive fragments
//! while retaining one private state-machine namespace.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cosh_gateway_contracts::{
    capability::CapabilityRequest,
    common::{
        ActorRef, BoundedName, BoundedOpaque, BoundedText, ContentPart, ContractHeader,
        ContractSchema, Correlation, Digest, RuntimeBindingRef, WorkspaceRef, MAX_TEXT_BYTES,
    },
    error::{ContractError, ErrorCategory},
    external::{ExternalRef, ExternalRefKind},
    ids::{
        AgentSessionId, InstallationId, MessageId, RequestId, RunId, RuntimeBindingId,
        RuntimeInstanceId, RuntimeMessageId, TaskId, TurnId,
    },
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, ProviderPermissionCallbackV2, RuntimeEventEnvelope,
        RuntimePermissionDecision, TurnLimit, TurnOutcome,
    },
};

use super::acp::DEFAULT_MAX_TOOL_IDENTIFIER_BYTES;
use super::{
    AcpSessionDriver, AcpSessionDriverConfig, AcpSessionDriverError, AcpSessionEvent,
    AcpSessionTerminalKind, AcpToolAccumulation, AcpV1AdapterProfile, AcpV1Observation,
    AcpV1PermissionDecision, AcpV1PermissionOptionKind, AcpV1PermissionRequest, AcpV1RequestId,
    AcpV1StopReason, AgentRuntimePort, AgentRuntimePortError, ToolInvocationAccumulator,
};
use sha2::{Digest as ShaDigest, Sha256};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(20);

// A shared namespace avoids widening transport state solely for file layout.
include!("acp_port/model.rs");
include!("acp_port/permission.rs");
include!("acp_port/runtime.rs");
include!("acp_port/port.rs");
include!("acp_port/support.rs");

#[cfg(test)]
#[path = "acp_port/tests.rs"]
mod tests;
