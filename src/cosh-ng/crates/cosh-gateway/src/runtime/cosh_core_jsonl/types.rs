//! Runtime-local types for the private cosh-core JSONL wire contract.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Exact private Shell/Core control protocol version implemented by cosh-core.
pub const PRIVATE_COSH_CONTROL_PROTOCOL_VERSION: u32 = 1;
/// Exact private protocol version for the Gateway-brokered Core profile.
pub const BROKERED_COSH_CONTROL_PROTOCOL_VERSION: u32 = 3;
/// Exact private protocol version for the Gateway checkpoint profile.
pub const BROKERED_CHECKPOINT_COSH_CONTROL_PROTOCOL_VERSION: u32 = 4;
/// Exact private protocol version for the Gateway workspace-write profile.
pub const BROKERED_WORKSPACE_WRITE_COSH_CONTROL_PROTOCOL_VERSION: u32 = 5;
/// Exact launch and acknowledgement name for the brokered Core profile.
pub const GATEWAY_BROKERED_EXECUTION_PROFILE: &str = "gateway_brokered_v1";
/// Exact launch and acknowledgement name for the brokered checkpoint profile.
pub const GATEWAY_BROKERED_CHECKPOINT_EXECUTION_PROFILE: &str = "gateway_brokered_checkpoint_v1";
/// Exact launch and acknowledgement name for the brokered workspace-write profile.
pub const GATEWAY_BROKERED_WORKSPACE_WRITE_EXECUTION_PROFILE: &str =
    "gateway_brokered_workspace_write_v1";

/// Private Core execution boundary selected before process launch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CoshCoreExecutionProfile {
    /// Existing Core behavior using private protocol v1.
    #[default]
    Legacy,
    /// Gateway owns the task-only brokered profile using private protocol v3.
    GatewayBrokeredV1,
    /// Gateway owns the closed checkpoint profile using private protocol v4.
    GatewayBrokeredCheckpointV1,
    /// Gateway owns the approval-gated workspace-write profile using private protocol v5.
    GatewayBrokeredWorkspaceWriteV1,
}

impl CoshCoreExecutionProfile {
    pub(super) const fn protocol_version(self) -> u32 {
        match self {
            Self::Legacy => PRIVATE_COSH_CONTROL_PROTOCOL_VERSION,
            Self::GatewayBrokeredV1 => BROKERED_COSH_CONTROL_PROTOCOL_VERSION,
            Self::GatewayBrokeredCheckpointV1 => BROKERED_CHECKPOINT_COSH_CONTROL_PROTOCOL_VERSION,
            Self::GatewayBrokeredWorkspaceWriteV1 => {
                BROKERED_WORKSPACE_WRITE_COSH_CONTROL_PROTOCOL_VERSION
            }
        }
    }

    pub(super) const fn wire_name(self) -> Option<&'static str> {
        match self {
            Self::Legacy => None,
            Self::GatewayBrokeredV1 => Some(GATEWAY_BROKERED_EXECUTION_PROFILE),
            Self::GatewayBrokeredCheckpointV1 => {
                Some(GATEWAY_BROKERED_CHECKPOINT_EXECUTION_PROFILE)
            }
            Self::GatewayBrokeredWorkspaceWriteV1 => {
                Some(GATEWAY_BROKERED_WORKSPACE_WRITE_EXECUTION_PROFILE)
            }
        }
    }

    pub(super) const fn is_brokered(self) -> bool {
        matches!(
            self,
            Self::GatewayBrokeredV1
                | Self::GatewayBrokeredCheckpointV1
                | Self::GatewayBrokeredWorkspaceWriteV1
        )
    }

    pub(super) const fn hosts_checkpoint(self) -> bool {
        matches!(self, Self::GatewayBrokeredCheckpointV1)
    }

    pub(super) const fn allows_workspace_write(self) -> bool {
        matches!(self, Self::GatewayBrokeredWorkspaceWriteV1)
    }
}

/// Protocol negotiation and terminal state owned by one codec instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoshCoreProtocolPhase {
    /// No initialize request has been encoded.
    Created,
    /// Initialize was sent; only its response or bounded auth bootstrap is valid.
    AwaitingInitialize,
    /// Exact-version negotiation succeeded and turn traffic is admissible.
    Ready,
    /// One result or synthetic EOF terminal was emitted.
    Terminal,
}

/// Private protocol capability snapshot returned by cosh-core initialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct CoshCoreCapabilities {
    /// Core accepts `can_use_tool` control exchanges.
    #[serde(default)]
    pub can_handle_can_use_tool: bool,
    /// Core accepts host-executed Shell tool results.
    #[serde(default)]
    pub can_handle_host_executed_shell_tool_result: bool,
    /// Core accepts bounded Shell evidence responses.
    #[serde(default)]
    pub can_handle_shell_evidence_tool: bool,
    /// Core accepts durable approval-ownership receipts.
    #[serde(default)]
    pub can_handle_approval_receipt: bool,
    /// Core accepts a typed Gateway-hosted checkpoint terminal result.
    ///
    /// The task-only Gateway profile requires this value to remain false and
    /// the checkpoint profile requires it to be true.
    #[serde(default)]
    pub can_handle_hosted_checkpoint_create: bool,
    /// Core accepts a Gateway-resolved, side-effect-free user question.
    #[serde(default)]
    pub can_handle_brokered_ask_user: bool,
}

/// One typed user turn encoded for the private cosh-core transport.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoshCoreUserTurn {
    /// Provider-facing prompt content.
    pub content: String,
    /// Optional provider session binding; not a Gateway Task or Agent identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Original user text retained for current hook compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_user_input: Option<String>,
    /// Optional bounded compatibility context for a brokered profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_context: Option<CoshCoreShellContext>,
}

/// Compatibility context accepted by the current private cosh-core protocol.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoshCoreShellContext {
    /// Pinned runtime workspace.
    pub cwd: PathBuf,
    /// Explicit bounded environment snapshot.
    pub env: std::collections::BTreeMap<String, String>,
    /// Previous governed Shell execution status.
    pub last_exit_code: i32,
}

/// Typed observation produced from one private cosh-core output frame.
///
/// These observations are intentionally runtime-local. A bridge must attach
/// public contract identities, ordering, fences, and causation separately.
#[derive(Debug, Clone, PartialEq)]
pub enum CoshCoreObservation {
    /// Exact-version initialization succeeded.
    Initialized(CoshCoreCapabilities),
    /// Session metadata, status, or hook notification.
    System(CoshCoreSystemMessage),
    /// Ordered provider stream update.
    Stream(CoshCoreStreamEvent),
    /// Completed assistant message.
    Assistant(CoshCoreAssistantMessage),
    /// Completed tool results echoed by the core.
    ToolResults {
        /// Provider session associated with the message.
        provider_session_id: String,
        /// Tool results in wire order.
        results: Vec<CoshCoreToolResult>,
    },
    /// Core-initiated permission, question, auth, or evidence request.
    ControlRequest(CoshCoreControlRequestEnvelope),
    /// Correlated non-initialization management response.
    ControlResponse(CoshCoreControlResponse),
    /// Correlated registry response.
    RegistryResponse {
        /// Caller-provided private request identifier.
        request_id: String,
        /// Whether the registry operation succeeded.
        success: bool,
        /// Bounded response data.
        data: Option<Value>,
        /// Provider-safe failure message.
        error: Option<String>,
    },
    /// Core-emitted turn terminal. Only one is accepted per codec lifecycle.
    Result(CoshCoreResult),
    /// Stdout ended before a core result; the bridge must fail/suspend the Run.
    ProtocolEndedWithoutResult,
}

/// Private cosh-core system output.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CoshCoreSystemMessage {
    /// Private system message subtype such as `init` or `status`.
    pub subtype: String,
    /// Provider session metadata when supplied.
    #[serde(default, rename = "session_id")]
    pub provider_session_id: Option<String>,
    /// Whether the provider session can be resumed.
    #[serde(default)]
    pub session_resumable: Option<bool>,
    /// Provider model name.
    #[serde(default)]
    pub model: Option<String>,
    /// Advertised provider/core tools.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Status or hook notification text.
    #[serde(default)]
    pub status: Option<String>,
    /// Hook name for hook notifications.
    #[serde(default)]
    pub hook_name: Option<String>,
    /// Provider tool-use correlation identifier.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Hook governance decision.
    #[serde(default)]
    pub decision: Option<String>,
}

/// One private cosh-core streaming update.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum CoshCoreStreamEvent {
    /// Starts one provider message.
    #[serde(rename = "message_start")]
    MessageStart,
    /// Starts a content block.
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        /// Provider block index.
        index: u32,
        /// Initial block metadata.
        content_block: CoshCoreContentBlockInfo,
    },
    /// Appends one bounded content delta.
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        /// Provider block index.
        index: u32,
        /// Provider delta payload.
        delta: CoshCoreContentDelta,
    },
    /// Completes a content block.
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        /// Provider block index.
        index: u32,
    },
    /// Completes one provider message.
    #[serde(rename = "message_stop")]
    MessageStop,
}

/// Initial metadata for a private stream content block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum CoshCoreContentBlockInfo {
    /// Assistant text block.
    #[serde(rename = "text")]
    Text,
    /// Provider thinking block.
    #[serde(rename = "thinking")]
    Thinking,
    /// Provider tool-use block.
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Provider tool-use identifier.
        id: String,
        /// Provider tool name.
        name: String,
    },
}

/// Delta payload for a private stream content block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum CoshCoreContentDelta {
    /// Assistant text fragment.
    #[serde(rename = "text_delta")]
    TextDelta {
        /// Text fragment.
        text: String,
    },
    /// Provider thinking fragment.
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        /// Thinking fragment.
        thinking: String,
    },
    /// Partial JSON tool input.
    #[serde(rename = "input_json_delta")]
    InputJsonDelta {
        /// JSON fragment; completeness is established only by block stop.
        partial_json: String,
    },
}

/// Completed assistant output from the private core protocol.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CoshCoreAssistantMessage {
    /// Provider session associated with the message.
    #[serde(rename = "session_id")]
    pub provider_session_id: String,
    /// Completed content blocks in provider order.
    pub message: CoshCoreAssistantBody,
}

/// Body of one completed assistant message.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CoshCoreAssistantBody {
    /// Completed content blocks.
    pub content: Vec<CoshCoreContentBlock>,
}

/// Completed assistant content block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum CoshCoreContentBlock {
    /// Assistant text.
    #[serde(rename = "text")]
    Text {
        /// Completed text.
        text: String,
    },
    /// Declared provider tool use.
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Provider tool-use identifier.
        id: String,
        /// Provider tool name.
        name: String,
        /// Typed tool input remains opaque until broker normalization.
        input: Value,
    },
}

/// Completed tool result emitted on the private user-message output path.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CoshCoreToolResult {
    /// Provider tool-use identifier.
    pub tool_use_id: String,
    /// Whether the tool result represents failure.
    pub is_error: bool,
    /// Bounded provider-facing result content.
    pub content: String,
}

/// Correlated private control request from cosh-core.
#[derive(Debug, Clone, PartialEq)]
pub struct CoshCoreControlRequestEnvelope {
    /// Core-provided request identifier.
    pub request_id: String,
    /// Typed request payload.
    pub request: CoshCoreControlRequest,
}

/// Private control requests that require a bridge-owned response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", deny_unknown_fields)]
pub enum CoshCoreControlRequest {
    /// Requests policy evaluation for provider tool intent.
    #[serde(rename = "can_use_tool")]
    CanUseTool {
        /// Provider tool name.
        tool_name: String,
        /// Tool input to canonicalize before broker evaluation.
        input: Value,
        /// Optional provider description.
        #[serde(default)]
        description: Option<String>,
        /// Provider tool-use identifier.
        tool_use_id: String,
        /// Optional durable audit correlation.
        #[serde(default)]
        audit_ref: Option<String>,
        /// Whether hooks independently require approval.
        #[serde(default)]
        hook_requires_approval: bool,
    },
    /// Requests durable user input.
    #[serde(rename = "ask_user")]
    AskUser {
        /// Provider tool-use identifier required by the brokered profile.
        #[serde(default)]
        tool_use_id: Option<String>,
        /// Question text.
        question: String,
        /// Strict user-presentable choices.
        options: Vec<CoshCoreAskUserOption>,
        /// Whether free text is accepted.
        allow_free_text: bool,
        /// Whether multiple options can be selected.
        multi_select: bool,
    },
    /// Requests credential bootstrap or reauthentication.
    #[serde(rename = "auth_required")]
    AuthRequired {
        /// Stable private auth reason.
        reason: String,
        /// Provider-safe auth error.
        #[serde(default)]
        error_message: Option<String>,
        /// Credential schemas; secret values never appear here.
        providers: Vec<Value>,
    },
    /// Requests bounded evidence owned by a separate capability.
    #[serde(rename = "shell_evidence")]
    ShellEvidence {
        /// Provider tool-use identifier.
        tool_use_id: String,
        /// Evidence operation.
        action: String,
        /// List bound.
        #[serde(default)]
        limit: Option<u16>,
        /// Pagination cursor.
        #[serde(default)]
        cursor: Option<String>,
        /// Evidence output identifier.
        #[serde(default)]
        output_id: Option<String>,
        /// Read direction.
        #[serde(default)]
        direction: Option<String>,
        /// Read line bound.
        #[serde(default)]
        lines: Option<u16>,
        /// Explicit compatibility flag subject to broker policy.
        #[serde(default)]
        bypass_recent_filter: Option<bool>,
    },
}

/// Strict private-wire choice carried by `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoshCoreAskUserOption {
    /// User-visible choice label.
    pub label: String,
    /// Optional user-visible explanation.
    #[serde(default)]
    pub description: Option<String>,
}

/// Non-initialization private control response.
#[derive(Debug, Clone, PartialEq)]
pub struct CoshCoreControlResponse {
    /// Private request identifier.
    pub request_id: String,
    /// Response subtype.
    pub subtype: String,
    /// Response body retained for management-path mapping.
    pub body: Value,
}

/// Core-emitted result for one provider turn.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CoshCoreResult {
    /// Optional private result subtype.
    #[serde(default)]
    pub subtype: Option<String>,
    /// Whether the provider/core classified the turn as failure.
    pub is_error: bool,
    /// Provider-facing result summary.
    #[serde(default)]
    pub result: Option<String>,
    /// Structured error summaries.
    #[serde(default)]
    pub errors: Option<Vec<String>>,
    /// Stable private core error code.
    #[serde(default)]
    pub error_code: Option<String>,
    /// Provider turn budget when reported.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Stable provider-session error code.
    #[serde(default)]
    pub session_error_code: Option<String>,
    /// Provider-session phase that failed.
    #[serde(default)]
    pub session_error_phase: Option<String>,
    /// Provider session binding returned by cosh-core.
    #[serde(default, rename = "session_id")]
    pub provider_session_id: Option<String>,
    /// Proposed environment change; not execution authority.
    #[serde(default)]
    pub env_delta: Option<Value>,
    /// Core-reported turn duration.
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// Private codec failure; callers map this to public protocol errors.
#[derive(Debug, Error)]
pub enum CoshCoreCodecError {
    /// Codec configuration was unsafe.
    #[error("private cosh-core JSONL limit must be non-zero")]
    InvalidLimit,
    /// Operation was not valid in the current negotiation phase.
    #[error("private cosh-core operation {operation} is invalid in phase {phase:?}")]
    InvalidPhase {
        /// Attempted operation.
        operation: &'static str,
        /// Current codec phase.
        phase: CoshCoreProtocolPhase,
    },
    /// Wire line exceeded its configured byte budget.
    #[error("private cosh-core JSONL frame exceeds the {limit}-byte limit")]
    FrameTooLarge {
        /// Maximum accepted frame size.
        limit: usize,
    },
    /// Wire bytes were not UTF-8.
    #[error("private cosh-core JSONL frame is not valid UTF-8")]
    InvalidUtf8,
    /// Empty lines are not protocol messages.
    #[error("private cosh-core JSONL frame is empty")]
    EmptyFrame,
    /// JSON or typed payload deserialization failed.
    #[error("malformed private cosh-core JSONL frame: {0}")]
    Malformed(#[from] serde_json::Error),
    /// Unknown top-level message types fail closed.
    #[error("unknown private cosh-core output type {0:?}")]
    UnknownMessageType(String),
    /// Initialization received output outside its allowed bootstrap subset.
    #[error("unexpected private cosh-core output {0:?} before initialization")]
    UnexpectedBeforeInitialization(String),
    /// Initialize response did not match the outstanding request.
    #[error("private cosh-core initialize response correlation mismatch")]
    InitializeCorrelationMismatch,
    /// Peer rejected private protocol initialization.
    #[error("private cosh-core initialize rejected: {0}")]
    InitializeRejected(String),
    /// Peer omitted exact private protocol version negotiation.
    #[error("private cosh-core initialize response omitted protocol_version")]
    InitializeVersionMissing,
    /// Peer advertised an incompatible private protocol version.
    #[error("private cosh-core protocol version {actual} does not match required {required}")]
    InitializeVersionMismatch {
        /// Required version.
        required: u32,
        /// Peer version.
        actual: u32,
    },
    /// Production bridge requires an explicit capability snapshot.
    #[error("private cosh-core initialize response omitted capabilities")]
    InitializeCapabilitiesMissing,
    /// Peer did not acknowledge the exact launch profile.
    #[error("private cosh-core initialize response did not acknowledge the execution profile")]
    InitializeProfileMismatch,
    /// Peer capability snapshot is unsafe for the selected execution profile.
    #[error("private cosh-core initialize capabilities are invalid for the execution profile")]
    InitializeCapabilitiesInvalid,
    /// A profile-specific frame was requested from a legacy codec.
    #[error("private cosh-core operation {operation} requires the brokered profile")]
    ProfileMismatch {
        /// Rejected codec operation.
        operation: &'static str,
    },
    /// Output following a terminal result violates deterministic settlement.
    #[error("private cosh-core emitted output after terminal result")]
    OutputAfterTerminal,
    /// A second initialize response cannot mutate the negotiated snapshot.
    #[error("private cosh-core emitted a duplicate initialize response")]
    DuplicateInitializeResponse,
}
