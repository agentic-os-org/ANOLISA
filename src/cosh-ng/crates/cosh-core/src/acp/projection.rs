//! Maps kernel JSONL output onto ACP v1 session updates.

use std::collections::HashMap;

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallContent,
    ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::Value;

use crate::core::AgentTurnOutcome;
use crate::settlement::TurnFailure;

/// Accumulated tool arguments are bounded so a model that streams a very large
/// argument blob cannot grow this adapter without limit.
const MAX_TOOL_INPUT_BYTES: usize = 256 * 1024;

/// What one kernel output line means for the ACP peer.
pub(crate) enum Projected {
    Update(Box<SessionUpdate>),
    /// The kernel asked the host to authorize a tool call.
    Permission(PermissionAsk),
    /// The kernel asked the host for a decision this entry point cannot serve.
    UnsupportedInteraction,
    Ignored,
}

/// A kernel approval request, kept as wire-neutral fields so the adapter owns
/// the ACP option set and the response framing.
pub(crate) struct PermissionAsk {
    pub request_id: String,
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: Value,
    /// Trusted mandatory-ask flag from the kernel's final-action classification.
    pub hook_requires_approval: bool,
}

/// A tool call whose arguments are still streaming.
struct ToolDraft {
    id: String,
    name: String,
    json: String,
    truncated: bool,
}

/// Projects kernel output lines, keeping the per-block tool argument state.
///
/// The kernel streams tool calls as `content_block_start` / `input_json_delta`
/// / `content_block_stop`; it never puts `tool_use` in an assistant snapshot.
pub(crate) struct Projector {
    tools: HashMap<u32, ToolDraft>,
    /// Whether the current model step already streamed its text. The kernel
    /// repeats that text in an assistant snapshot, but also uses snapshots for
    /// messages that were never streamed, such as a hook refusal.
    text_streamed: bool,
}

impl Projector {
    pub(crate) fn new() -> Self {
        Self {
            tools: HashMap::new(),
            text_streamed: false,
        }
    }

    pub(crate) fn project(&mut self, value: &Value) -> Vec<Projected> {
        match value.get("type").and_then(Value::as_str) {
            Some("control_request") => vec![project_control_request(value)],
            Some("stream_event") => self.project_stream_event(value),
            Some("assistant") => self.project_assistant(value),
            Some("user") => project_user(value),
            _ => Vec::new(),
        }
    }

    fn project_stream_event(&mut self, value: &Value) -> Vec<Projected> {
        let Some(event) = value.get("event") else {
            return Vec::new();
        };
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                self.text_streamed = false;
                Vec::new()
            }
            Some("content_block_start") => self.start_block(index, event),
            Some("content_block_delta") => self.delta(index, event),
            Some("content_block_stop") => self.stop_block(index),
            _ => Vec::new(),
        }
    }

    fn start_block(&mut self, index: u32, event: &Value) -> Vec<Projected> {
        let Some(block) = event.get("content_block") else {
            return Vec::new();
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            return Vec::new();
        }
        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.tools.insert(
            index,
            ToolDraft {
                id: id.to_string(),
                name: name.to_string(),
                json: String::new(),
                truncated: false,
            },
        );
        vec![Projected::Update(Box::new(SessionUpdate::ToolCall(
            ToolCall::new(ToolCallId::new(id), name)
                .kind(tool_kind(name))
                .status(ToolCallStatus::Pending),
        )))]
    }

    fn delta(&mut self, index: u32, event: &Value) -> Vec<Projected> {
        let Some(delta) = event.get("delta") else {
            return Vec::new();
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("input_json_delta") => {
                if let Some(draft) = self.tools.get_mut(&index) {
                    if let Some(part) = delta.get("partial_json").and_then(Value::as_str) {
                        if draft.json.len().saturating_add(part.len()) > MAX_TOOL_INPUT_BYTES {
                            draft.truncated = true;
                        } else if !draft.truncated {
                            draft.json.push_str(part);
                        }
                    }
                }
                Vec::new()
            }
            Some("text_delta") => {
                self.text_streamed = true;
                vec![chunk(delta, "text", false)]
            }
            Some("thinking_delta") => vec![chunk(delta, "thinking", true)],
            _ => Vec::new(),
        }
    }

    fn project_assistant(&mut self, value: &Value) -> Vec<Projected> {
        let streamed = std::mem::replace(&mut self.text_streamed, false);
        if streamed {
            return Vec::new();
        }
        let Some(blocks) = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                let text = block.get("text").and_then(Value::as_str)?;
                if text.is_empty() {
                    return None;
                }
                Some(Projected::Update(Box::new(
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                        TextContent::new(text),
                    ))),
                )))
            })
            .collect()
    }

    fn stop_block(&mut self, index: u32) -> Vec<Projected> {
        let Some(draft) = self.tools.remove(&index) else {
            return Vec::new();
        };
        // A truncated argument blob is not valid JSON, so no raw input is
        // published rather than presenting a partial value as complete.
        if draft.truncated {
            return Vec::new();
        }
        let Ok(input) = serde_json::from_str::<Value>(&draft.json) else {
            return Vec::new();
        };
        vec![Projected::Update(Box::new(SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new(
                ToolCallId::new(draft.id),
                ToolCallUpdateFields::new()
                    .title(title(&draft.name, &input))
                    .raw_input(input),
            ),
        )))]
    }
}

fn chunk(delta: &Value, field: &str, is_thought: bool) -> Projected {
    let Some(text) = delta.get(field).and_then(Value::as_str) else {
        return Projected::Ignored;
    };
    if text.is_empty() {
        return Projected::Ignored;
    }
    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
    Projected::Update(Box::new(if is_thought {
        SessionUpdate::AgentThoughtChunk(chunk)
    } else {
        SessionUpdate::AgentMessageChunk(chunk)
    }))
}

fn project_control_request(value: &Value) -> Projected {
    let Some(request) = value.get("request") else {
        return Projected::UnsupportedInteraction;
    };
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return Projected::UnsupportedInteraction;
    }
    let field = |name: &str| {
        request
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if request_id.is_empty() {
        return Projected::UnsupportedInteraction;
    }
    // The kernel is the identified trusted legacy source: it legally omits the
    // flag when false. A present but non-boolean value is malformed, not an
    // omission, so it fails closed to a mandatory human decision.
    let hook_requires_approval = match request.get("hook_requires_approval") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => true,
    };
    Projected::Permission(PermissionAsk {
        request_id,
        tool_use_id: field("tool_use_id"),
        tool_name: field("tool_name"),
        input: request.get("input").cloned().unwrap_or(Value::Null),
        hook_requires_approval,
    })
}

fn project_user(value: &Value) -> Vec<Projected> {
    let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| {
            let id = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let failed = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = block
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let status = if failed {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            // The event line cap upstream already bounds this text, so it is
            // forwarded whole instead of being truncated a second time.
            let content = vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(
                text,
            )))];
            Projected::Update(Box::new(SessionUpdate::ToolCallUpdate(
                ToolCallUpdate::new(
                    ToolCallId::new(id),
                    ToolCallUpdateFields::new()
                        .status(status)
                        .content(content)
                        .raw_output(block.clone()),
                ),
            )))
        })
        .collect()
}

/// Maps a kernel tool name onto the standard tool category.
///
/// Unknown names stay `Other` rather than being guessed from their input.
pub(super) fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read_file" | "read_many_files" | "list_directory" => ToolKind::Read,
        "grep" | "glob" => ToolKind::Search,
        "write_file" | "edit" => ToolKind::Edit,
        "shell" => ToolKind::Execute,
        "web_fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

/// Builds a human-readable title from the tool name and its subject.
pub(super) fn title(name: &str, input: &Value) -> String {
    let subject = input
        .get("path")
        .or_else(|| input.get("pattern"))
        .or_else(|| input.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if subject.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {subject}")
    }
}

/// Maps the kernel's typed turn terminal onto the standard stop reason.
pub(crate) fn stop_reason(outcome: AgentTurnOutcome) -> StopReason {
    match outcome {
        AgentTurnOutcome::Completed => StopReason::EndTurn,
        AgentTurnOutcome::Interrupted => StopReason::Cancelled,
        AgentTurnOutcome::MaxTurns { .. } => StopReason::MaxTurnRequests,
    }
}

/// Projects the shared settlement result onto the standard prompt terminal.
///
/// A refused commit is never a successful stop reason, and a delivered
/// cancellation cannot mask it: the peer would be told a turn ended cleanly
/// while its transcript was not durable. `MaxTurns` stays a standard stop reason
/// here even though the legacy JSONL transport reports it as a turn error.
pub(crate) fn prompt_terminal(
    settled: Result<AgentTurnOutcome, TurnFailure>,
    cancelled: bool,
) -> Result<StopReason, String> {
    match settled {
        Ok(outcome) => Ok(stop_reason(outcome)),
        Err(failure) if failure.error_code == Some("max_turns") => Ok(StopReason::MaxTurnRequests),
        Err(failure) if cancelled && failure.session_error_code.is_none() => {
            Ok(StopReason::Cancelled)
        }
        Err(failure) => Err(failure.message),
    }
}
