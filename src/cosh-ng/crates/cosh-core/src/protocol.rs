use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::brokered_profile::BrokeredCapabilityProfileIdentity;
use crate::config::ApprovalMode;

/// Exact legacy shell-to-core control protocol version supported by this binary.
pub const CONTROL_PROTOCOL_VERSION: u32 = 1;
/// Exact private protocol version for the Gateway-owned execution profile.
pub const BROKERED_CONTROL_PROTOCOL_VERSION: u32 = 3;
/// Exact private protocol version for the Gateway-owned checkpoint profile.
pub const BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION: u32 = 4;
/// Exact private protocol version for the approval-gated workspace-write profile.
pub const BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION: u32 = 5;
/// Exact private launch name for the task-only brokered profile.
pub(crate) const GATEWAY_BROKERED_V1_EXECUTION_PROFILE: &str = "gateway_brokered_v1";
/// Exact private launch name for the checkpoint-enabled brokered profile.
pub(crate) const GATEWAY_BROKERED_CHECKPOINT_V1_EXECUTION_PROFILE: &str =
    "gateway_brokered_checkpoint_v1";
/// Exact private launch name for the workspace-write brokered profile.
pub(crate) const GATEWAY_BROKERED_WORKSPACE_WRITE_V1_EXECUTION_PROFILE: &str =
    "gateway_brokered_workspace_write_v1";

// =====================================================================
// Auth types (used by CoreControlRequest::AuthRequired)
// =====================================================================

#[derive(Debug, Clone, Serialize)]
pub enum AuthReason {
    #[serde(rename = "not_configured")]
    NotConfigured,
    #[serde(rename = "invalid")]
    Invalid,
    #[serde(rename = "expired")]
    Expired,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthField {
    pub name: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub secret: bool,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthProvider {
    pub id: String,
    pub label: String,
    /// Short guidance shown under the provider label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Simplified Chinese guidance shown when the shell uses zh-CN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_zh_cn: Option<String>,
    pub fields: Vec<AuthField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin_base_url: Option<String>,
    pub builtin_provider_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin_default_model: Option<String>,
}

// =====================================================================
// Input messages (Shell → Core, read from stdin)
// =====================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum InputMessage {
    #[serde(rename = "user")]
    User {
        message: UserMessageContent,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        parent_tool_use_id: Option<Value>,
        #[serde(default)]
        shell_context: Option<ShellContext>,
    },

    #[serde(rename = "control_request")]
    ControlRequest {
        request_id: String,
        request: ShellControlRequest,
    },

    #[serde(rename = "control_response")]
    ControlResponse {
        response: Box<ControlResponsePayload>,
    },

    /// #1940 receipt protocol: the shell emits this as soon as a control
    /// approval request reaches its main thread, proving the request has an
    /// owner for its terminal state. Cores that predate the receipt simply
    /// fail to deserialize this line and keep their residual guard.
    #[serde(rename = "approval_receipt")]
    ApprovalReceipt { request_id: String },

    #[serde(rename = "registry_request")]
    RegistryRequest {
        request_id: String,
        domain: String,
        action: String,
        #[serde(default)]
        params: Value,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessageContent {
    pub role: String,
    pub content: String,
    /// Original user text supplied by cosh-shell for hook compatibility.
    ///
    /// Older clients omit this field; callers must fall back to `content`.
    #[serde(default)]
    pub raw_user_input: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShellContext {
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub last_exit_code: i32,
}

/// Control capabilities a client declares in its `initialize` request.
///
/// Every field defaults to `false` so a legacy client that predates this
/// field keeps the pre-capability behavior exactly; the core only switches
/// trust-mode shell execution onto the approval channel when the client has
/// opted into both halves of that exchange (#2067).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ClientControlCapabilities {
    #[serde(default)]
    pub can_handle_can_use_tool: bool,
    #[serde(default)]
    pub can_handle_host_executed_shell: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "subtype")]
pub enum ShellControlRequest {
    #[serde(rename = "initialize")]
    Initialize {
        /// Whether this initialization should run SessionStart hooks.
        ///
        /// The default keeps older clients compatible. cosh-shell's one-shot
        /// transport disables the lifecycle event because its former
        /// positional invocation did not emit startup hooks.
        #[serde(default = "default_fire_session_start")]
        fire_session_start: bool,
        /// Missing or null only for legacy shells that predate negotiation.
        #[serde(default)]
        protocol_version: Option<u32>,
        #[serde(default)]
        capabilities: ClientControlCapabilities,
        /// Exact launch profile requested by a brokered Gateway peer.
        #[serde(default)]
        execution_profile: Option<String>,
        /// Closed capability identity requested by a v3 Gateway peer.
        #[serde(default)]
        capability_profile: Option<BrokeredCapabilityProfileIdentity>,
    },

    #[serde(rename = "interrupt")]
    Interrupt,

    #[serde(rename = "shutdown")]
    Shutdown,

    #[serde(rename = "config_override")]
    ConfigOverride {
        #[serde(default)]
        approval_mode: Option<ApprovalMode>,
        #[serde(default)]
        allowed_tools: Option<Vec<String>>,
    },

    #[serde(rename = "switch_model")]
    SwitchModel { model: String },

    #[serde(rename = "reload_config")]
    ReloadConfig,
}

fn default_fire_session_start() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponsePayload {
    pub subtype: String,
    pub request_id: String,
    pub response: ControlResponseBody,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponseBody {
    pub behavior: Option<String>,
    pub message: Option<String>,
    pub result: Option<HostExecutedShellResult>,
    #[serde(default, rename = "checkpointResult")]
    pub checkpoint_result: Option<Value>,
    #[serde(default, rename = "checkpointError")]
    pub checkpoint_error: Option<Value>,
    #[serde(rename = "toolUseID")]
    pub tool_use_id: Option<String>,
    #[serde(default, rename = "updatedPermissions")]
    pub updated_permissions: Option<Value>,
    pub answer: Option<String>,
    pub selected_options: Option<Vec<usize>>,
    // Auth response fields
    pub provider_id: Option<String>,
    pub provider_type: Option<String>,
    #[serde(default)]
    pub values: Option<HashMap<String, String>>,
    pub persist: Option<bool>,
    #[serde(default, flatten)]
    pub unknown_fields: HashMap<String, Value>,
}

/// Typed successful result returned only by the Gateway checkpoint target.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostExecutedCheckpointCreateResult {
    pub checkpoint_id: String,
    pub outcome: HostExecutedCheckpointCreateOutcome,
}

/// Target-reported outcome for a hosted checkpoint creation.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostExecutedCheckpointCreateOutcome {
    Created { snapshot_id: String },
    Skipped { reason: String },
}

/// Typed known-failure or uncertain result returned by the Gateway target.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostExecutedCheckpointError {
    pub outcome: HostExecutedCheckpointErrorOutcome,
    pub code: String,
    pub message: String,
}

/// Stable terminal classification for a hosted checkpoint error.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostExecutedCheckpointErrorOutcome {
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostExecutedShellResult {
    #[serde(rename = "llmContent")]
    pub llm_content: String,
    #[serde(rename = "returnDisplay")]
    pub return_display: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

// =====================================================================
// Output messages (Core → Shell, written to stdout)
// =====================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum OutputMessage {
    #[serde(rename = "system")]
    System {
        subtype: String,
        #[serde(flatten)]
        payload: SystemPayload,
    },

    #[serde(rename = "stream_event")]
    StreamEvent { event: StreamEventPayload },

    #[serde(rename = "assistant")]
    Assistant {
        session_id: String,
        message: AssistantMessage,
    },

    #[serde(rename = "user")]
    User {
        session_id: String,
        message: UserOutputMessage,
    },

    #[serde(rename = "control_request")]
    ControlRequest {
        request_id: String,
        request: CoreControlRequest,
    },

    #[serde(rename = "control_response")]
    ControlResponse {
        response: CoreControlResponsePayload,
    },

    #[serde(rename = "result")]
    Result {
        #[serde(skip_serializing_if = "Option::is_none")]
        subtype: Option<String>,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        errors: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_error_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_error_phase: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env_delta: Option<EnvDelta>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },

    #[serde(rename = "registry_response")]
    RegistryResponse {
        request_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreControlResponsePayload {
    pub subtype: String,
    pub request_id: String,
    pub response: CoreControlResponseBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreControlResponseBody {
    pub subtype: String,
    pub protocol_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_profile: Option<BrokeredCapabilityProfileIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CoreControlCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreControlCapabilities {
    pub can_handle_can_use_tool: bool,
    pub can_handle_host_executed_shell_tool_result: bool,
    pub can_handle_shell_evidence_tool: bool,
    /// #1940 receipt protocol: the core consumes `approval_receipt` lines to
    /// disarm its last-resort approval timeout. Announced so the shell only
    /// sends receipts to a core that understands them; older or mock
    /// providers without this capability never see receipt lines.
    pub can_handle_approval_receipt: bool,
    /// Core accepts the typed checkpoint terminal result on private v4.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub can_handle_hosted_checkpoint_create: bool,
    /// Brokered control can suspend one side-effect-free question for Gateway resolution.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub can_handle_brokered_ask_user: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SystemPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_resumable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Hook decision (allow/ask/block/deny) for per-hook color-coding in cosh-shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserOutputMessage {
    pub role: String,
    pub content: Vec<UserContentBlock>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum UserContentBlock {
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        content: String,
        /// Machine-readable hook terminal verdict (#2156), present only when
        /// a hook blocked the call. Clients must key rejection semantics on
        /// this marker instead of inferring from the result text, which the
        /// command itself can control. Absent on every other result, so the
        /// field is additive and absent-safe for older clients.
        #[serde(skip_serializing_if = "Option::is_none")]
        cosh_hook_verdict: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "subtype")]
pub enum CoreControlRequest {
    #[serde(rename = "can_use_tool")]
    CanUseTool {
        tool_name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        audit_ref: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        hook_requires_approval: bool,
    },

    #[serde(rename = "ask_user")]
    AskUser {
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        question: String,
        options: Vec<AskUserOption>,
        allow_free_text: bool,
        multi_select: bool,
    },

    #[serde(rename = "auth_required")]
    AuthRequired {
        reason: AuthReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        providers: Vec<AuthProvider>,
    },

    #[serde(rename = "shell_evidence")]
    ShellEvidence {
        tool_use_id: String,
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        direction: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lines: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bypass_recent_filter: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct AskUserOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum StreamEventPayload {
    #[serde(rename = "message_start")]
    MessageStart,

    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockInfo,
    },

    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: ContentDelta },

    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },

    #[serde(rename = "message_stop")]
    MessageStop,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlockInfo {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[allow(clippy::enum_variant_names)]
pub enum ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_cwd: Option<PathBuf>,
    #[serde(default)]
    pub env_changes: HashMap<String, Option<String>>,
}

// =====================================================================
// Helper constructors
// =====================================================================

impl OutputMessage {
    pub fn system_init(
        session_id: &str,
        model: &str,
        tools: Vec<String>,
        session_resumable: bool,
    ) -> Self {
        Self::System {
            subtype: "init".to_string(),
            payload: SystemPayload {
                session_id: Some(session_id.to_string()),
                session_resumable: Some(session_resumable),
                model: Some(model.to_string()),
                tools: Some(tools),
                ..Default::default()
            },
        }
    }

    pub fn initialize_success(request_id: &str, can_handle_shell_evidence_tool: bool) -> Self {
        Self::initialize_success_for_profile(
            request_id,
            CONTROL_PROTOCOL_VERSION,
            None,
            None,
            None,
            can_handle_shell_evidence_tool,
        )
    }

    /// Builds an initialize acknowledgement bound to the selected launch profile.
    pub fn initialize_success_for_profile(
        request_id: &str,
        protocol_version: u32,
        execution_profile: Option<&str>,
        capability_profile: Option<BrokeredCapabilityProfileIdentity>,
        runtime_tools: Option<Vec<String>>,
        can_handle_shell_evidence_tool: bool,
    ) -> Self {
        Self::ControlResponse {
            response: CoreControlResponsePayload {
                subtype: "success".to_string(),
                request_id: request_id.to_string(),
                response: CoreControlResponseBody {
                    subtype: "initialize".to_string(),
                    protocol_version,
                    execution_profile: execution_profile.map(str::to_string),
                    capability_profile,
                    runtime_tools,
                    capabilities: Some(CoreControlCapabilities {
                        can_handle_can_use_tool: true,
                        can_handle_host_executed_shell_tool_result: execution_profile.is_none(),
                        can_handle_shell_evidence_tool,
                        can_handle_approval_receipt: true,
                        can_handle_hosted_checkpoint_create: execution_profile
                            == Some(GATEWAY_BROKERED_CHECKPOINT_V1_EXECUTION_PROFILE),
                        can_handle_brokered_ask_user: execution_profile.is_some(),
                    }),
                    error: None,
                },
            },
        }
    }

    /// Builds a fail-loud initialize response for an unsupported exact version.
    pub fn initialize_version_error(request_id: &str, received_version: u32) -> Self {
        Self::initialize_negotiation_error(
            request_id,
            CONTROL_PROTOCOL_VERSION,
            None,
            None,
            format!(
                "unsupported control protocol version {received_version}; expected exact version {CONTROL_PROTOCOL_VERSION}"
            ),
        )
    }

    /// Builds a fail-loud response for a version or launch-profile mismatch.
    pub fn initialize_negotiation_error(
        request_id: &str,
        protocol_version: u32,
        execution_profile: Option<&str>,
        capability_profile: Option<BrokeredCapabilityProfileIdentity>,
        error: String,
    ) -> Self {
        Self::ControlResponse {
            response: CoreControlResponsePayload {
                subtype: "error".to_string(),
                request_id: request_id.to_string(),
                response: CoreControlResponseBody {
                    subtype: "initialize".to_string(),
                    protocol_version,
                    execution_profile: execution_profile.map(str::to_string),
                    capability_profile,
                    runtime_tools: None,
                    capabilities: None,
                    error: Some(error),
                },
            },
        }
    }

    pub fn system_status(status: &str) -> Self {
        Self::System {
            subtype: "status".to_string(),
            payload: SystemPayload {
                status: Some(status.to_string()),
                ..Default::default()
            },
        }
    }

    pub fn hook_notification(
        hook_name: &str,
        message: &str,
        tool_use_id: Option<&str>,
        decision: Option<&str>,
    ) -> Self {
        Self::System {
            subtype: "hook_notification".to_string(),
            payload: SystemPayload {
                status: Some(message.to_string()),
                hook_name: Some(hook_name.to_string()),
                tool_use_id: tool_use_id.map(String::from),
                decision: decision.map(String::from),
                ..Default::default()
            },
        }
    }

    pub fn assistant_text(session_id: &str, text: &str) -> Self {
        Self::Assistant {
            session_id: session_id.to_string(),
            message: AssistantMessage {
                content: vec![ContentBlock::Text {
                    text: text.to_string(),
                }],
            },
        }
    }

    pub fn tool_result(session_id: &str, tool_use_id: &str, content: &str, is_error: bool) -> Self {
        Self::User {
            session_id: session_id.to_string(),
            message: UserOutputMessage {
                role: "user".to_string(),
                content: vec![UserContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    is_error,
                    content: content.to_string(),
                    cosh_hook_verdict: None,
                }],
            },
        }
    }

    /// A blocked tool result carrying the machine-readable hook verdict
    /// marker (#2156). Only the M2 hook-block release uses this constructor;
    /// every other result keeps the marker absent.
    pub fn tool_result_hook_blocked(session_id: &str, tool_use_id: &str, content: &str) -> Self {
        Self::User {
            session_id: session_id.to_string(),
            message: UserOutputMessage {
                role: "user".to_string(),
                content: vec![UserContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    is_error: true,
                    content: content.to_string(),
                    cosh_hook_verdict: Some("blocked".to_string()),
                }],
            },
        }
    }

    pub fn result_success(session_id: &str, result: &str) -> Self {
        Self::Result {
            subtype: Some("success".to_string()),
            is_error: false,
            result: Some(result.to_string()),
            errors: None,
            error_code: None,
            max_turns: None,
            session_error_code: None,
            session_error_phase: None,
            session_id: Some(session_id.to_string()),
            env_delta: None,
            duration_ms: None,
        }
    }

    pub fn result_error(session_id: &str, error: &str) -> Self {
        Self::result_error_with_code(session_id, error, None)
    }

    /// Builds an error result with a stable code for machine consumers.
    pub fn result_error_with_code(session_id: &str, error: &str, error_code: Option<&str>) -> Self {
        Self::Result {
            subtype: Some("error".to_string()),
            is_error: true,
            result: Some(error.to_string()),
            errors: Some(vec![error.to_string()]),
            error_code: error_code.map(str::to_string),
            max_turns: None,
            session_error_code: None,
            session_error_phase: None,
            session_id: Some(session_id.to_string()),
            env_delta: None,
            duration_ms: None,
        }
    }

    /// Builds a max-turn result with stable machine-readable metadata.
    pub fn max_turns_result_error(session_id: &str, error: &str, max_turns: u32) -> Self {
        Self::Result {
            subtype: Some("error".to_string()),
            is_error: true,
            result: Some(error.to_string()),
            errors: Some(vec![error.to_string()]),
            error_code: Some("max_turns".to_string()),
            max_turns: Some(max_turns),
            session_error_code: None,
            session_error_phase: None,
            session_id: Some(session_id.to_string()),
            env_delta: None,
            duration_ms: None,
        }
    }

    pub fn session_result_error(
        session_id: &str,
        error: &str,
        session_error_code: &str,
        session_error_phase: &str,
    ) -> Self {
        Self::Result {
            subtype: Some("error".to_string()),
            is_error: true,
            result: Some(error.to_string()),
            errors: Some(vec![error.to_string()]),
            error_code: None,
            max_turns: None,
            session_error_code: Some(session_error_code.to_string()),
            session_error_phase: Some(session_error_phase.to_string()),
            session_id: Some(session_id.to_string()),
            env_delta: None,
            duration_ms: None,
        }
    }

    pub fn stream_message_start() -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::MessageStart,
        }
    }

    pub fn stream_message_stop() -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::MessageStop,
        }
    }

    pub fn stream_text_start(index: u32) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockStart {
                index,
                content_block: ContentBlockInfo::Text,
            },
        }
    }

    pub fn stream_text_delta(index: u32, text: &str) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockDelta {
                index,
                delta: ContentDelta::TextDelta {
                    text: text.to_string(),
                },
            },
        }
    }

    pub fn stream_tool_use_start(index: u32, id: &str, name: &str) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockStart {
                index,
                content_block: ContentBlockInfo::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                },
            },
        }
    }

    pub fn stream_tool_use_delta(index: u32, partial_json: &str) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockDelta {
                index,
                delta: ContentDelta::InputJsonDelta {
                    partial_json: partial_json.to_string(),
                },
            },
        }
    }

    pub fn stream_thinking_start(index: u32) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockStart {
                index,
                content_block: ContentBlockInfo::Thinking,
            },
        }
    }

    pub fn stream_thinking_delta(index: u32, thinking: &str) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockDelta {
                index,
                delta: ContentDelta::ThinkingDelta {
                    thinking: thinking.to_string(),
                },
            },
        }
    }

    pub fn stream_block_stop(index: u32) -> Self {
        Self::StreamEvent {
            event: StreamEventPayload::ContentBlockStop { index },
        }
    }

    pub fn can_use_tool(
        request_id: &str,
        tool_name: &str,
        input: Value,
        tool_use_id: &str,
        hook_requires_approval: bool,
    ) -> Self {
        Self::can_use_tool_with_audit_ref(
            request_id,
            tool_name,
            input,
            tool_use_id,
            hook_requires_approval,
            None,
        )
    }

    /// Builds a Tool approval request linked to a persisted audit event.
    pub fn can_use_tool_with_audit_ref(
        request_id: &str,
        tool_name: &str,
        input: Value,
        tool_use_id: &str,
        hook_requires_approval: bool,
        audit_ref: Option<String>,
    ) -> Self {
        Self::ControlRequest {
            request_id: request_id.to_string(),
            request: CoreControlRequest::CanUseTool {
                tool_name: tool_name.to_string(),
                input,
                description: None,
                tool_use_id: tool_use_id.to_string(),
                hook_requires_approval,
                audit_ref,
            },
        }
    }

    pub fn auth_required(
        request_id: &str,
        reason: AuthReason,
        error_message: Option<String>,
        providers: Vec<AuthProvider>,
    ) -> Self {
        Self::ControlRequest {
            request_id: request_id.to_string(),
            request: CoreControlRequest::AuthRequired {
                reason,
                error_message,
                providers,
            },
        }
    }

    pub fn shell_evidence_list_commands(
        request_id: &str,
        tool_use_id: &str,
        limit: u16,
        cursor: Option<&str>,
    ) -> Self {
        Self::ControlRequest {
            request_id: request_id.to_string(),
            request: CoreControlRequest::ShellEvidence {
                tool_use_id: tool_use_id.to_string(),
                action: "list_commands".to_string(),
                limit: Some(limit),
                cursor: cursor.map(str::to_string),
                output_id: None,
                direction: None,
                lines: None,
                bypass_recent_filter: None,
            },
        }
    }

    pub fn shell_evidence_read_output(
        request_id: &str,
        tool_use_id: &str,
        output_id: &str,
        direction: &str,
        lines: u16,
        bypass_recent_filter: bool,
    ) -> Self {
        Self::ControlRequest {
            request_id: request_id.to_string(),
            request: CoreControlRequest::ShellEvidence {
                tool_use_id: tool_use_id.to_string(),
                action: "read_output".to_string(),
                limit: None,
                cursor: None,
                output_id: Some(output_id.to_string()),
                direction: Some(direction.to_string()),
                lines: Some(lines),
                bypass_recent_filter: bypass_recent_filter.then_some(true),
            },
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_message() {
        let json = r#"{"type":"user","message":{"role":"user","content":"hello world"},"parent_tool_use_id":null,"session_id":"default"}"#;
        let msg: InputMessage = serde_json::from_str(json).expect("should parse user message");
        match msg {
            InputMessage::User {
                message,
                session_id,
                ..
            } => {
                assert_eq!(message.role, "user");
                assert_eq!(message.content, "hello world");
                assert_eq!(message.raw_user_input, None);
                assert_eq!(session_id.as_deref(), Some("default"));
            }
            _ => panic!("expected User variant"),
        }
    }

    #[test]
    fn parse_user_message_preserves_optional_raw_input() {
        let json = r#"{"type":"user","message":{"role":"user","content":"envelope","raw_user_input":"raw"}}"#;
        let msg: InputMessage = serde_json::from_str(json).expect("should parse raw user input");
        match msg {
            InputMessage::User { message, .. } => {
                assert_eq!(message.content, "envelope");
                assert_eq!(message.raw_user_input.as_deref(), Some("raw"));
            }
            _ => panic!("expected User variant"),
        }
    }

    #[test]
    fn parse_initialize_can_disable_session_start() {
        let json = r#"{"request_id":"init-1","type":"control_request","request":{"subtype":"initialize","fire_session_start":false}}"#;
        let msg: InputMessage = serde_json::from_str(json).expect("should parse initialize");
        assert!(matches!(
            msg,
            InputMessage::ControlRequest {
                request: ShellControlRequest::Initialize {
                    fire_session_start: false,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn parse_initialize_request() {
        let json = r#"{"request_id":"init-1","type":"control_request","request":{"subtype":"initialize"}}"#;
        let msg: InputMessage = serde_json::from_str(json).expect("should parse initialize");
        match msg {
            InputMessage::ControlRequest {
                request_id,
                request,
            } => {
                assert_eq!(request_id, "init-1");
                match request {
                    ShellControlRequest::Initialize {
                        fire_session_start,
                        protocol_version,
                        capabilities,
                        execution_profile,
                        capability_profile,
                    } => {
                        assert!(fire_session_start);
                        assert!(protocol_version.is_none());
                        assert!(!capabilities.can_handle_can_use_tool);
                        assert!(!capabilities.can_handle_host_executed_shell);
                        assert!(execution_profile.is_none());
                        assert!(capability_profile.is_none());
                    }
                    _ => panic!("expected Initialize variant"),
                }
            }
            _ => panic!("expected ControlRequest variant"),
        }
    }

    #[test]
    fn parse_initialize_request_preserves_profile_and_capabilities() {
        let identity = BrokeredCapabilityProfileIdentity::task_only_v1();
        let json = serde_json::json!({
            "request_id": "init-3",
            "type": "control_request",
            "request": {
                "subtype": "initialize",
                "protocol_version": BROKERED_CONTROL_PROTOCOL_VERSION,
                "capabilities": {
                    "can_handle_can_use_tool": true,
                    "can_handle_host_executed_shell": true
                },
                "execution_profile": "gateway_brokered_v1",
                "capability_profile": identity
            }
        });
        let msg: InputMessage = serde_json::from_value(json).expect("should parse initialize");
        match msg {
            InputMessage::ControlRequest {
                request_id,
                request,
            } => {
                assert_eq!(request_id, "init-3");
                match request {
                    ShellControlRequest::Initialize {
                        capabilities,
                        execution_profile,
                        capability_profile,
                        ..
                    } => {
                        assert!(capabilities.can_handle_can_use_tool);
                        assert!(capabilities.can_handle_host_executed_shell);
                        assert_eq!(execution_profile.as_deref(), Some("gateway_brokered_v1"));
                        assert_eq!(
                            capability_profile,
                            Some(BrokeredCapabilityProfileIdentity::task_only_v1())
                        );
                    }
                    _ => panic!("expected Initialize variant"),
                }
            }
            _ => panic!("expected ControlRequest variant"),
        }
    }

    #[test]
    fn parse_versioned_initialize_request() {
        let json = r#"{"request_id":"init-1","type":"control_request","request":{"subtype":"initialize","protocol_version":1}}"#;
        let msg: InputMessage = serde_json::from_str(json).expect("should parse initialize");
        assert!(matches!(
            msg,
            InputMessage::ControlRequest {
                request: ShellControlRequest::Initialize {
                    protocol_version: Some(CONTROL_PROTOCOL_VERSION),
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn config_override_uses_typed_approval_mode() {
        for (value, expected) in [
            ("recommend", ApprovalMode::Recommend),
            ("balanced", ApprovalMode::Recommend),
            ("strict", ApprovalMode::Recommend),
            ("suggest", ApprovalMode::Recommend),
            ("auto", ApprovalMode::Auto),
            ("trust", ApprovalMode::Trust),
        ] {
            let json = format!(
                r#"{{"request_id":"cfg-1","type":"control_request","request":{{"subtype":"config_override","approval_mode":"{value}"}}}}"#
            );
            let message: InputMessage = serde_json::from_str(&json).expect("typed override");
            assert!(matches!(
                message,
                InputMessage::ControlRequest {
                    request: ShellControlRequest::ConfigOverride {
                        approval_mode: Some(mode),
                        ..
                    },
                    ..
                } if mode == expected
            ));
        }

        let invalid = r#"{"request_id":"cfg-1","type":"control_request","request":{"subtype":"config_override","approval_mode":"invalid"}}"#;
        assert!(serde_json::from_str::<InputMessage>(invalid).is_err());
    }

    #[test]
    fn parse_control_response_allow() {
        let json = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"allow","updatedPermissions":[],"toolUseID":"toolu_abc"}}}"#;
        let msg: InputMessage = serde_json::from_str(json).expect("should parse control_response");
        match msg {
            InputMessage::ControlResponse { response } => {
                assert_eq!(response.request_id, "req-1");
                assert_eq!(response.response.behavior.as_deref(), Some("allow"));
                assert_eq!(response.response.tool_use_id.as_deref(), Some("toolu_abc"));
            }
            _ => panic!("expected ControlResponse variant"),
        }
    }

    #[test]
    fn parse_control_response_deny() {
        let json = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-2","response":{"behavior":"deny","message":"User denied"}}}"#;
        let msg: InputMessage = serde_json::from_str(json).unwrap();
        match msg {
            InputMessage::ControlResponse { response } => {
                assert_eq!(response.response.behavior.as_deref(), Some("deny"));
                assert_eq!(response.response.message.as_deref(), Some("User denied"));
            }
            _ => panic!("expected ControlResponse"),
        }
    }

    #[test]
    fn parse_control_response_host_executed_shell() {
        let json = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-3","response":{"behavior":"host_executed_shell","result":{"llmContent":"ShellCommandCompleted evidence\ncommand: df -h","returnDisplay":"df -h completed","metadata":{"command":"df -h","status":"completed","exit_code":0}}}}}"#;
        let msg: InputMessage = serde_json::from_str(json).unwrap();
        match msg {
            InputMessage::ControlResponse { response } => {
                assert_eq!(
                    response.response.behavior.as_deref(),
                    Some("host_executed_shell")
                );
                let result = response.response.result.expect("host result");
                assert!(result.llm_content.contains("ShellCommandCompleted"));
                assert_eq!(result.return_display.as_deref(), Some("df -h completed"));
                assert_eq!(
                    result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("exit_code"))
                        .and_then(Value::as_i64),
                    Some(0)
                );
            }
            _ => panic!("expected ControlResponse"),
        }
    }

    #[test]
    fn parse_control_response_host_executed_shell_with_null_return_display() {
        let json = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-3","response":{"behavior":"host_executed_shell","result":{"llmContent":"ShellCommandCompleted evidence\ncommand: df -h","returnDisplay":null,"metadata":{"command":"df -h","status":"completed","exit_code":0}}}}}"#;
        let msg: InputMessage = serde_json::from_str(json).unwrap();
        match msg {
            InputMessage::ControlResponse { response } => {
                let result = response.response.result.expect("host result");
                assert!(result.llm_content.contains("ShellCommandCompleted"));
                assert_eq!(result.return_display, None);
                assert_eq!(
                    result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("exit_code"))
                        .and_then(Value::as_i64),
                    Some(0)
                );
            }
            _ => panic!("expected ControlResponse"),
        }
    }

    #[test]
    fn parse_typed_checkpoint_terminal_responses() {
        let created = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-4","response":{"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"ckp_123e4567-e89b-12d3-a456-426614174000","outcome":{"status":"created","snapshot_id":"snap-1"}}}}}"#;
        let message: InputMessage = serde_json::from_str(created).unwrap();
        let InputMessage::ControlResponse { response } = message else {
            panic!("expected checkpoint response");
        };
        let checkpoint_result: HostExecutedCheckpointCreateResult = serde_json::from_value(
            response
                .response
                .checkpoint_result
                .expect("checkpoint result"),
        )
        .unwrap();
        assert!(matches!(
            checkpoint_result,
            HostExecutedCheckpointCreateResult {
                outcome: HostExecutedCheckpointCreateOutcome::Created { ref snapshot_id },
                ..
            } if snapshot_id == "snap-1"
        ));

        let uncertain = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-5","response":{"behavior":"host_executed_checkpoint_error","checkpointError":{"outcome":"uncertain","code":"checkpoint_unknown","message":"Exact evidence was not available"}}}}"#;
        let message: InputMessage = serde_json::from_str(uncertain).unwrap();
        let InputMessage::ControlResponse { response } = message else {
            panic!("expected checkpoint error");
        };
        let checkpoint_error: HostExecutedCheckpointError = serde_json::from_value(
            response
                .response
                .checkpoint_error
                .expect("checkpoint error"),
        )
        .unwrap();
        assert_eq!(
            checkpoint_error,
            HostExecutedCheckpointError {
                outcome: HostExecutedCheckpointErrorOutcome::Uncertain,
                code: "checkpoint_unknown".to_string(),
                message: "Exact evidence was not available".to_string(),
            }
        );
    }

    #[test]
    fn serialize_system_init() {
        let msg = OutputMessage::system_init(
            "sess-1",
            "mock-model",
            vec!["shell".to_string(), "read_file".to_string()],
            true,
        );
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "system");
        assert_eq!(v["subtype"], "init");
        assert_eq!(v["session_id"], "sess-1");
        assert_eq!(v["session_resumable"], true);
        assert_eq!(v["model"], "mock-model");
    }

    #[test]
    fn serialize_initialize_success_capabilities() {
        let msg = OutputMessage::initialize_success("init-1", false);
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "init-1");
        assert_eq!(v["response"]["response"]["subtype"], "initialize");
        assert_eq!(
            v["response"]["response"]["protocol_version"],
            CONTROL_PROTOCOL_VERSION
        );
        assert_eq!(
            v["response"]["response"]["capabilities"]["can_handle_can_use_tool"],
            true
        );
        assert_eq!(
            v["response"]["response"]["capabilities"]["can_handle_host_executed_shell_tool_result"],
            true
        );
        assert_eq!(
            v["response"]["response"]["capabilities"]["can_handle_shell_evidence_tool"],
            false
        );
        assert_eq!(
            v["response"]["response"]["capabilities"]["can_handle_approval_receipt"],
            true
        );
        assert!(v["response"]["response"]["capabilities"]
            .get("can_handle_shell_output_evidence_tool")
            .is_none());
    }

    #[test]
    fn serialize_brokered_initialize_ack_is_v3_and_profile_bound() {
        let capability_profile = BrokeredCapabilityProfileIdentity::task_only_v1();
        let msg = OutputMessage::initialize_success_for_profile(
            "init-brokered",
            BROKERED_CONTROL_PROTOCOL_VERSION,
            Some("gateway_brokered_v1"),
            Some(capability_profile.clone()),
            Some(vec!["ask_user_question".to_string()]),
            false,
        );
        let value = serde_json::to_value(msg).unwrap();
        let response = &value["response"]["response"];
        assert_eq!(
            response["protocol_version"],
            BROKERED_CONTROL_PROTOCOL_VERSION
        );
        assert_eq!(response["execution_profile"], "gateway_brokered_v1");
        assert_eq!(
            response["capability_profile"],
            serde_json::json!(capability_profile)
        );
        assert_eq!(
            response["runtime_tools"],
            serde_json::json!(["ask_user_question"])
        );
        assert!(response["capabilities"]
            .get("can_handle_hosted_checkpoint_create")
            .is_none());
        assert_eq!(
            response["capabilities"]["can_handle_brokered_ask_user"],
            true
        );
        assert!(response["capabilities"]
            .get("can_handle_shell_evidence_tool")
            .is_some());
    }

    #[test]
    fn serialize_checkpoint_initialize_ack_is_v4_and_profile_bound() {
        let capability_profile = BrokeredCapabilityProfileIdentity::workspace_checkpoint_v1();
        let msg = OutputMessage::initialize_success_for_profile(
            "init-checkpoint",
            BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION,
            Some(GATEWAY_BROKERED_CHECKPOINT_V1_EXECUTION_PROFILE),
            Some(capability_profile.clone()),
            Some(vec![
                "ask_user_question".to_string(),
                "workspace_checkpoint_create".to_string(),
            ]),
            false,
        );
        let value = serde_json::to_value(msg).unwrap();
        let response = &value["response"]["response"];
        assert_eq!(
            response["protocol_version"],
            BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION
        );
        assert_eq!(
            response["execution_profile"],
            GATEWAY_BROKERED_CHECKPOINT_V1_EXECUTION_PROFILE
        );
        assert_eq!(
            response["capability_profile"],
            serde_json::json!(capability_profile)
        );
        assert_eq!(
            response["runtime_tools"],
            serde_json::json!(["ask_user_question", "workspace_checkpoint_create"])
        );
        assert_eq!(
            response["capabilities"]["can_handle_hosted_checkpoint_create"],
            true
        );
        assert_eq!(
            response["capabilities"]["can_handle_brokered_ask_user"],
            true
        );
    }

    #[test]
    fn serialize_workspace_write_initialize_ack_is_v5_and_profile_bound() {
        let capability_profile = BrokeredCapabilityProfileIdentity::workspace_write_v1();
        let msg = OutputMessage::initialize_success_for_profile(
            "init-workspace-write",
            BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION,
            Some(GATEWAY_BROKERED_WORKSPACE_WRITE_V1_EXECUTION_PROFILE),
            Some(capability_profile.clone()),
            Some(vec![
                "ask_user_question".to_string(),
                "write_file".to_string(),
            ]),
            false,
        );
        let value = serde_json::to_value(msg).unwrap();
        let response = &value["response"]["response"];
        assert_eq!(
            response["protocol_version"],
            BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION
        );
        assert_eq!(
            response["execution_profile"],
            GATEWAY_BROKERED_WORKSPACE_WRITE_V1_EXECUTION_PROFILE
        );
        assert_eq!(
            response["capability_profile"],
            serde_json::json!(capability_profile)
        );
        assert_eq!(
            response["runtime_tools"],
            serde_json::json!(["ask_user_question", "write_file"])
        );
        assert!(response["capabilities"]
            .get("can_handle_hosted_checkpoint_create")
            .is_none());
        assert_eq!(
            response["capabilities"]["can_handle_brokered_ask_user"],
            true
        );
    }

    #[test]
    fn private_wire_dual_version_corpus_matches_core_types() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/cosh-private-wire-dual-version.json"
        ))
        .unwrap();

        let legacy_ack = OutputMessage::initialize_success_for_profile(
            "gateway-init-1",
            CONTROL_PROTOCOL_VERSION,
            None,
            None,
            None,
            false,
        );
        assert_eq!(
            serde_json::to_value(legacy_ack).unwrap(),
            corpus["legacy_v1"]["initialize_ack"]
        );

        let brokered_ack = OutputMessage::initialize_success_for_profile(
            "gateway-init-v3",
            BROKERED_CONTROL_PROTOCOL_VERSION,
            Some("gateway_brokered_v1"),
            Some(BrokeredCapabilityProfileIdentity::task_only_v1()),
            Some(vec!["ask_user_question".to_string()]),
            false,
        );
        assert_eq!(
            serde_json::to_value(brokered_ack).unwrap(),
            corpus["gateway_brokered_v3"]["initialize_ack"]
        );

        let checkpoint_ack = OutputMessage::initialize_success_for_profile(
            "gateway-init-v4",
            BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION,
            Some(GATEWAY_BROKERED_CHECKPOINT_V1_EXECUTION_PROFILE),
            Some(BrokeredCapabilityProfileIdentity::workspace_checkpoint_v1()),
            Some(vec![
                "ask_user_question".to_string(),
                "workspace_checkpoint_create".to_string(),
            ]),
            false,
        );
        assert_eq!(
            serde_json::to_value(checkpoint_ack).unwrap(),
            corpus["gateway_brokered_checkpoint_v4"]["initialize_ack"]
        );

        let workspace_write_ack = OutputMessage::initialize_success_for_profile(
            "gateway-init-v5",
            BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION,
            Some(GATEWAY_BROKERED_WORKSPACE_WRITE_V1_EXECUTION_PROFILE),
            Some(BrokeredCapabilityProfileIdentity::workspace_write_v1()),
            Some(vec![
                "ask_user_question".to_string(),
                "write_file".to_string(),
            ]),
            false,
        );
        assert_eq!(
            serde_json::to_value(workspace_write_ack).unwrap(),
            corpus["gateway_brokered_workspace_write_v5"]["initialize_ack"]
        );

        let checkpoint_created: InputMessage = serde_json::from_value(
            corpus["gateway_brokered_checkpoint_v4"]["checkpoint_created"].clone(),
        )
        .unwrap();
        assert!(matches!(
            checkpoint_created,
            InputMessage::ControlResponse { response }
                if response.request_id == "checkpoint-request-1"
                    && response.response.behavior.as_deref()
                        == Some("host_executed_checkpoint_create")
                    && response.response.checkpoint_result.is_some()
        ));

        let checkpoint_uncertain: InputMessage = serde_json::from_value(
            corpus["gateway_brokered_checkpoint_v4"]["checkpoint_uncertain"].clone(),
        )
        .unwrap();
        assert!(matches!(
            checkpoint_uncertain,
            InputMessage::ControlResponse { response }
                if response.request_id == "checkpoint-request-2"
                    && response.response.behavior.as_deref()
                        == Some("host_executed_checkpoint_error")
                    && response.response.checkpoint_error.is_some()
        ));

        let ask_user_request = OutputMessage::ControlRequest {
            request_id: "question-1".to_string(),
            request: CoreControlRequest::AskUser {
                tool_use_id: Some("question-call".to_string()),
                question: "Choose a branch".to_string(),
                options: vec![AskUserOption {
                    label: "main".to_string(),
                    description: Some("Use the default branch".to_string()),
                }],
                allow_free_text: true,
                multi_select: false,
            },
        };
        assert_eq!(
            serde_json::to_value(ask_user_request).unwrap(),
            corpus["gateway_brokered_v3"]["ask_user_request"]
        );

        let ask_user_answer: InputMessage =
            serde_json::from_value(corpus["gateway_brokered_v3"]["ask_user_answer"].clone())
                .unwrap();
        assert!(matches!(
            ask_user_answer,
            InputMessage::ControlResponse { response }
                if response.request_id == "question-1"
                    && response.response.answer.as_deref() == Some("main")
        ));
    }

    #[test]
    fn serialize_initialize_version_error() {
        let msg = OutputMessage::initialize_version_error("init-1", 9);
        let v = serde_json::to_value(msg).unwrap();
        assert_eq!(v["response"]["subtype"], "error");
        assert_eq!(v["response"]["response"]["subtype"], "initialize");
        assert_eq!(
            v["response"]["response"]["protocol_version"],
            CONTROL_PROTOCOL_VERSION
        );
        assert!(v["response"]["response"]["capabilities"].is_null());
        assert!(v["response"]["response"]["error"]
            .as_str()
            .unwrap()
            .contains("unsupported control protocol version 9"));
    }

    #[test]
    fn serialize_initialize_success_shell_evidence_capability() {
        let msg = OutputMessage::initialize_success("init-1", true);
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["response"]["response"]["capabilities"]["can_handle_shell_evidence_tool"],
            true
        );
    }

    #[test]
    fn serialize_assistant_text() {
        let msg = OutputMessage::assistant_text("sess-1", "Hello!");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "assistant");
        assert_eq!(v["session_id"], "sess-1");
        assert_eq!(v["message"]["content"][0]["type"], "text");
        assert_eq!(v["message"]["content"][0]["text"], "Hello!");
    }

    #[test]
    fn serialize_can_use_tool() {
        let msg = OutputMessage::can_use_tool(
            "req-1",
            "Bash",
            serde_json::json!({"command": "echo hello"}),
            "toolu_001",
            false,
        );
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "req-1");
        assert_eq!(v["request"]["subtype"], "can_use_tool");
        assert_eq!(v["request"]["tool_name"], "Bash");
        assert_eq!(v["request"]["input"]["command"], "echo hello");
        assert_eq!(v["request"]["tool_use_id"], "toolu_001");
    }

    #[test]
    fn serialize_shell_evidence_read_output() {
        let msg = OutputMessage::shell_evidence_read_output(
            "evidence-1",
            "toolu_abc",
            "terminal-output://raw-session-a1b2/cmd-1",
            "tail",
            120,
            true,
        );
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "evidence-1");
        assert_eq!(v["request"]["subtype"], "shell_evidence");
        assert_eq!(v["request"]["tool_use_id"], "toolu_abc");
        assert_eq!(v["request"]["action"], "read_output");
        assert_eq!(
            v["request"]["output_id"],
            "terminal-output://raw-session-a1b2/cmd-1"
        );
        assert_eq!(v["request"]["direction"], "tail");
        assert_eq!(v["request"]["lines"], 120);
        assert_eq!(v["request"]["bypass_recent_filter"], true);
    }

    #[test]
    fn serialize_shell_evidence_list_commands() {
        let msg = OutputMessage::shell_evidence_list_commands("evidence-1", "toolu_abc", 20, None);
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "evidence-1");
        assert_eq!(v["request"]["subtype"], "shell_evidence");
        assert_eq!(v["request"]["tool_use_id"], "toolu_abc");
        assert_eq!(v["request"]["action"], "list_commands");
        assert_eq!(v["request"]["limit"], 20);
        assert!(v["request"].get("output_id").is_none());
        assert!(v["request"].get("direction").is_none());
        assert!(v["request"].get("lines").is_none());
    }

    #[test]
    fn can_use_tool_parseable_by_cosh_shell_format() {
        // Verify our output matches the format cosh-shell's parse_control_request() expects
        let msg = OutputMessage::can_use_tool(
            "mock-req-001",
            "Bash",
            serde_json::json!({"command": "echo hello"}),
            "toolu_mock001",
            false,
        );
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();

        // cosh-shell checks: v["type"] == "control_request"
        assert_eq!(v.get("type").unwrap().as_str().unwrap(), "control_request");
        // cosh-shell checks: v["request"]["subtype"]
        assert_eq!(
            v.get("request")
                .unwrap()
                .get("subtype")
                .unwrap()
                .as_str()
                .unwrap(),
            "can_use_tool"
        );
        // cosh-shell checks: v["request_id"]
        assert!(v.get("request_id").is_some());
        // cosh-shell checks: v["request"]["tool_name"]
        assert!(v.get("request").unwrap().get("tool_name").is_some());
        // cosh-shell checks: v["request"]["input"]
        assert!(v.get("request").unwrap().get("input").is_some());
        // cosh-shell checks: v["request"]["tool_use_id"]
        assert!(v.get("request").unwrap().get("tool_use_id").is_some());
    }

    #[test]
    fn can_use_tool_carries_real_optional_audit_reference() {
        let msg = OutputMessage::can_use_tool_with_audit_ref(
            "req-1",
            "Bash",
            serde_json::json!({"command": "echo ok"}),
            "toolu-1",
            false,
            Some("audit-event-1".to_string()),
        );
        let value = serde_json::to_value(msg).unwrap();
        assert_eq!(value["request"]["audit_ref"], "audit-event-1");
    }

    #[test]
    fn serialize_result_success() {
        let msg = OutputMessage::result_success("sess-1", "Done");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["subtype"], "success");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["result"], "Done");
        assert_eq!(v["session_id"], "sess-1");
    }

    #[test]
    fn serialize_result_error() {
        let msg = OutputMessage::result_error("sess-1", "cancelled");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["is_error"], true);
        assert!(v.get("session_error_code").is_none());
    }

    #[test]
    fn serialize_max_turns_result_error() {
        let msg = OutputMessage::max_turns_result_error("sess-1", "turn limit", 50);
        let value = serde_json::to_value(msg).unwrap();

        assert_eq!(value["type"], "result");
        assert_eq!(value["error_code"], "max_turns");
        assert_eq!(value["max_turns"], 50);
    }

    #[test]
    fn serialize_structured_session_result_error() {
        let msg =
            OutputMessage::session_result_error("sess-1", "session missing", "not_found", "load");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["is_error"], true);
        assert_eq!(v["session_error_code"], "not_found");
        assert_eq!(v["session_error_phase"], "load");
    }

    #[test]
    fn serialize_stream_text_delta() {
        let msg = OutputMessage::stream_text_delta(0, "hello");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "stream_event");
        assert_eq!(v["event"]["type"], "content_block_delta");
        assert_eq!(v["event"]["index"], 0);
        assert_eq!(v["event"]["delta"]["type"], "text_delta");
        assert_eq!(v["event"]["delta"]["text"], "hello");
    }

    #[test]
    fn serialize_stream_tool_use_start() {
        let msg = OutputMessage::stream_tool_use_start(1, "call_1", "shell");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "stream_event");
        assert_eq!(v["event"]["type"], "content_block_start");
        assert_eq!(v["event"]["index"], 1);
        assert_eq!(v["event"]["content_block"]["type"], "tool_use");
        assert_eq!(v["event"]["content_block"]["id"], "call_1");
        assert_eq!(v["event"]["content_block"]["name"], "shell");
    }

    #[test]
    fn serialize_stream_message_start_stop() {
        let start = OutputMessage::stream_message_start();
        let json = serde_json::to_string(&start).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "stream_event");
        assert_eq!(v["event"]["type"], "message_start");

        let stop = OutputMessage::stream_message_stop();
        let json = serde_json::to_string(&stop).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["event"]["type"], "message_stop");
    }

    #[test]
    fn stream_text_delta_matches_cosh_shell_parser_path() {
        // ClaudeStreamParser extracts text from: value.pointer("/event/delta/text")
        let msg = OutputMessage::stream_text_delta(0, "check...");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        let text = v.pointer("/event/delta/text").and_then(|t| t.as_str());
        assert_eq!(text, Some("check..."));
    }

    #[test]
    fn parse_interrupt_request() {
        let json =
            r#"{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}"#;
        let msg: InputMessage = serde_json::from_str(json).unwrap();
        match msg {
            InputMessage::ControlRequest { request, .. } => {
                assert!(matches!(request, ShellControlRequest::Interrupt));
            }
            _ => panic!("expected ControlRequest"),
        }
    }

    #[test]
    fn parse_shutdown_request() {
        let json =
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#;
        let msg: InputMessage = serde_json::from_str(json).unwrap();
        match msg {
            InputMessage::ControlRequest { request, .. } => {
                assert!(matches!(request, ShellControlRequest::Shutdown));
            }
            _ => panic!("expected ControlRequest"),
        }
    }

    #[test]
    fn serialize_stream_thinking_delta() {
        let msg = OutputMessage::stream_thinking_delta(0, "Let me think...");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "stream_event");
        assert_eq!(v["event"]["type"], "content_block_delta");
        assert_eq!(v["event"]["index"], 0);
        assert_eq!(v["event"]["delta"]["type"], "thinking_delta");
        assert_eq!(v["event"]["delta"]["thinking"], "Let me think...");
    }

    #[test]
    fn thinking_delta_matches_cosh_shell_parser_path() {
        let msg = OutputMessage::stream_thinking_delta(0, "reasoning...");
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        let thinking = v.pointer("/event/delta/thinking").and_then(|t| t.as_str());
        assert_eq!(thinking, Some("reasoning..."));
    }

    #[test]
    fn serialize_stream_thinking_start() {
        let msg = OutputMessage::stream_thinking_start(0);
        let json = serde_json::to_string(&msg).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["event"]["type"], "content_block_start");
        assert_eq!(v["event"]["content_block"]["type"], "thinking");
    }
}
