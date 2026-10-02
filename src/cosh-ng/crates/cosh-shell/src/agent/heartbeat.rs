use std::time::{Duration, Instant};

use crate::agent::run::ActiveAgentRun;
use crate::runtime::prelude::*;
use crate::tools::display::display_tool_name;
use crate::types::{TOOL_ARGUMENTS_STATUS_PHASE, TOOL_ARGUMENTS_STATUS_PREFIX};

use super::display::display_agent_error;
#[cfg(test)]
use super::pending_tools::pending_tool_status_detail;
use super::pending_tools::{
    pending_tool_status_detail_for_run, pending_tool_status_detail_with_completed,
    shell_evidence_status_message,
};

const AGENT_HEARTBEAT_AFTER: Duration = Duration::from_secs(6);
const AGENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

pub(crate) fn render_agent_heartbeat<W: Write>(
    active_run: &mut ActiveAgentRun,
    output: &mut W,
    suppress_for_shell_handoff: bool,
) -> std::io::Result<()> {
    if suppress_for_shell_handoff {
        active_run.status_animation.clear(output)?;
        return Ok(());
    }

    let pending_tools = status_detail_for_run(active_run);
    if active_run.markdown_stream.has_started() {
        return Ok(());
    }
    if active_run.has_visible_text_delta && pending_tools.is_none() {
        return Ok(());
    }

    let i18n = I18n::new(active_run.language);
    let now = Instant::now();
    if active_run.status_animation.is_enabled() {
        let elapsed = now.duration_since(active_run.started_at).as_secs();
        if elapsed >= AGENT_HEARTBEAT_AFTER.as_secs() {
            let detail = if let Some(pending_tools) = pending_tools.as_deref() {
                pending_tools
            } else if active_run.current_message.is_empty() {
                active_run.current_phase.as_str()
            } else {
                active_run.current_message.as_str()
            };
            let text = elapsed_thinking_text(&i18n, active_run.language, elapsed, detail);
            return active_run.status_animation.render(output, &text);
        }
        if let Some(pending_tools) = pending_tools {
            let text = format!("{} {pending_tools}", i18n.t(MessageId::AgentThinking));
            return active_run.status_animation.render(output, &text);
        }
        return active_run
            .status_animation
            .render(output, i18n.t(MessageId::AgentThinking));
    }

    if now.duration_since(active_run.started_at) < AGENT_HEARTBEAT_AFTER {
        return Ok(());
    }
    if now.duration_since(active_run.last_activity_at) < AGENT_HEARTBEAT_AFTER {
        return Ok(());
    }
    if now.duration_since(active_run.last_heartbeat_at) < AGENT_HEARTBEAT_INTERVAL {
        return Ok(());
    }

    active_run.last_heartbeat_at = now;
    let elapsed = now.duration_since(active_run.started_at).as_secs_f32();
    let pending_tools = status_detail_for_run(active_run);
    let detail = if let Some(pending_tools) = pending_tools.as_deref() {
        pending_tools
    } else if active_run.current_message.is_empty() {
        active_run.current_phase.as_str()
    } else {
        active_run.current_message.as_str()
    };
    let elapsed_text = format!("{elapsed:.0}");
    let body = if status_detail_is_generic_thinking(detail, active_run.language) {
        format!("{} {elapsed_text}s", i18n.t(MessageId::AgentThinking))
    } else {
        i18n.format(
            MessageId::AgentStillWorking,
            &[("elapsed", &elapsed_text), ("detail", detail)],
        )
    };
    writeln!(output)?;
    active_run.renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: i18n.t(MessageId::AgentStatusTitle),
            body: vec![body],
            footer: Some(i18n.t(MessageId::AgentStatusFooter)),
        },
    )
}

fn elapsed_thinking_text(i18n: &I18n, language: Language, elapsed: u64, detail: &str) -> String {
    if status_detail_is_generic_thinking(detail, language) {
        format!("{} {elapsed}s", i18n.t(MessageId::AgentThinking))
    } else {
        i18n.format(
            MessageId::AgentThinkingElapsed,
            &[("elapsed", &elapsed.to_string()), ("detail", detail)],
        )
    }
}

fn status_detail_is_generic_thinking(detail: &str, language: Language) -> bool {
    let detail = detail.trim();
    match language {
        Language::ZhCn => detail == "正在思考" || detail == "正在思考...",
        Language::EnUs => {
            detail.eq_ignore_ascii_case("thinking") || detail.eq_ignore_ascii_case("thinking...")
        }
    }
}

/// The status detail the heartbeat should show, if any.
///
/// Falls back to the tool-argument generation status: that window has no pending
/// tool yet — the call only exists once its arguments finish — so without this it
/// loses to streamed text and to the "no detail" path, leaving the blank Agent
/// card this status was added to replace.
fn status_detail_for_run(active_run: &ActiveAgentRun) -> Option<String> {
    pending_tool_status_detail_for_run(active_run)
        .or_else(|| tool_argument_status_detail(active_run))
}

fn tool_argument_status_detail(active_run: &ActiveAgentRun) -> Option<String> {
    let i18n = I18n::new(active_run.language);
    // The phase is the localized label `display_status_changed` produced for the
    // `tool_arguments` wire phase, and every other event overwrites it — so it
    // marks exactly the window between block start and a complete tool call.
    if active_run.current_phase != i18n.t(MessageId::AgentStatusToolArguments) {
        return None;
    }
    Some(active_run.current_message.clone()).filter(|message| !message.is_empty())
}

pub(crate) fn render_agent_pending_tool_status<W: Write>(
    active_run: &mut ActiveAgentRun,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(status_detail) = status_detail_for_run(active_run) else {
        return Ok(());
    };
    if active_run.markdown_stream.has_started() || active_run.markdown_stream.has_buffered_text() {
        active_run.prepare_structured_surface(output)?;
    }

    let i18n = I18n::new(active_run.language);
    let text = format!("{} {status_detail}", i18n.t(MessageId::AgentThinking));
    if active_run.status_animation.is_enabled() {
        active_run.status_animation.render(output, &text)
    } else {
        active_run.renderer.write_loading_text(output, &text)
    }
}

pub(crate) fn render_agent_shell_evidence_pending_status<W: Write>(
    active_run: &mut ActiveAgentRun,
    output: &mut W,
) -> std::io::Result<()> {
    active_run.prepare_structured_surface(output)?;
    let i18n = I18n::new(active_run.language);
    let text = match active_run.language {
        Language::ZhCn => format!("{} Shell 证据 1 项", i18n.t(MessageId::AgentThinking)),
        Language::EnUs => format!("{} shell evidence 1 item", i18n.t(MessageId::AgentThinking)),
    };
    if active_run.status_animation.is_enabled() {
        active_run.status_animation.render(output, &text)
    } else {
        active_run.renderer.write_loading_text(output, &text)
    }
}

pub(crate) fn remember_agent_activity(active_run: &mut ActiveAgentRun, governed: &[GovernedEvent]) {
    if governed.is_empty() {
        return;
    }

    let i18n = I18n::new(active_run.language);
    let now = Instant::now();
    active_run.last_activity_at = now;
    for event in governed {
        match &event.event {
            AgentEvent::StatusChanged { phase, message, .. } => {
                let (phase, message) = display_status_changed(phase, message, &i18n);
                active_run.current_phase = phase;
                active_run.current_message = message;
            }
            AgentEvent::TextDelta { .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusStreaming).to_string();
                active_run.current_message =
                    i18n.t(MessageId::AgentStatusReceivingResponse).to_string();
            }

            AgentEvent::ToolCall { .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusTool).to_string();
                if let Some(pending_tools) = pending_tool_status_detail_with_completed(
                    active_run.language,
                    active_run.governed_events.iter().chain(governed.iter()),
                    active_run
                        .host_completed_tool_ids
                        .iter()
                        .map(String::as_str),
                ) {
                    active_run.current_message = pending_tools;
                } else {
                    active_run.current_message = i18n
                        .t(MessageId::AgentStatusRunningApprovedProviderTool)
                        .to_string();
                }
            }
            AgentEvent::UserQuestion { question, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusQuestion).to_string();
                let question = display_question_text(question, &i18n);
                active_run.current_message = i18n.format(
                    MessageId::AgentStatusWaitingUserAnswer,
                    &[("question", question.as_str())],
                );
            }
            AgentEvent::Action { command, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusApproval).to_string();
                active_run.current_message = i18n.format(
                    MessageId::AgentStatusWaitingApprovalCommand,
                    &[("command", command)],
                );
            }
            AgentEvent::ToolPermissionRequest { tool_name, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusApproval).to_string();
                active_run.current_message = i18n.format(
                    MessageId::AgentStatusWaitingApprovalTool,
                    &[("tool", tool_name)],
                );
            }
            AgentEvent::ToolOutputDelta { tool_id, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusTool).to_string();
                active_run.current_message = i18n.format(
                    MessageId::AgentStatusCapturingToolOutput,
                    &[("tool_id", tool_id)],
                );
            }
            AgentEvent::ToolCompleted {
                tool_id, status, ..
            } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusTool).to_string();
                if let Some(pending_tools) = pending_tool_status_detail_with_completed(
                    active_run.language,
                    active_run.governed_events.iter().chain(governed.iter()),
                    active_run
                        .host_completed_tool_ids
                        .iter()
                        .map(String::as_str),
                ) {
                    active_run.current_message = pending_tools;
                } else {
                    active_run.current_message = i18n.format(
                        MessageId::AgentStatusToolCompleted,
                        &[("tool_id", tool_id), ("status", status)],
                    );
                }
            }
            AgentEvent::ToolHookVerdict { .. } => {
                // Audit-only marker: the status line is already driven by the
                // tool result events around it.
            }
            AgentEvent::AgentCompleted { summary, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusCompleted).to_string();
                active_run.current_message = display_agent_summary(summary, &i18n);
            }
            AgentEvent::AgentFailed { error, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusFailed).to_string();
                active_run.current_message = display_agent_error(error, &i18n);
            }
            AgentEvent::AgentCancelled { reason, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusCancelled).to_string();
                active_run.current_message = reason.clone();
            }
            AgentEvent::Recommendation { summary, .. } => {
                active_run.current_message = summary.clone();
            }

            AgentEvent::AuthRequired { .. } => {
                active_run.current_phase = "auth".to_string();
                active_run.current_message = "Authentication credentials required".to_string();
            }
            AgentEvent::ShellEvidenceRequest { action, .. } => {
                active_run.current_phase = i18n.t(MessageId::AgentStatusTool).to_string();
                active_run.current_message =
                    shell_evidence_status_message(active_run.language, action.as_str());
            }
            AgentEvent::HookNotification {
                hook_name, message, ..
            } => {
                active_run.current_phase = "hook".to_string();
                active_run.current_message = format!("[{hook_name}] {message}");
            }
        }
    }
}

fn display_question_text(question: &str, i18n: &I18n) -> String {
    let question = question.trim();
    if question.is_empty() {
        i18n.t(MessageId::QuestionDefaultPrompt).to_string()
    } else {
        question.to_string()
    }
}

fn display_status_changed(phase: &str, message: &str, i18n: &I18n) -> (String, String) {
    let phase = if phase == "thinking" {
        i18n.t(MessageId::AgentStatusThinking).to_string()
    } else if phase == TOOL_ARGUMENTS_STATUS_PHASE {
        i18n.t(MessageId::AgentStatusToolArguments).to_string()
    } else {
        phase.to_string()
    };
    let message = display_status_message(message, i18n);
    (phase, message)
}

fn display_status_message(message: &str, i18n: &I18n) -> String {
    if message == "thinking" {
        return i18n.t(MessageId::AgentStatusThinking).to_string();
    }
    if message == "preparing model session" {
        return i18n
            .t(MessageId::AgentStatusPreparingModelSession)
            .to_string();
    }
    if is_starting_model_backend_message(message) {
        return i18n
            .t(MessageId::AgentStatusStartingModelBackend)
            .to_string();
    }
    // Carries the tool name only — the marker is emitted before any argument
    // byte is parsed, so there is nothing else safe to show.
    if let Some(tool) = message.strip_prefix(TOOL_ARGUMENTS_STATUS_PREFIX) {
        // Re-sanitized here because this is the boundary that reaches a terminal.
        return i18n.format(
            MessageId::AgentStatusGeneratingToolArguments,
            &[("tool", &display_tool_name(tool))],
        );
    }
    if let Some(model) = message.strip_prefix("model initialized ") {
        return i18n.format(MessageId::AgentStatusModelInitialized, &[("model", model)]);
    }
    if let Some(status) = message.strip_prefix("model status: ") {
        return i18n.format(MessageId::AgentStatusModelStatus, &[("status", status)]);
    }
    message.to_string()
}

fn is_starting_model_backend_message(message: &str) -> bool {
    matches!(
        message,
        "Starting model backend"
            | "starting model backend"
            | "starting claude-code stream-json backend"
            | "starting claude-code control protocol backend"
            | "starting co stream-json backend"
            | "starting co control protocol backend"
            | "starting cosh-tui headless backend"
            | "starting cosh-tui control protocol backend"
    )
}

fn display_agent_summary(summary: &str, i18n: &I18n) -> String {
    if summary == "analysis completed" {
        i18n.t(MessageId::AgentStatusAnalysisCompleted).to_string()
    } else {
        summary.to_string()
    }
}

#[cfg(test)]
#[path = "heartbeat_tests.rs"]
mod tests;
