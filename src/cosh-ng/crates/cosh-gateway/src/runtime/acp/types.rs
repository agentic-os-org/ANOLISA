//! Runtime-local ACP v1 projections that keep SDK types out of domain contracts.

use serde_json::Value;
use thiserror::Error;

use cosh_gateway_contracts::common::Digest;

/// Stable ACP wire version negotiated by the first COSH bridge profile.
pub const ACP_WIRE_PROTOCOL_VERSION: u16 = 1;

/// Adapter-specific protocol extensions enabled for one ACP connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AcpV1AdapterProfile {
    /// Use only stable ACP v1 fields.
    #[default]
    Generic,
    /// Use the schema frozen to `@agentclientprotocol/codex-acp` 1.6.2.
    Codex162,
}

/// Configuration for one ACP v1 codec instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpV1ClientConfig {
    /// Programmatic client implementation name advertised to the Agent.
    pub name: String,
    /// Client implementation version, independent from the ACP wire version.
    pub version: String,
    /// Maximum accepted or emitted JSON-RPC frame size.
    pub max_frame_bytes: usize,
    /// Explicit adapter extension profile; executable names never select it.
    pub adapter_profile: AcpV1AdapterProfile,
}

impl AcpV1ClientConfig {
    /// Builds a client configuration with an explicit frame bound.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        max_frame_bytes: usize,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            max_frame_bytes,
            adapter_profile: AcpV1AdapterProfile::Generic,
        }
    }

    /// Enables extensions frozen to the selected trusted adapter profile.
    #[must_use]
    pub fn adapter_profile(mut self, adapter_profile: AcpV1AdapterProfile) -> Self {
        self.adapter_profile = adapter_profile;
        self
    }
}

/// Negotiation and terminal state for one ACP process generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpV1ProtocolPhase {
    /// No ACP frame has been sent.
    Created,
    /// The initialize response is outstanding.
    AwaitingInitialize,
    /// ACP v1 negotiation succeeded.
    Ready,
    /// The wire became unusable and no more traffic is accepted.
    Terminal,
}

/// JSON-RPC request identity scoped to one ACP connection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AcpV1RequestId {
    /// Integer request identifier.
    Number(i64),
    /// String request identifier.
    String(String),
}

impl std::fmt::Display for AcpV1RequestId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
        }
    }
}

/// Outbound request operation used to classify correlated responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpV1RequestKind {
    /// Connection initialization.
    Initialize,
    /// New Agent session creation.
    NewSession,
    /// One prompt turn.
    Prompt,
}

/// Agent implementation metadata copied out of the ACP SDK type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpV1AgentInfo {
    /// Programmatic implementation name.
    pub name: String,
    /// Optional human-readable title.
    pub title: Option<String>,
    /// Agent implementation version.
    pub version: String,
}

/// Immutable subset of stable ACP v1 capabilities needed by later bridge phases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AcpV1AgentCapabilities {
    /// Agent supports `session/load`.
    pub load_session: bool,
    /// Agent supports `session/list`.
    pub list_sessions: bool,
    /// Agent supports `session/delete`.
    pub delete_session: bool,
    /// Agent accepts additional workspace roots.
    pub additional_directories: bool,
    /// Agent supports `session/resume`.
    pub resume_session: bool,
    /// Agent supports `session/close`.
    pub close_session: bool,
    /// Agent accepts image prompt blocks.
    pub image_prompts: bool,
    /// Agent accepts audio prompt blocks.
    pub audio_prompts: bool,
    /// Agent accepts embedded resource prompt blocks.
    pub embedded_context: bool,
}

/// Normalized ACP prompt stop reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpV1StopReason {
    /// Agent completed the turn normally.
    EndTurn,
    /// Agent reached its token limit.
    MaxTokens,
    /// Agent reached its request limit for the turn.
    MaxTurnRequests,
    /// Agent refused the prompt.
    Refusal,
    /// Agent acknowledged client cancellation.
    Cancelled,
    /// SDK added a stable value that this bridge version does not yet map.
    Unsupported,
}

/// Display classification for an Agent-provided permission option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpV1PermissionOptionKind {
    /// Permit only the current operation.
    AllowOnce,
    /// Request a durable allow choice; COSH policy may still narrow it.
    AllowAlways,
    /// Reject only the current operation.
    RejectOnce,
    /// Request a durable rejection choice.
    RejectAlways,
    /// SDK added an option kind that this bridge version does not yet map.
    Unsupported,
}

/// One untrusted option supplied by an ACP Agent for user presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpV1PermissionOption {
    /// Opaque Agent option identity.
    pub option_id: String,
    /// Untrusted human-readable option label.
    pub name: String,
    /// Presentation hint; this is never an authorization decision by itself.
    pub kind: AcpV1PermissionOptionKind,
}

/// Validated permission callback awaiting the COSH governance path.
#[derive(Debug, Clone, PartialEq)]
pub struct AcpV1PermissionRequest {
    /// Agent-owned JSON-RPC correlation identifier.
    pub request_id: AcpV1RequestId,
    /// Opaque ACP session identity bound by this codec.
    pub session_id: String,
    /// Validated ACP tool call payload retained for later policy normalization.
    pub tool_call: Value,
    /// Untrusted Agent-provided choices.
    pub options: Vec<AcpV1PermissionOption>,
    /// Digest of the complete JSON-RPC callback carrier, including extensions.
    pub callback_payload_digest: Digest,
}

/// Decision sent back after the COSH governance path resolves a permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpV1PermissionDecision {
    /// The prompt or permission interaction was cancelled.
    Cancelled,
    /// Select one option that appeared in the correlated Agent request.
    Selected {
        /// Opaque Agent option identity.
        option_id: String,
    },
}

/// One validated observation from an ACP v1 Agent.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpV1Observation {
    /// Exact wire-version negotiation succeeded.
    Initialized {
        /// Optional Agent implementation metadata.
        agent_info: Option<AcpV1AgentInfo>,
        /// Immutable stable capability snapshot.
        capabilities: AcpV1AgentCapabilities,
    },
    /// A new opaque ACP session was created.
    SessionOpened {
        /// Agent-owned session identifier; it is not a COSH Task or Run ID.
        session_id: String,
    },
    /// A stable `session/update` payload validated by official ACP v1 types.
    SessionUpdate {
        /// Bound Agent session identifier.
        session_id: String,
        /// Validated update serialized into a runtime-local neutral value.
        update: Value,
    },
    /// Agent requests a permission decision during a prompt.
    PermissionRequested(AcpV1PermissionRequest),
    /// An Agent request outside the narrow first client profile was rejected.
    ///
    /// The session actor has already sent method-not-found before publishing
    /// this diagnostic observation; consumers must not answer it again.
    UnsupportedClientRequest {
        /// Request identifier that received the fail-closed response.
        request_id: AcpV1RequestId,
        /// Unrecognized or unadvertised method name.
        method: String,
    },
    /// An extension or unsupported notification was ignored diagnostically.
    UnsupportedNotification {
        /// Unrecognized notification method.
        method: String,
    },
    /// One prompt request reached its ACP terminal response.
    PromptFinished {
        /// Bound Agent session identifier.
        session_id: String,
        /// Normalized stable stop reason.
        stop_reason: AcpV1StopReason,
    },
    /// A cancelled prompt abandoned unresolved permission callbacks.
    ///
    /// Raw request identifiers remain connection-local and are consumed by
    /// the Runtime port to recover COSH-owned request identities.
    PromptCancelledWithPendingPermissions {
        /// Bound Agent session identifier.
        session_id: String,
        /// Provider request identities abandoned by the terminal response.
        request_ids: Vec<AcpV1RequestId>,
    },
    /// Agent returned a JSON-RPC error for a correlated COSH request.
    RequestFailed {
        /// Operation that failed.
        request: AcpV1RequestKind,
        /// Numeric JSON-RPC or ACP error code.
        code: i32,
        /// Agent-provided diagnostic message.
        message: String,
    },
    /// Runtime stdout closed before the bridge was explicitly shut down.
    TransportClosed,
}

/// ACP v1 codec validation or state failure.
#[derive(Debug, Error)]
pub enum AcpV1CodecError {
    /// Frame limit must fit the bridge safety envelope.
    #[error("invalid ACP frame limit {actual}; expected 1..={maximum}")]
    InvalidFrameLimit {
        /// Rejected frame limit.
        actual: usize,
        /// Hard safety ceiling.
        maximum: usize,
    },
    /// Client implementation metadata must be non-empty.
    #[error("ACP client {field} must not be empty")]
    InvalidClientInfo {
        /// Invalid metadata field.
        field: &'static str,
    },
    /// Operation is invalid in the current protocol phase.
    #[error("ACP operation {operation} is invalid while phase is {phase:?}")]
    InvalidPhase {
        /// Requested codec operation.
        operation: &'static str,
        /// Current protocol phase.
        phase: AcpV1ProtocolPhase,
    },
    /// Frame was empty after newline removal.
    #[error("ACP frame must not be empty")]
    EmptyFrame,
    /// Frame exceeded the configured hard bound.
    #[error("ACP frame exceeds {limit} bytes")]
    FrameTooLarge {
        /// Configured maximum frame bytes.
        limit: usize,
    },
    /// Frame was not valid UTF-8.
    #[error("ACP frame is not valid UTF-8")]
    InvalidUtf8,
    /// A low-level single-message decoder cannot consume a multi-message frame.
    #[error("ACP multi-message frame requires the batch-aware runtime bridge")]
    MultiMessageFrameRequiresBridge,
    /// One frame exceeded the bounded number of independently dispatched entries.
    #[error("ACP batch exceeds {limit} entries")]
    BatchTooLarge {
        /// Maximum entries accepted in one JSON-RPC batch.
        limit: usize,
    },
    /// Official SDK JSON parsing or serialization failed.
    #[error("invalid ACP JSON-RPC frame: {0}")]
    Json(#[from] serde_json::Error),
    /// Official SDK rejected construction of a typed JSON-RPC message.
    #[error("ACP SDK rejected JSON-RPC message: {0}")]
    Sdk(String),
    /// Agent selected a wire version the bridge does not implement.
    #[error("ACP Agent selected unsupported protocol version {actual}; expected 1")]
    UnsupportedProtocolVersion {
        /// Agent-selected numeric protocol version.
        actual: u16,
    },
    /// A response did not match any outstanding client request.
    #[error("ACP response references unknown request id {0}")]
    UnknownResponse(AcpV1RequestId),
    /// JSON-RPC null cannot safely correlate bidirectional callbacks.
    #[error("ACP request id must not be null")]
    NullRequestId,
    /// Workspace roots must be absolute before reaching the Agent.
    #[error("ACP workspace path must be absolute: {0}")]
    WorkspaceNotAbsolute(std::path::PathBuf),
    /// Only one Agent session is supported by this first codec profile.
    #[error("ACP session is already bound to this codec")]
    SessionAlreadyBound,
    /// An operation needs a successfully opened Agent session.
    #[error("ACP operation requires an open session")]
    SessionNotOpen,
    /// Agent referenced a session other than the bound session.
    #[error("ACP session mismatch: expected {expected:?}, received {actual:?}")]
    SessionMismatch {
        /// Bound opaque session identity.
        expected: String,
        /// Received opaque session identity.
        actual: String,
    },
    /// A prompt is already active.
    #[error("ACP prompt is already active")]
    PromptAlreadyActive,
    /// Cancellation or permission callbacks require an active prompt.
    #[error("ACP prompt is not active")]
    PromptNotActive,
    /// Prompt text must not be empty.
    #[error("ACP prompt text must not be empty")]
    EmptyPrompt,
    /// Optional method or field was used without Agent advertisement.
    #[error("ACP Agent did not advertise capability {0}")]
    UnsupportedCapability(&'static str),
    /// A second cancellation was attempted before the prompt settled.
    #[error("ACP cancellation was already sent for the active prompt")]
    CancellationAlreadySent,
    /// Agent reused an outstanding callback identity.
    #[error("ACP Agent reused pending request id {0}")]
    DuplicateInboundRequest(AcpV1RequestId),
    /// Agent exceeded the bounded callback queue.
    #[error("ACP Agent has too many pending client requests; maximum is {limit}")]
    TooManyPendingClientRequests {
        /// Hard limit for one connection.
        limit: usize,
    },
    /// Permission callback had no selectable options.
    #[error("ACP permission request must provide at least one option")]
    EmptyPermissionOptions,
    /// Permission callback reused an option identity.
    #[error("ACP permission request contains duplicate option id {0:?}")]
    DuplicatePermissionOption(String),
    /// Permission response did not correlate to a pending callback.
    #[error("ACP permission request {0} is not pending")]
    UnknownPermissionRequest(AcpV1RequestId),
    /// Selected permission option did not appear in the correlated request.
    #[error("ACP permission option {option_id:?} was not offered for request {request_id}")]
    UnknownPermissionOption {
        /// Correlated Agent request.
        request_id: AcpV1RequestId,
        /// Rejected option identity.
        option_id: String,
    },
    /// The Agent offered an option outside the MVP once-only boundary.
    #[error("ACP permission option {option_id:?} for request {request_id} is not once-only")]
    UnsupportedPermissionOption {
        /// Correlated Agent request.
        request_id: AcpV1RequestId,
        /// Rejected option identity.
        option_id: String,
    },
    /// Unsupported callback rejection did not correlate to an observed request.
    #[error("ACP unsupported request {0} is not pending")]
    UnknownUnsupportedRequest(AcpV1RequestId),
    /// Outbound request sequence exceeded the supported JSON-RPC range.
    #[error("ACP request id sequence exhausted")]
    RequestIdExhausted,
    /// Inbound batch sequence exceeded the supported correlation range.
    #[error("ACP inbound batch id sequence exhausted")]
    BatchIdExhausted,
    /// A deferred response referenced a batch that is no longer pending.
    #[error("ACP inbound batch {0} is not pending")]
    UnknownInboundBatch(u64),
    /// A deferred response referenced a slot outside its pending batch.
    #[error("ACP inbound batch {batch_id} has no response slot {slot}")]
    UnknownInboundBatchSlot {
        /// Connection-local batch identity.
        batch_id: u64,
        /// Response position within the batch response array.
        slot: usize,
    },
    /// A batch callback attempted to settle the same response slot twice.
    #[error("ACP inbound batch {batch_id} response slot {slot} is already settled")]
    InboundBatchSlotAlreadySettled {
        /// Connection-local batch identity.
        batch_id: u64,
        /// Response position within the batch response array.
        slot: usize,
    },
    /// Prompt settled while callbacks still required a response.
    #[error("ACP prompt finished with {count} pending permission requests")]
    PromptFinishedWithPendingPermissions {
        /// Number of unsettled permission callbacks.
        count: usize,
    },
    /// Prompt settled while unsupported callbacks still required rejection.
    #[error("ACP prompt finished with {count} pending unsupported requests")]
    PromptFinishedWithPendingUnsupported {
        /// Number of callbacks still awaiting method-not-found.
        count: usize,
    },
    /// A Codex profile did not identify the exact adapter schema frozen here.
    #[error(
        "Codex ACP adapter identity mismatch: expected @agentclientprotocol/codex-acp 1.6.2, received {name:?} {version:?}"
    )]
    CodexAdapterIdentityMismatch {
        /// Received package identity, or `None` when `agentInfo` was absent.
        name: Option<String>,
        /// Received package version, or `None` when `agentInfo` was absent.
        version: Option<String>,
    },
    /// The exact Codex adapter did not complete AIR v1 negotiation.
    #[error("invalid Codex AIR sessionFailure negotiation: {0}")]
    InvalidCodexSessionFailureNegotiation(&'static str),
    /// A negotiated Codex session failure record violated the frozen schema.
    #[error("invalid Codex AIR sessionFailure record: {0}")]
    InvalidCodexSessionFailure(&'static str),
}
