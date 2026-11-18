//! Projects standard ACP session updates onto existing Shell agent events.
//!
//! Every update variant produces at least one event: unsupported content is
//! reported as a status fact instead of being silently dropped, so the UI never
//! implies it rendered something this adapter cannot render.

use agent_client_protocol::v1::schema::{
    ContentBlock, SessionUpdate, StopReason, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate,
};

use crate::types::AgentEvent;

/// Maximum plan entries summarized in one status message.
const MAX_PLAN_ENTRIES: usize = 16;

pub(super) fn project_update(run_id: &str, update: SessionUpdate) -> Vec<AgentEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(text) => vec![AgentEvent::TextDelta {
                run_id: run_id.to_string(),
                text: text.text,
            }],
            other => vec![status_event(
                run_id,
                "unsupported_content",
                format!("agent message carried {}", content_kind(&other)),
            )],
        },
        SessionUpdate::AgentThoughtChunk(chunk) => match chunk.content {
            ContentBlock::Text(text) => vec![status_event(run_id, "thinking", text.text)],
            other => vec![status_event(
                run_id,
                "unsupported_content",
                format!("agent thought carried {}", content_kind(&other)),
            )],
        },
        SessionUpdate::UserMessageChunk(_) => Vec::new(),
        SessionUpdate::ToolCall(call) => project_tool_call(run_id, &call),
        SessionUpdate::ToolCallUpdate(update) => project_tool_update(run_id, &update),
        SessionUpdate::Plan(plan) => {
            let mut summary = plan
                .entries
                .iter()
                .take(MAX_PLAN_ENTRIES)
                .map(|entry| entry.content.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if plan.entries.len() > MAX_PLAN_ENTRIES {
                summary.push_str(&format!(
                    "; (+{} more)",
                    plan.entries.len() - MAX_PLAN_ENTRIES
                ));
            }
            vec![status_event(
                run_id,
                "plan",
                format!("{} entries: {summary}", plan.entries.len()),
            )]
        }
        SessionUpdate::AvailableCommandsUpdate(_) => vec![status_event(
            run_id,
            "available_commands",
            "agent reported available commands; cosh-shell does not render them yet",
        )],
        SessionUpdate::CurrentModeUpdate(update) => vec![status_event(
            run_id,
            "session_mode",
            format!(
                "agent switched session mode to {}",
                update.current_mode_id.0
            ),
        )],
        SessionUpdate::ConfigOptionUpdate(_) => vec![status_event(
            run_id,
            "session_config",
            "agent reported session configuration options",
        )],
        SessionUpdate::SessionInfoUpdate(_) => vec![status_event(
            run_id,
            "session_info",
            "agent reported session metadata",
        )],
        SessionUpdate::UsageUpdate(_) => vec![status_event(
            run_id,
            "session_usage",
            "agent reported context window or cost usage",
        )],
        _ => vec![status_event(
            run_id,
            "unsupported_update",
            "agent sent a session update this client does not recognize",
        )],
    }
}

fn project_tool_call(run_id: &str, call: &ToolCall) -> Vec<AgentEvent> {
    let tool_id = call.tool_call_id.0.to_string();
    let mut events = vec![AgentEvent::ToolCall {
        run_id: run_id.to_string(),
        tool_id: Some(tool_id.clone()),
        name: call.title.clone(),
        input: raw_json(call.raw_input.as_ref()),
    }];
    events.extend(tool_content(run_id, &tool_id, &call.content, call.status));
    events.extend(tool_status(run_id, &tool_id, call.status));
    events
}

fn project_tool_update(run_id: &str, update: &ToolCallUpdate) -> Vec<AgentEvent> {
    let tool_id = update.tool_call_id.0.to_string();
    let fields = &update.fields;
    let mut events = Vec::new();
    if let Some(title) = &fields.title {
        events.push(AgentEvent::ToolCall {
            run_id: run_id.to_string(),
            tool_id: Some(tool_id.clone()),
            name: title.clone(),
            input: raw_json(fields.raw_input.as_ref()),
        });
    }
    let status = fields.status.unwrap_or(ToolCallStatus::Pending);
    if let Some(content) = &fields.content {
        events.extend(tool_content(run_id, &tool_id, content, status));
    }
    events.extend(tool_status(run_id, &tool_id, status));
    events
}

fn tool_content(
    run_id: &str,
    tool_id: &str,
    content: &[ToolCallContent],
    status: ToolCallStatus,
) -> Vec<AgentEvent> {
    let stream = if status == ToolCallStatus::Failed {
        "stderr"
    } else {
        "stdout"
    };
    let mut events = Vec::new();
    for item in content {
        let text = match item {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => text.text.clone(),
                other => format!("[{} content not rendered]", content_kind(other)),
            },
            ToolCallContent::Diff(diff) => format!("[diff] {}", diff.path.display()),
            ToolCallContent::Terminal(_) => {
                "[terminal reference not rendered: cosh-shell does not provide terminal/*]"
                    .to_string()
            }
            _ => "[unsupported tool content not rendered]".to_string(),
        };
        if text.is_empty() {
            continue;
        }
        events.push(AgentEvent::ToolOutputDelta {
            run_id: run_id.to_string(),
            tool_id: tool_id.to_string(),
            stream: stream.to_string(),
            text,
        });
    }
    events
}

fn tool_status(run_id: &str, tool_id: &str, status: ToolCallStatus) -> Vec<AgentEvent> {
    match status {
        ToolCallStatus::Completed => vec![AgentEvent::ToolCompleted {
            run_id: run_id.to_string(),
            tool_id: tool_id.to_string(),
            status: "success".to_string(),
        }],
        ToolCallStatus::Failed => vec![AgentEvent::ToolCompleted {
            run_id: run_id.to_string(),
            tool_id: tool_id.to_string(),
            status: "error".to_string(),
        }],
        ToolCallStatus::Pending | ToolCallStatus::InProgress => vec![status_event(
            run_id,
            "tool_status",
            format!("{tool_id} is {status:?}"),
        )],
        _ => vec![status_event(
            run_id,
            "tool_status",
            format!("{tool_id} reported an unrecognized status {status:?}"),
        )],
    }
}

fn raw_json(value: Option<&serde_json::Value>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn content_kind(content: &ContentBlock) -> &'static str {
    match content {
        ContentBlock::Text(_) => "text",
        ContentBlock::Image(_) => "image",
        ContentBlock::Audio(_) => "audio",
        ContentBlock::ResourceLink(_) => "resource_link",
        ContentBlock::Resource(_) => "embedded_resource",
        _ => "unknown",
    }
}

fn status_event(run_id: &str, phase: &str, message: impl Into<String>) -> AgentEvent {
    AgentEvent::StatusChanged {
        run_id: run_id.to_string(),
        phase: phase.to_string(),
        message: message.into(),
    }
}

pub(super) fn stop_event(run_id: &str, reason: StopReason) -> AgentEvent {
    match reason {
        StopReason::EndTurn => AgentEvent::AgentCompleted {
            run_id: run_id.to_string(),
            summary: String::new(),
        },
        StopReason::Cancelled => AgentEvent::AgentCancelled {
            run_id: run_id.to_string(),
            reason: "session/cancel acknowledged by agent".to_string(),
        },
        StopReason::MaxTokens | StopReason::MaxTurnRequests | StopReason::Refusal => {
            AgentEvent::AgentFailed {
                run_id: run_id.to_string(),
                error: format!("acp turn stopped: {reason:?}"),
                error_code: Some(format!("acp_{reason:?}")),
                max_turns: None,
            }
        }
        _ => AgentEvent::AgentFailed {
            run_id: run_id.to_string(),
            error: format!("acp turn stopped with an unrecognized reason: {reason:?}"),
            error_code: Some("acp_unknown_stop_reason".to_string()),
            max_turns: None,
        },
    }
}
