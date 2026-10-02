use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::tools::{is_shell_tool_name, known_provider_tool, KnownProviderTool};
use crate::types::{AgentEvent, QuestionSelectionMode};

mod auth;
mod serialization;

use auth::parse_auth_provider;
pub(crate) use serialization::serialize_approval_receipt;
pub use serialization::{
    serialize_answer, serialize_auth_response, serialize_claude_allow, serialize_co_allow,
    serialize_cosh_core_initialize, serialize_deny, serialize_host_executed_shell_result,
    serialize_initialize, serialize_shell_evidence_result, serialize_user_message,
    HostExecutedInputWait, HostExecutedShellMetadata, HostExecutedShellResult,
};
pub(crate) use serialization::{
    serialize_cosh_core_user_message, serialize_initialize_without_session_start,
};

const SHELL_HANDOFF_EVIDENCE_PROMPT_MARKER: &str = "ShellCommandCompleted";
const SHELL_HANDOFF_CONTINUATION_HINT: &str =
    "analysis-only continuation after foreground shell handoff";
pub const PENDING_CONTROL_TOOL_CALL_GRACE: Duration = Duration::from_millis(200);
const CONSUMED_CONTROL_TOOL_ID_TTL: Duration = Duration::from_secs(30);
pub const ANALYSIS_ONLY_SHELL_DENY_MESSAGE: &str = "The foreground shell command already completed and its output was injected. Summarize the existing shell evidence or ask the user to start a new request before running another shell command.";
/// Exact control protocol version emitted and accepted by this shell.
pub(crate) const CONTROL_PROTOCOL_VERSION: u32 = 1;

pub enum ControlRequest {
    Initialize {
        request_id: String,
    },
    CanUseTool {
        request_id: String,
        tool_name: String,
        tool_input: Value,
        tool_use_id: String,
        hook_requires_approval: bool,
        audit_ref: Option<String>,
    },
    AskUser {
        request_id: String,
        question: String,
        options: Vec<String>,
        allow_free_text: bool,
        selection_mode: QuestionSelectionMode,
    },
    AuthRequired {
        request_id: String,
        reason: String,
        error_message: Option<String>,
        providers: Vec<AuthProviderInfo>,
    },
    ShellEvidence {
        request_id: String,
        tool_use_id: String,
        action: ShellEvidenceAction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellOutputDirection {
    Head,
    Tail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellEvidenceAction {
    ListCommands {
        limit: u16,
        cursor: Option<String>,
    },
    ReadOutput {
        output_id: String,
        direction: ShellOutputDirection,
        lines: u16,
        bypass_recent_filter: bool,
    },
}

impl ShellEvidenceAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ListCommands { .. } => "list_commands",
            Self::ReadOutput { .. } => "read_output",
        }
    }
}

impl ShellOutputDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Tail => "tail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthProviderInfo {
    pub id: String,
    pub label: String,
    /// Short guidance shown under the provider label.
    #[serde(default)]
    pub description: Option<String>,
    /// Simplified Chinese guidance supplied by the provider registry.
    #[serde(default)]
    pub description_zh_cn: Option<String>,
    /// Fixed endpoint used to recognize preset-backed saved providers.
    #[serde(default)]
    pub builtin_base_url: Option<String>,
    pub fields: Vec<AuthFieldInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthFieldInfo {
    pub name: String,
    pub label: String,
    pub hint: Option<String>,
    pub secret: bool,
    pub required: bool,
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthResponse {
    pub request_id: String,
    pub provider_id: String,
    pub provider_type: Option<String>,
    pub values: HashMap<String, String>,
    pub persist: bool,
}

/// Non-exhaustive: capability flags grow with the control protocol (e.g. the
/// #1940 `can_handle_approval_receipt`), and out-of-crate code must stay
/// source-compatible across those additions — construct via `default()` and
/// set fields, or read them; only this crate builds it exhaustively.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlProtocolCapabilities {
    pub provider_initialize_seen: bool,
    /// Negotiated exact version, or `None` for a legacy unversioned peer.
    pub protocol_version: Option<u32>,
    pub can_handle_can_use_tool: bool,
    pub can_handle_host_executed_shell_tool_result: bool,
    pub can_handle_shell_evidence_tool: bool,
    /// #1940 receipt protocol: only a provider that announces this capability
    /// receives `approval_receipt` lines. Without it the receipt is skipped —
    /// a provider that does not understand receipts would misread the line as
    /// an ordinary response, and losing the receipt only means the core keeps
    /// its last-resort approval guard armed (the designed degradation).
    pub can_handle_approval_receipt: bool,
}

/// Shared writer-thread check for the #1940 receipt gate: a poisoned or
/// unparsed capability set reads as "not capable", so the receipt is skipped
/// rather than risking an unintelligible line on the provider's stdin.
pub(crate) fn receipt_capable(
    capabilities: &std::sync::Arc<std::sync::Mutex<ControlProtocolCapabilities>>,
) -> bool {
    capabilities
        .lock()
        .map(|caps| caps.can_handle_approval_receipt)
        .unwrap_or(false)
}

#[derive(Debug, Default)]
pub struct PendingControlProtocolToolCall {
    pending_shell_tool_calls: Vec<PendingShellToolCall>,
    held_events: Vec<AgentEvent>,
    consumed_control_tool_ids: Vec<ConsumedControlToolId>,
}

#[derive(Debug)]
struct PendingShellToolCall {
    event: AgentEvent,
    staged_at: Instant,
}

#[derive(Debug)]
struct ConsumedControlToolId {
    run_id: String,
    tool_use_id: String,
    consumed_at: Instant,
}

impl PendingControlProtocolToolCall {
    pub fn take_matching_control_shell(&mut self, run_id: &str, tool_use_id: &str) -> bool {
        self.take_matching_control_tool_call(run_id, tool_use_id)
    }

    pub fn take_matching_control_tool_call(&mut self, run_id: &str, tool_use_id: &str) -> bool {
        self.record_consumed_control_tool_id(run_id, tool_use_id);
        if let Some(index) = self.pending_shell_tool_call_index(tool_use_id) {
            self.pending_shell_tool_calls.remove(index);
            if self.pending_shell_tool_calls.is_empty() {
                self.held_events.clear();
            }
            true
        } else {
            false
        }
    }

    pub fn stage_or_emit(&mut self, event: AgentEvent) -> Vec<AgentEvent> {
        self.prune_consumed_control_tool_ids(Instant::now());
        if let Some(tool_id) = provider_tool_call_id(&event) {
            if event_run_id(&event)
                .is_some_and(|run_id| self.is_consumed_control_tool_id(run_id, tool_id))
            {
                return Vec::new();
            }
        }

        if matches!(&event, AgentEvent::ToolCall { tool_id: Some(_), name, .. } if is_control_backed_tool_name(name))
        {
            self.pending_shell_tool_calls.push(PendingShellToolCall {
                event,
                staged_at: Instant::now(),
            });
            return Vec::new();
        }

        if let Some(tool_id) = provider_tool_result_id(&event) {
            if let Some(run_id) = event_run_id(&event) {
                if self.is_consumed_control_tool_id(run_id, tool_id) {
                    if matches!(event, AgentEvent::ToolCompleted { .. }) {
                        self.remove_consumed_control_tool_id(run_id, tool_id);
                    }
                    return Vec::new();
                }
            }
            let mut events = self.take_pending_shell_tool_call(tool_id);
            events.push(event);
            if self.pending_shell_tool_calls.is_empty() {
                events.append(&mut self.held_events);
            }
            return events;
        }

        // HookNotifications must never be held - they need to be available in
        // pending_hook_notifications before the corresponding ToolPermissionRequest arrives.
        if matches!(&event, AgentEvent::HookNotification { .. }) {
            return vec![event];
        }

        if !self.pending_shell_tool_calls.is_empty() {
            if is_terminal_agent_event(&event) {
                self.pending_shell_tool_calls.clear();
                let mut events = std::mem::take(&mut self.held_events);
                events.push(event);
                return events;
            }
            self.held_events.push(event);
            return Vec::new();
        }

        let mut events = std::mem::take(&mut self.held_events);
        events.push(event);
        events
    }

    pub fn flush(&mut self) -> Vec<AgentEvent> {
        let mut events = self
            .pending_shell_tool_calls
            .drain(..)
            .map(|pending| pending.event)
            .collect::<Vec<_>>();
        events.append(&mut self.held_events);
        self.consumed_control_tool_ids.clear();
        events
    }

    fn record_consumed_control_tool_id(&mut self, run_id: &str, tool_use_id: &str) {
        self.prune_consumed_control_tool_ids(Instant::now());
        if self
            .consumed_control_tool_ids
            .iter()
            .any(|entry| entry.run_id == run_id && entry.tool_use_id == tool_use_id)
        {
            return;
        }
        self.consumed_control_tool_ids.push(ConsumedControlToolId {
            run_id: run_id.to_string(),
            tool_use_id: tool_use_id.to_string(),
            consumed_at: Instant::now(),
        });
    }

    fn prune_consumed_control_tool_ids(&mut self, now: Instant) {
        self.consumed_control_tool_ids.retain(|entry| {
            now.saturating_duration_since(entry.consumed_at) < CONSUMED_CONTROL_TOOL_ID_TTL
        });
    }

    fn is_consumed_control_tool_id(&self, run_id: &str, tool_use_id: &str) -> bool {
        self.consumed_control_tool_ids
            .iter()
            .any(|entry| entry.run_id == run_id && entry.tool_use_id == tool_use_id)
    }

    fn remove_consumed_control_tool_id(&mut self, run_id: &str, tool_use_id: &str) {
        self.consumed_control_tool_ids
            .retain(|entry| entry.run_id != run_id || entry.tool_use_id != tool_use_id);
    }

    #[cfg(test)]
    pub(crate) fn expire_consumed_control_tool_ids_for_test(&mut self) {
        for entry in &mut self.consumed_control_tool_ids {
            entry.consumed_at = Instant::now() - CONSUMED_CONTROL_TOOL_ID_TTL;
        }
    }

    pub fn flush_stalled(&mut self, grace: Duration) -> Vec<AgentEvent> {
        let now = Instant::now();
        let count = self
            .pending_shell_tool_calls
            .iter()
            .take_while(|pending| now.saturating_duration_since(pending.staged_at) >= grace)
            .count();
        if count == 0 {
            return Vec::new();
        }
        let mut events = self
            .pending_shell_tool_calls
            .drain(..count)
            .map(|pending| pending.event)
            .collect::<Vec<_>>();
        if self.pending_shell_tool_calls.is_empty() {
            events.append(&mut self.held_events);
        }
        events
    }

    fn pending_shell_tool_call_index(&self, tool_use_id: &str) -> Option<usize> {
        self.pending_shell_tool_calls
            .iter()
            .position(|pending| matches!(&pending.event, AgentEvent::ToolCall { tool_id: Some(tool_id), .. } if tool_id == tool_use_id))
    }

    fn take_pending_shell_tool_call(&mut self, tool_use_id: &str) -> Vec<AgentEvent> {
        let Some(index) = self.pending_shell_tool_call_index(tool_use_id) else {
            return Vec::new();
        };
        vec![self.pending_shell_tool_calls.remove(index).event]
    }
}

fn event_run_id(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::ToolCall { run_id, .. }
        | AgentEvent::ToolOutputDelta { run_id, .. }
        | AgentEvent::ToolCompleted { run_id, .. } => Some(run_id),
        _ => None,
    }
}

fn provider_tool_call_id(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::ToolCall {
            tool_id: Some(tool_id),
            ..
        } => Some(tool_id),
        _ => None,
    }
}

fn provider_tool_result_id(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::ToolOutputDelta { tool_id, .. } | AgentEvent::ToolCompleted { tool_id, .. } => {
            Some(tool_id)
        }
        _ => None,
    }
}

pub(crate) fn is_control_backed_tool_name(name: &str) -> bool {
    known_provider_tool(name).is_some_and(KnownProviderTool::is_control_backed)
}

fn is_terminal_agent_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. }
    )
}

pub fn parse_control_request(line: &str) -> Option<ControlRequest> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "control_request" {
        return None;
    }
    let request = v.get("request")?;
    let subtype = request.get("subtype")?.as_str()?;
    let request_id = v.get("request_id")?.as_str()?.to_string();

    match subtype {
        "initialize" => Some(ControlRequest::Initialize { request_id }),
        "can_use_tool" => {
            let tool_name = request.get("tool_name")?.as_str()?.to_string();
            let tool_input = request.get("input")?.clone();
            let tool_use_id = request.get("tool_use_id")?.as_str()?.to_string();
            let hook_requires_approval = request
                .get("hook_requires_approval")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let audit_ref = request
                .get("audit_ref")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Some(ControlRequest::CanUseTool {
                request_id,
                tool_name,
                tool_input,
                tool_use_id,
                hook_requires_approval,
                audit_ref,
            })
        }
        "ask_user" => {
            let question = request.get("question")?.as_str()?.to_string();
            let options = request
                .get("options")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            item.get("label")
                                .and_then(|label| label.as_str())
                                .or_else(|| item.as_str())
                                .map(str::to_string)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let allow_free_text = request
                .get("allow_free_text")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let selection_mode = if request
                .get("multi_select")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                QuestionSelectionMode::Multiple
            } else {
                QuestionSelectionMode::Single
            };
            Some(ControlRequest::AskUser {
                request_id,
                question,
                options,
                allow_free_text,
                selection_mode,
            })
        }
        "auth_required" => {
            let reason = request
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("not_configured")
                .to_string();
            let error_message = request
                .get("error_message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let providers = request
                .get("providers")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(parse_auth_provider).collect())
                .unwrap_or_default();
            Some(ControlRequest::AuthRequired {
                request_id,
                reason,
                error_message,
                providers,
            })
        }
        "shell_evidence" => {
            let tool_use_id = request.get("tool_use_id")?.as_str()?.to_string();
            let action = match request.get("action")?.as_str()? {
                "list_commands" => {
                    if request.get("output_id").is_some()
                        || request.get("lines").is_some()
                        || request.get("bypass_recent_filter").is_some()
                    {
                        return None;
                    }
                    ShellEvidenceAction::ListCommands {
                        limit: parse_shell_evidence_list_limit(request)?,
                        cursor: parse_shell_evidence_list_cursor(request)?,
                    }
                }
                "read_output" => {
                    let output_id = request.get("output_id")?.as_str()?.to_string();
                    if !output_id.starts_with("terminal-output://") {
                        return None;
                    }
                    let direction = parse_shell_output_direction(request)?;
                    let lines = parse_shell_output_lines(request)?;
                    let bypass_recent_filter = parse_bypass_recent_filter(request)?;
                    ShellEvidenceAction::ReadOutput {
                        output_id,
                        direction,
                        lines,
                        bypass_recent_filter,
                    }
                }
                _ => return None,
            };
            Some(ControlRequest::ShellEvidence {
                request_id,
                tool_use_id,
                action,
            })
        }
        _ => None,
    }
}

fn parse_shell_output_direction(request: &Value) -> Option<ShellOutputDirection> {
    let direction = match request.get("direction") {
        Some(value) => value.as_str()?,
        None => "tail",
    };
    match direction {
        "head" => Some(ShellOutputDirection::Head),
        "tail" => Some(ShellOutputDirection::Tail),
        _ => None,
    }
}

fn parse_shell_output_lines(request: &Value) -> Option<u16> {
    let lines = match request.get("lines") {
        Some(value) => value.as_u64()?,
        None => 120,
    };
    if lines == 0 {
        return None;
    }
    Some(lines.min(300) as u16)
}

fn parse_bypass_recent_filter(request: &Value) -> Option<bool> {
    match request.get("bypass_recent_filter") {
        Some(value) => value.as_bool(),
        None => Some(false),
    }
}

fn parse_shell_evidence_list_limit(request: &Value) -> Option<u16> {
    let limit = match request.get("limit") {
        Some(value) => value.as_u64()?,
        None => 20,
    };
    if limit == 0 {
        return None;
    }
    Some(limit.min(100) as u16)
}

fn parse_shell_evidence_list_cursor(request: &Value) -> Option<Option<String>> {
    match request.get("cursor") {
        Some(Value::Null) | None => Some(None),
        Some(value) => value.as_str().map(|cursor| Some(cursor.to_string())),
    }
}

pub fn should_deny_shell_request_for_analysis_continuation(prompt: &str, tool_name: &str) -> bool {
    prompt.contains(SHELL_HANDOFF_EVIDENCE_PROMPT_MARKER)
        && prompt.contains(SHELL_HANDOFF_CONTINUATION_HINT)
        && is_shell_tool_name(tool_name)
}

pub(crate) fn parse_initialize_response(
    line: &str,
    expected_request_id: &str,
) -> Option<Result<ControlProtocolCapabilities, String>> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    parse_initialize_response_value(&v, expected_request_id)
}

pub(crate) fn parse_initialize_response_value(
    v: &Value,
    expected_request_id: &str,
) -> Option<Result<ControlProtocolCapabilities, String>> {
    if v.get("type")?.as_str()? != "control_response" {
        return None;
    }
    let envelope = v.get("response")?;
    let response = envelope.get("response")?;
    if response.get("subtype")?.as_str()? != "initialize" {
        return None;
    }
    let Some(request_id) = envelope.get("request_id").and_then(Value::as_str) else {
        return Some(Err(
            "initialize response is missing a valid request id".to_string()
        ));
    };
    if request_id != expected_request_id {
        return Some(Err(format!(
            "initialize response request id {request_id:?} does not match {expected_request_id:?}"
        )));
    }
    let protocol_version = match response.get("protocol_version") {
        None => None,
        Some(value) => match value
            .as_u64()
            .and_then(|version| u32::try_from(version).ok())
        {
            Some(version) => Some(version),
            None => {
                return Some(Err(
                    "initialize response has an invalid control protocol version".to_string(),
                ));
            }
        },
    };
    if let Some(version) = protocol_version {
        if version != CONTROL_PROTOCOL_VERSION {
            return Some(Err(format!(
                "unsupported control protocol version {version}; expected exact version {CONTROL_PROTOCOL_VERSION}"
            )));
        }
    }
    if envelope.get("subtype").and_then(Value::as_str) != Some("success") {
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("provider rejected control protocol initialization");
        return Some(Err(message.to_string()));
    }
    let capabilities = response.get("capabilities");
    Some(Ok(ControlProtocolCapabilities {
        provider_initialize_seen: true,
        protocol_version,
        can_handle_can_use_tool: bool_capability(capabilities, "can_handle_can_use_tool"),
        can_handle_host_executed_shell_tool_result: bool_capability(
            capabilities,
            "can_handle_host_executed_shell_tool_result",
        ),
        can_handle_shell_evidence_tool: bool_capability(
            capabilities,
            "can_handle_shell_evidence_tool",
        ),
        can_handle_approval_receipt: bool_capability(capabilities, "can_handle_approval_receipt"),
    }))
}

fn bool_capability(capabilities: Option<&Value>, key: &str) -> bool {
    capabilities
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct ApprovalResponse {
    pub request_id: String,
    pub tool_use_id: Option<String>,
    pub tool_input: Option<Value>,
    pub decision: ApprovalDecision,
}

/// #1940 receipt protocol: everything the shell can send over the approval
/// channel. A `Response` is terminal for its request; a `Receipt` only
/// proves the request reached the shell main thread (so the core can
/// disarm its residual timeout) and never resolves anything.
#[derive(Debug, Clone)]
pub(crate) enum ApprovalChannelMessage {
    Response(ApprovalResponse),
    Receipt { request_id: String },
}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Allow,
    Deny {
        message: String,
    },
    HostExecutedShell {
        result: Box<HostExecutedShellResult>,
    },
    Answer {
        answer: String,
    },
    ShellEvidence {
        result: Box<ShellEvidenceResult>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEvidenceResult {
    pub llm_content: String,
    pub return_display: Option<String>,
    pub metadata: ShellEvidenceMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEvidenceMetadata {
    pub action: String,
    pub scope: Option<String>,
    pub limit: Option<u16>,
    pub next_cursor: Option<String>,
    pub output_id: String,
    pub status: String,
    pub excerpt_status: String,
    pub reason: Option<String>,
    pub direction: String,
    pub lines: u16,
    pub command_count: Option<usize>,
    pub provider_visible_byte_cap: usize,
    pub truncated: bool,
    pub truncated_by_lines: bool,
    pub truncated_by_bytes: bool,
    pub truncation_reason: String,
    pub is_error: bool,
}

pub fn analysis_continuation_shell_deny_response(
    prompt: &str,
    request_id: &str,
    tool_name: &str,
    tool_input: &Value,
    tool_use_id: &str,
) -> Option<ApprovalResponse> {
    if !should_deny_shell_request_for_analysis_continuation(prompt, tool_name) {
        return None;
    }
    Some(ApprovalResponse {
        request_id: request_id.to_string(),
        tool_use_id: Some(tool_use_id.to_string()),
        tool_input: Some(tool_input.clone()),
        decision: ApprovalDecision::Deny {
            message: ANALYSIS_ONLY_SHELL_DENY_MESSAGE.to_string(),
        },
    })
}
