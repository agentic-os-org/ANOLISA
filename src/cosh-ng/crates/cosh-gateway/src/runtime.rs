//! Supervised child-process primitives and private runtime protocol codecs.
//!
//! This module deliberately stops at the process/protocol boundary. Public
//! Task, Run, Agent, and Runtime identities and events belong to
//! `cosh-gateway-contracts` and are mapped by a higher-level bridge.

mod acp;
mod acp_port;
mod bounded_io;
mod containment;
mod cosh_core_bridge;
mod cosh_core_jsonl;
mod installed_acp_factory;
mod installed_core_factory;
mod pinned_local;
mod port;
mod process_group;
mod profile;
mod scheduled_adapter;
mod session_driver;
mod supervisor;

pub use acp::{
    AcpToolAccumulation, AcpToolAccumulatorError, AcpToolAccumulatorLimits,
    AcpToolInvocationSnapshot, AcpV1AdapterProfile, AcpV1AgentCapabilities, AcpV1AgentInfo,
    AcpV1BridgeError, AcpV1BridgeRead, AcpV1ClientConfig, AcpV1Codec, AcpV1CodecError,
    AcpV1Observation, AcpV1PermissionDecision, AcpV1PermissionOption, AcpV1PermissionOptionKind,
    AcpV1PermissionRequest, AcpV1ProtocolPhase, AcpV1RequestId, AcpV1RequestKind,
    AcpV1RuntimeBridge, AcpV1StopReason, ToolInvocationAccumulator, ACP_WIRE_PROTOCOL_VERSION,
};
pub use acp_port::{
    AcpAgentRuntime, AcpAgentRuntimeConfig, AcpAgentRuntimeIdentity, AcpPermissionContext,
    AcpPermissionNormalizer,
};
pub use bounded_io::{BoundedLineError, BoundedLineReader, StderrSnapshot};
pub use containment::{
    LinuxSystemdContainmentVerifier, RuntimeContainmentError, VerifiedRuntimeContainment,
};
pub use cosh_core_bridge::{
    CoshCoreBridge, CoshCoreBridgeConfig, CoshCoreBridgeIdentity, CoshCoreBrokeredContext,
};
pub use cosh_core_jsonl::{
    CoshCoreAskUserOption, CoshCoreAssistantBody, CoshCoreAssistantMessage, CoshCoreCapabilities,
    CoshCoreCodecError, CoshCoreContentBlock, CoshCoreContentBlockInfo, CoshCoreContentDelta,
    CoshCoreControlRequest, CoshCoreControlRequestEnvelope, CoshCoreControlResponse,
    CoshCoreExecutionProfile, CoshCoreJsonlCodec, CoshCoreObservation, CoshCoreProtocolPhase,
    CoshCoreResult, CoshCoreShellContext, CoshCoreStreamEvent, CoshCoreSystemMessage,
    CoshCoreToolResult, CoshCoreUserTurn, BROKERED_COSH_CONTROL_PROTOCOL_VERSION,
    BROKERED_WORKSPACE_WRITE_COSH_CONTROL_PROTOCOL_VERSION, GATEWAY_BROKERED_EXECUTION_PROFILE,
    GATEWAY_BROKERED_WORKSPACE_WRITE_EXECUTION_PROFILE, PRIVATE_COSH_CONTROL_PROTOCOL_VERSION,
};
pub use installed_acp_factory::{
    InstalledAcpRuntimePortFactory, LocalOsActorResolver, ResolvedWorkspace,
    TrustedWorkspaceResolver,
};
pub use installed_core_factory::{
    InstalledBrokeredCoreRuntimePortFactory, ResolvedBrokeredCoreRuntimeProfile,
    GATEWAY_BROKERED_CORE_RUNTIME_PROFILE, GATEWAY_CHECKPOINT_CORE_RUNTIME_PROFILE,
    GATEWAY_WORKSPACE_WRITE_CORE_RUNTIME_PROFILE,
};
pub use pinned_local::{PinnedDirectory, PinnedExecutable, PinnedFileIdentity};
pub use port::{AgentRuntimePort, AgentRuntimePortError};
pub use process_group::{PlatformProcessGroup, ProcessGroupLifecycle};
pub use profile::{
    built_in_acp_runtime_profiles, AcpRuntimeProfile, AcpRuntimeProfileId,
    AcpRuntimeProfileLaunchError, AcpRuntimeProfileRequest, AcpRuntimeProfileResolveError,
    AcpRuntimeProfileResolver, ResolvedAcpRuntimeProfile,
};
pub use scheduled_adapter::{
    AgentRuntimePortFactory, ScheduledAgentRuntimeFactory, ScheduledRuntimePort,
};
pub use session_driver::{
    AcpSessionControl, AcpSessionDriver, AcpSessionDriverConfig, AcpSessionDriverError,
    AcpSessionEvent, AcpSessionObservation, AcpSessionTerminal, AcpSessionTerminalKind,
};
pub use supervisor::{
    ProcessExit, ProcessTerminal, RuntimeFrameRead, RuntimeLaunchError, RuntimeLaunchSpec,
    RuntimeState, RuntimeSupervisor, RuntimeSupervisorError,
};
