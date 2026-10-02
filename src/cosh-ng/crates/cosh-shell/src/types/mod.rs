use serde::{Deserialize, Serialize};

mod agent_status;
pub mod audit;
mod card;
pub(crate) mod composer;
mod continuation;
pub mod hooks;
mod shell_event_metadata;
mod shell_handoff;

pub(crate) use agent_status::{TOOL_ARGUMENTS_STATUS_PHASE, TOOL_ARGUMENTS_STATUS_PREFIX};
pub(crate) use card::{CardKind, CardModel, InputModel, InputOwner, PermissionCardRequest};
pub(crate) use continuation::*;

pub(crate) use hooks::BuiltinFactRecord;
pub use hooks::{
    BuiltinFindingFacts, EvaluatedHookFinding, FindingSeverity, HighMemoryProcessFacts,
    HookFinding, HookProvenance, MemoryPressureFacts, MetricsConfidence, ProcessMemoryFact,
};
pub use shell_event_metadata::{ShellCaptureLifecycle, ShellCaptureMetadata, ShellRoutingMetadata};
pub use shell_handoff::{ImplicitPagerPolicy, ShellHandoffRequest};
pub(crate) use shell_handoff::{
    BOUNDED_HANDOFF_COMMAND, NON_INTERACTIVE_PAGER_PREFIX, SHELL_HANDOFF_UNTRACKED_STATUS,
};

pub const COMMAND_OUTPUT_REF_MAX_BYTES: usize = 1024 * 1024;
pub const SESSION_OUTPUT_REF_MAX_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const PROVIDER_TIMEOUT_ERROR_CODE: &str = "provider_timeout";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellEventKind {
    ShellStarted,
    ShellReady,
    UserInputIntercepted,
    CommandRoutingObserved,
    CommandStarted,
    CommandCompleted,
    CommandFailed,
    ShellExited,
    ComponentFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CommandOrigin {
    UserInteractive,
    UserSendToShell,
    UserAnalysisAction,
    AgentHandoff,
    ProviderTool,
    ShellInternal,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCommandAuditIdentity {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// One-time handoff claim token echoed back by the marker script (#2142).
    /// Lets handoff closure match on identity instead of the possibly
    /// redacted command text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellEvent {
    pub kind: ShellEventKind,
    pub session_id: String,
    pub command_id: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub end_cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub terminal_output_ref: Option<String>,
    pub terminal_output_bytes: Option<u64>,
    pub input: Option<String>,
    pub component: Option<String>,
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_origin: Option<CommandOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_environment_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_identity: Option<ShellCommandAuditIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<ShellRoutingMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<ShellCaptureMetadata>,
}

impl ShellEvent {
    pub fn command_started(
        session_id: impl Into<String>,
        command_id: impl Into<String>,
        command: impl Into<String>,
        cwd: impl Into<String>,
        started_at_ms: u64,
    ) -> Self {
        Self {
            kind: ShellEventKind::CommandStarted,
            session_id: session_id.into(),
            command_id: Some(command_id.into()),
            command: Some(command.into()),
            cwd: Some(cwd.into()),
            end_cwd: None,
            exit_code: None,
            started_at_ms: Some(started_at_ms),
            ended_at_ms: None,
            duration_ms: None,
            terminal_output_ref: None,
            terminal_output_bytes: None,
            input: None,
            component: None,
            message: None,
            command_origin: Some(CommandOrigin::UserInteractive),
            shell_environment_generation: None,
            audit_identity: None,
            routing: None,
            capture: None,
        }
    }

    pub fn command_started_with_origin(
        session_id: impl Into<String>,
        command_id: impl Into<String>,
        command: impl Into<String>,
        cwd: impl Into<String>,
        started_at_ms: u64,
        origin: CommandOrigin,
    ) -> Self {
        let mut event = Self::command_started(session_id, command_id, command, cwd, started_at_ms);
        event.command_origin = Some(origin);
        event
    }

    pub fn command_finished(
        kind: ShellEventKind,
        session_id: impl Into<String>,
        command_id: impl Into<String>,
        exit_code: i32,
        ended_at_ms: u64,
        terminal_output_ref: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            session_id: session_id.into(),
            command_id: Some(command_id.into()),
            command: None,
            cwd: None,
            end_cwd: None,
            exit_code: Some(exit_code),
            started_at_ms: None,
            ended_at_ms: Some(ended_at_ms),
            duration_ms: None,
            terminal_output_ref: Some(terminal_output_ref.into()),
            terminal_output_bytes: Some(0),
            input: None,
            component: None,
            message: None,
            command_origin: None,
            shell_environment_generation: None,
            audit_identity: None,
            routing: None,
            capture: None,
        }
    }

    pub fn user_input_intercepted(session_id: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            kind: ShellEventKind::UserInputIntercepted,
            session_id: session_id.into(),
            command_id: None,
            command: None,
            cwd: None,
            end_cwd: None,
            exit_code: None,
            started_at_ms: None,
            ended_at_ms: None,
            duration_ms: None,
            terminal_output_ref: None,
            terminal_output_bytes: None,
            input: Some(input.into()),
            component: None,
            message: None,
            command_origin: None,
            shell_environment_generation: None,
            audit_identity: None,
            routing: None,
            capture: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRefs {
    pub terminal_output_ref: Option<String>,
    pub terminal_output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEnvironmentSnapshot {
    pub session_id: String,
    pub marker_sequence: u64,
    pub generation: u64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandBlock {
    pub id: String,
    pub session_id: String,
    pub command: String,
    #[serde(default)]
    pub origin: CommandOrigin,
    pub cwd: String,
    pub end_cwd: String,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub duration_ms: u64,
    pub exit_code: i32,
    pub status: CommandStatus,
    pub output: OutputRefs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_environment_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_identity: Option<ShellCommandAuditIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    NonZeroExit,
    CommandNotFound,
    PermissionDenied,
    ServiceFailed,
    MissingOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub command_block_id: String,
    pub kind: FindingKind,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionDecision {
    Suggest,
    AskAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intervention {
    pub id: String,
    pub finding_id: String,
    pub command_block_id: String,
    pub decision: InterventionDecision,
    pub guidance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    AnalysisOnly,
    RecommendOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub(crate) enum AgentContextBinding {
    #[default]
    FreeForm,
    FailedCommand,
    HookConsultation,
    StartupHealthFollowUp,
    SelectedCommand,
    ControlProtocolEvidence,
    ShellHandoffContinuation,
}

pub(crate) const CONTEXT_BINDING_HINT_PREFIX: &str = "__cosh_context_binding=";
pub(crate) const STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT: &str =
    "__cosh_context_binding=startup_health_follow_up";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequest {
    pub id: String,
    pub session_id: String,
    pub command_block: CommandBlock,
    #[serde(default)]
    pub context_blocks: Vec<CommandBlock>,
    #[serde(default)]
    pub context_hints: Vec<String>,
    pub user_input: Option<String>,
    pub findings: Vec<Finding>,
    pub mode: AgentMode,
    pub user_confirmed: bool,
    #[serde(default)]
    pub hook_finding: Option<HookFinding>,
    #[serde(default)]
    pub recommended_skill: Option<String>,
}

pub(crate) fn set_request_context_binding(
    request: &mut AgentRequest,
    binding: AgentContextBinding,
) {
    request
        .context_hints
        .retain(|hint| !hint.starts_with(CONTEXT_BINDING_HINT_PREFIX));
    if let Some(hint) = context_binding_hint(binding) {
        request.context_hints.push(hint.to_string());
    }
}

#[allow(dead_code)]
pub(crate) fn request_context_binding(request: &AgentRequest) -> AgentContextBinding {
    request
        .context_hints
        .iter()
        .find_map(|hint| context_binding_from_hint(hint))
        .unwrap_or_default()
}

fn context_binding_hint(binding: AgentContextBinding) -> Option<&'static str> {
    match binding {
        AgentContextBinding::FreeForm => None,
        AgentContextBinding::FailedCommand => Some("__cosh_context_binding=failed_command"),
        AgentContextBinding::HookConsultation => Some("__cosh_context_binding=hook_consultation"),
        AgentContextBinding::StartupHealthFollowUp => Some(STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT),
        AgentContextBinding::SelectedCommand => Some("__cosh_context_binding=selected_command"),
        AgentContextBinding::ControlProtocolEvidence => {
            Some("__cosh_context_binding=control_protocol_evidence")
        }
        AgentContextBinding::ShellHandoffContinuation => {
            Some("__cosh_context_binding=shell_handoff_continuation")
        }
    }
}

#[allow(dead_code)]
fn context_binding_from_hint(hint: &str) -> Option<AgentContextBinding> {
    let value = hint.strip_prefix(CONTEXT_BINDING_HINT_PREFIX)?;
    match value {
        "failed_command" => Some(AgentContextBinding::FailedCommand),
        "hook_consultation" => Some(AgentContextBinding::HookConsultation),
        "startup_health_follow_up" => Some(AgentContextBinding::StartupHealthFollowUp),
        "selected_command" => Some(AgentContextBinding::SelectedCommand),
        "control_protocol_evidence" => Some(AgentContextBinding::ControlProtocolEvidence),
        "shell_handoff_continuation" => Some(AgentContextBinding::ShellHandoffContinuation),
        _ => None,
    }
}

/// In-band marker for requests whose input the shell-side secret gate
/// flagged sensitive (#2138). `__cosh_` hints never reach provider
/// prompts; durable sinks (personalization activity store) key off it
/// to redact the whole input field instead of trusting sanitizer
/// regexes to re-detect every shell-gate form.
pub(crate) const SENSITIVE_INPUT_HINT: &str = "__cosh_sensitive_input=true";

pub(crate) fn mark_request_sensitive_input(request: &mut AgentRequest) {
    if !request_has_sensitive_input(request) {
        request.context_hints.push(SENSITIVE_INPUT_HINT.to_string());
    }
}

pub(crate) fn request_has_sensitive_input(request: &AgentRequest) -> bool {
    request
        .context_hints
        .iter()
        .any(|hint| hint == SENSITIVE_INPUT_HINT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuestionSelectionMode {
    #[default]
    Single,
    Multiple,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    StatusChanged {
        run_id: String,
        phase: String,
        message: String,
    },
    TextDelta {
        run_id: String,
        text: String,
    },
    Recommendation {
        run_id: String,
        summary: String,
        commands: Vec<String>,
        auto_execute: bool,
    },
    ToolCall {
        run_id: String,
        #[serde(default)]
        tool_id: Option<String>,
        name: String,
        input: String,
    },
    UserQuestion {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_request_id: Option<String>,
        question: String,
        options: Vec<String>,
        allow_free_text: bool,
        #[serde(default)]
        selection_mode: QuestionSelectionMode,
    },
    Action {
        run_id: String,
        command: String,
    },
    ToolPermissionRequest {
        run_id: String,
        request_id: String,
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
        hook_requires_approval: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audit_ref: Option<String>,
    },

    ToolOutputDelta {
        run_id: String,
        tool_id: String,
        stream: String,
        text: String,
    },
    ToolCompleted {
        run_id: String,
        tool_id: String,
        status: String,
    },
    /// The core's machine-readable hook verdict for a tool call (#2156).
    /// Emitted only when the provider-native result carries the
    /// `cosh_hook_verdict` wire marker; the bridge keys rejection semantics
    /// on this event, never on user-controllable result text.
    ToolHookVerdict {
        run_id: String,
        tool_id: String,
        verdict: String,
    },
    AgentCompleted {
        run_id: String,
        summary: String,
    },
    AgentFailed {
        run_id: String,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
    },
    AgentCancelled {
        run_id: String,
        reason: String,
    },
    AuthRequired {
        run_id: String,
        request_id: String,
        reason: String,
        error_message: Option<String>,
        providers: Vec<crate::adapter::AuthProviderInfo>,
    },
    ShellEvidenceRequest {
        run_id: String,
        request_id: String,
        tool_use_id: String,
        action: crate::adapter::ShellEvidenceAction,
    },
    HookNotification {
        run_id: String,
        hook_name: String,
        message: String,
        tool_use_id: Option<String>,
        /// Per-hook decision (allow/ask/block/deny). Only populated by CoshCore adapter;
        /// other adapters leave this as None, preserving their existing behaviour.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoshApprovalMode {
    Recommend,
    #[default]
    Auto,
    Trust,
}

impl CoshApprovalMode {
    /// Parses canonical names and read-only legacy aliases.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "recommend" | "balanced" | "strict" | "suggest" => Some(Self::Recommend),
            "auto" => Some(Self::Auto),
            "trust" => Some(Self::Trust),
            _ => None,
        }
    }

    /// Parses configuration input and falls back to the safest mode.
    pub fn from_config(value: &str) -> Self {
        match Self::parse(value) {
            Some(mode) => mode,
            None => {
                eprintln!("[cosh-shell] Warning: invalid approval mode {value:?}; using recommend");
                Self::Recommend
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Recommend => "recommend",
            Self::Auto => "auto",
            Self::Trust => "trust",
        }
    }

    pub fn uses_control_protocol(self) -> bool {
        matches!(self, Self::Auto | Self::Trust)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceDecision {
    Display,
    Degraded,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GovernancePolicyDecision {
    #[default]
    DisplayOnly,
    NeedsUserApproval,
    ProviderApprovalResponse,
    HostAutoApproved,
    HostDenied,
    HostBlocked,
    AuditOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedEvent {
    pub decision: GovernanceDecision,
    #[serde(default)]
    pub policy_decision: GovernancePolicyDecision,
    pub event: AgentEvent,
    pub reason: String,
    pub display_text: String,
    pub auto_execute: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: String,
    pub subject: String,
    pub decision: GovernanceDecision,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub recommend_only: bool,
    pub permission_callback_available: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            recommend_only: true,
            permission_callback_available: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CoshApprovalMode, ShellEvent};

    #[test]
    fn approval_mode_parses_canonical_and_legacy_names() {
        for value in ["recommend", "balanced", "strict", "suggest"] {
            assert_eq!(
                CoshApprovalMode::parse(value),
                Some(CoshApprovalMode::Recommend)
            );
        }
        assert_eq!(
            CoshApprovalMode::parse("auto"),
            Some(CoshApprovalMode::Auto)
        );
        assert_eq!(
            CoshApprovalMode::parse("trust"),
            Some(CoshApprovalMode::Trust)
        );
        assert_eq!(CoshApprovalMode::parse("unknown"), None);
        assert_eq!(
            CoshApprovalMode::from_config("unknown"),
            CoshApprovalMode::Recommend
        );
    }

    #[test]
    fn shell_event_without_environment_generation_remains_compatible() {
        let event: ShellEvent = serde_json::from_str(
            r#"{"kind":"command_started","session_id":"s1","command_id":"c1","command":"echo ok","cwd":"/tmp","end_cwd":null,"exit_code":null,"started_at_ms":1,"ended_at_ms":null,"duration_ms":null,"terminal_output_ref":null,"terminal_output_bytes":null,"input":null,"component":null,"message":null}"#,
        )
        .expect("legacy shell event");

        assert_eq!(event.shell_environment_generation, None);
        assert!(serde_json::to_value(event)
            .expect("serialize shell event")
            .get("shell_environment_generation")
            .is_none());
    }
}
