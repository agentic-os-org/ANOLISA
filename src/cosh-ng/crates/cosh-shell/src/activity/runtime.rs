use crate::runtime::state::PendingInteractiveShellHandoff;
use crate::tools::display::{presentation_for_tool, ToolPresentation};
use crate::tools::{known_provider_tool, KnownProviderTool};

use crate::runtime::prelude::*;

use super::runtime_output::tool_output_detail;
pub(crate) use super::runtime_output::write_tool_output_ref;
pub(crate) use super::runtime_render::{
    render_activity_details_by_id, render_activity_rows, render_provider_native_shell_transcript,
};
pub(crate) use super::shell_handoff::{
    close_untracked_shell_handoffs, record_approved_shell_handoff_blocks,
};
use super::tool_invocation::{
    complete_tool_invocation, control_tool_invocation_id, first_error_line,
    tool_output_ref_for_row, update_tool_output_invocation, upsert_tool_call_invocation,
};
pub(crate) use super::tool_invocation::{
    record_shell_evidence_action, ToolInvocationPhase, ToolInvocationRecord, ToolOutputRef,
};

#[derive(Debug, Clone)]
pub(crate) enum ActivityPresentation {
    Tool(ToolPresentation),
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeActivityRow {
    pub(crate) id: String,
    pub(crate) audit_ref: Option<String>,
    pub(crate) run_id: String,
    pub(crate) kind: ActivityKind,
    pub(crate) status: String,
    pub(crate) subject: String,
    pub(crate) summary: String,
    pub(crate) detail: String,
    pub(crate) presentation: Option<ActivityPresentation>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ActivityKind {
    ToolOutput,
    Tool,
    ShellHandoff,
}

#[cfg(test)]
pub(super) fn record_activity_rows(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
) -> Vec<String> {
    record_activity_rows_with_policy(state, governed_events, ActivityRecordPolicy::default())
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ActivityRecordPolicy {
    pub(crate) suppress_provider_native_shell: bool,
    pub(crate) shell_evidence_tool_available: bool,
    pub(crate) origin: AgentRunOrigin,
}

pub(crate) fn record_activity_rows_with_policy(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    policy: ActivityRecordPolicy,
) -> Vec<String> {
    let mut ids = Vec::new();
    let permission_tool_use_ids = governed_events
        .iter()
        .filter_map(|event| match &event.event {
            AgentEvent::ToolPermissionRequest { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for (event_index, event) in governed_events.iter().enumerate() {
        if let AgentEvent::ShellEvidenceRequest {
            run_id,
            request_id,
            tool_use_id,
            ..
        } = &event.event
        {
            if let Some(id) = shell_evidence_activity_row_id(state, run_id, request_id, tool_use_id)
            {
                ids.push(id);
            }
            continue;
        }

        let row = match &event.event {
            AgentEvent::ToolCall {
                run_id,
                tool_id,
                name,
                input,
            } => {
                let covered_by_control_permission = tool_id
                    .as_deref()
                    .is_some_and(|tool_id| permission_tool_use_ids.contains(tool_id));
                if is_shell_tool_name(name) {
                    if let Some(tool_id) = tool_id.as_deref() {
                        state
                            .control
                            .record_provider_shell_command_from_tool_call(run_id, tool_id, input);
                    } else {
                        state
                            .control
                            .record_pending_provider_shell_command(run_id, input);
                    }
                    if covered_by_control_permission {
                        continue;
                    }
                    let provider_shell_command = provider_shell_command_for_tool_call(
                        state,
                        run_id,
                        tool_id.as_deref(),
                        input,
                    );
                    if policy.suppress_provider_native_shell {
                        if provider_shell_transcript_seen(state, run_id, tool_id.as_deref()) {
                            continue;
                        }
                        if provider_shell_command.as_deref().is_some_and(|command| {
                            state
                                .control
                                .provider_foreground_shell_command_seen(command)
                        }) {
                            if let Some(tool_id) = tool_id.as_deref() {
                                state
                                    .control
                                    .mark_provider_shell_transcript_seen(run_id, tool_id);
                            }
                            continue;
                        }
                        let row = provider_native_shell_auto_approved_row(
                            state,
                            run_id,
                            tool_id.as_deref(),
                            name,
                            input,
                            None,
                        );
                        let invocation_id =
                            tool_call_invocation_id(run_id, tool_id.as_deref(), event_index);
                        upsert_tool_call_invocation(
                            state,
                            run_id,
                            &invocation_id,
                            name,
                            input,
                            "auto-approved",
                            &row.id,
                        );
                        Some(row)
                    } else {
                        let row = provider_tool_call_row(
                            state,
                            run_id,
                            tool_id.as_deref(),
                            name,
                            input,
                            policy.shell_evidence_tool_available,
                        );
                        let invocation_id =
                            tool_call_invocation_id(run_id, tool_id.as_deref(), event_index);
                        upsert_tool_call_invocation(
                            state,
                            run_id,
                            &invocation_id,
                            name,
                            input,
                            "called",
                            &row.id,
                        );
                        Some(row)
                    }
                } else {
                    if covered_by_control_permission {
                        continue;
                    }
                    let row = provider_tool_call_row(
                        state,
                        run_id,
                        tool_id.as_deref(),
                        name,
                        input,
                        policy.shell_evidence_tool_available,
                    );
                    let invocation_id =
                        tool_call_invocation_id(run_id, tool_id.as_deref(), event_index);
                    upsert_tool_call_invocation(
                        state,
                        run_id,
                        &invocation_id,
                        name,
                        input,
                        "called",
                        &row.id,
                    );
                    Some(row)
                }
            }
            AgentEvent::ToolOutputDelta {
                run_id,
                tool_id,
                stream,
                text,
            } => {
                state
                    .control
                    .record_provider_tool_output_delta(run_id, tool_id, stream, text);
                if state
                    .control
                    .provider_tool_is_control_permission_shell(run_id, tool_id)
                    || (state.control.provider_tool_is_shell(run_id, tool_id)
                        && state
                            .control
                            .provider_shell_transcript_seen(run_id, tool_id))
                {
                    update_tool_output_invocation(
                        state,
                        run_id,
                        tool_id,
                        stream,
                        text,
                        None,
                        None,
                        Some(event_index),
                    );
                    continue;
                } else {
                    let row = tool_output_row(state, run_id, tool_id, stream, text);
                    let output_ref = tool_output_ref_for_row(&row);
                    update_tool_output_invocation(
                        state,
                        run_id,
                        tool_id,
                        stream,
                        text,
                        Some(&row.id),
                        output_ref,
                        Some(event_index),
                    );
                    Some(row)
                }
            }
            AgentEvent::ToolPermissionRequest {
                run_id,
                request_id,
                tool_name,
                tool_input,
                tool_use_id,
                audit_ref,
                ..
            } => {
                state.control.record_provider_tool_command_from_input(
                    run_id,
                    tool_use_id,
                    tool_input,
                );
                if is_shell_tool_name(tool_name) {
                    state
                        .control
                        .mark_provider_control_permission_shell_tool(run_id, tool_use_id);
                }
                let row = provider_tool_request_row(
                    state,
                    run_id,
                    request_id,
                    tool_name,
                    tool_input,
                    tool_use_id,
                    audit_ref.as_deref(),
                    policy.shell_evidence_tool_available,
                );
                let input_str = serde_json::to_string(tool_input).unwrap_or_default();
                let invocation_id = control_tool_invocation_id(tool_use_id, request_id);
                upsert_tool_call_invocation(
                    state,
                    run_id,
                    &invocation_id,
                    tool_name,
                    &input_str,
                    "requested",
                    &row.id,
                );
                Some(row)
            }
            AgentEvent::ToolCompleted {
                run_id,
                tool_id,
                status,
            } => {
                if state
                    .control
                    .provider_tool_is_control_permission_shell(run_id, tool_id)
                    || (state.control.provider_tool_is_shell(run_id, tool_id)
                        && state
                            .control
                            .provider_shell_transcript_seen(run_id, tool_id))
                {
                    complete_tool_invocation(
                        state,
                        run_id,
                        tool_id,
                        status,
                        None,
                        Some(event_index),
                    );
                    continue;
                } else {
                    let row = tool_completed_row(state, run_id, tool_id, status, policy.origin);
                    complete_tool_invocation(
                        state,
                        run_id,
                        tool_id,
                        status,
                        Some(&row.id),
                        Some(event_index),
                    );
                    Some(row)
                }
            }
            AgentEvent::ToolHookVerdict {
                run_id,
                tool_id,
                verdict,
            } => {
                if verdict == "blocked" {
                    state
                        .control
                        .mark_provider_hook_blocked_result(run_id, tool_id);
                    // A block marker arriving after the staging grace converts
                    // the provisional staged_unresolved journal entry into the
                    // final rejection (#2156).
                    crate::approval::requests::reconcile_staged_unresolved_entry(
                        state,
                        run_id,
                        tool_id,
                        ApprovalRequestStatus::Blocked,
                        "cosh-core",
                        "hook_block",
                    );
                }
                None
            }
            _ => None,
        };
        if let Some(row) = row {
            let id = row.id.clone();
            state.activity.rows.push(row);
            ids.push(id);
        }
    }
    ids
}

fn shell_evidence_activity_row_id(
    state: &InlineState,
    run_id: &str,
    request_id: &str,
    tool_use_id: &str,
) -> Option<String> {
    let invocation_id = control_tool_invocation_id(tool_use_id, request_id);
    if let Some(id) = state
        .activity
        .tool_invocations
        .iter()
        .rev()
        .find(|record| {
            record.run_id == run_id
                && record.invocation_id == invocation_id
                && record.tool_name == "cosh_shell_evidence"
        })
        .and_then(|record| record.activity_row_ids.last())
    {
        return Some(id.clone());
    }

    state
        .activity
        .rows
        .iter()
        .rev()
        .find(|row| row.run_id == run_id && row.subject == invocation_id)
        .map(|row| row.id.clone())
}

pub(super) fn tool_call_invocation_id(
    run_id: &str,
    tool_id: Option<&str>,
    event_index: usize,
) -> String {
    tool_id
        .filter(|id| !id.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{run_id}:event-{event_index}"))
}

fn provider_shell_transcript_seen(
    state: &InlineState,
    run_id: &str,
    tool_id: Option<&str>,
) -> bool {
    tool_id.is_some_and(|tool_id| {
        state
            .control
            .provider_shell_transcript_seen(run_id, tool_id)
    })
}

fn provider_shell_command_for_tool_call(
    state: &InlineState,
    run_id: &str,
    tool_id: Option<&str>,
    input: &str,
) -> Option<String> {
    tool_id
        .and_then(|tool_id| state.control.provider_tool().command(run_id, tool_id))
        .map(|command| command.command.clone())
        .or_else(|| shell_command_from_tool_call_input(input))
}

fn shell_command_from_tool_call_input(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.contains('\0') {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| {
            value
                .get("command")
                .and_then(|command| command.as_str())
                .filter(|command| !command.is_empty() && !command.contains('\0'))
                .map(ToString::to_string)
        })
        .or_else(|| Some(input.to_string()))
}

fn provider_native_shell_auto_approved_row(
    state: &mut InlineState,
    run_id: &str,
    tool_id: Option<&str>,
    tool_name: &str,
    input: &str,
    artifact: Option<(&str, &str)>,
) -> RuntimeActivityRow {
    let id = next_activity_id(state, "tool");
    let subject = tool_id.unwrap_or(tool_name).to_string();
    let command = tool_id
        .and_then(|tool_id| state.control.provider_tool().command(run_id, tool_id))
        .map(|command| command.command.as_str())
        .unwrap_or(input);
    let mut detail = format!(
        "evidence: ProviderNativeShellBypass\nprovider: provider_native_stream\nexecution_path: provider_native_shell_bypassed_control_protocol\ntool_id: {}\ntool_name: {tool_name}\nprovider_native_shell_command: {}\ninput_preview: {}\nprovider_auto_approval_status: auto_approved_by_provider\nreason: control_protocol_provider_emitted_shell_tool_without_foreground_handoff",
        tool_id.unwrap_or("<none>"),
        truncate_activity_preview(command, 4_000),
        truncate_activity_preview(input, 4_000)
    );
    if let Some((kind, text)) = artifact {
        detail.push_str(&format!(
            "\nartifact_kind: {kind}\nartifact_preview:\n{}",
            truncate_activity_preview(text, 4_000)
        ));
    }
    let preview = legacy_activity_summary_preview(&format!("$ {command}"), 120);
    RuntimeActivityRow {
        id: id.clone(),
        audit_ref: None,
        run_id: run_id.to_string(),
        kind: ActivityKind::Tool,
        status: "auto-approved".to_string(),
        subject,
        summary: legacy_activity_summary_message(
            state,
            MessageId::ActivityProviderNativeShellBypassSummary,
            &[("tool", tool_name), ("preview", &preview), ("id", &id)],
        ),
        detail,
        presentation: Some(ActivityPresentation::Tool(presentation_for_tool(
            tool_name, input,
        ))),
    }
}

fn provider_tool_call_row(
    state: &mut InlineState,
    run_id: &str,
    tool_id: Option<&str>,
    tool_name: &str,
    input: &str,
    shell_evidence_tool_available: bool,
) -> RuntimeActivityRow {
    let id = next_activity_id(state, "tool");
    let presentation = presentation_for_tool(tool_name, input);
    let info = display_for_tool(tool_name, input);
    let misroute_detail =
        terminal_output_misroute_detail(tool_name, input, shell_evidence_tool_available);
    RuntimeActivityRow {
        id: id.clone(),
        audit_ref: None,
        run_id: run_id.to_string(),
        kind: ActivityKind::Tool,
        status: "called".to_string(),
        subject: tool_id.unwrap_or(&info.label).to_string(),
        summary: legacy_activity_summary_message(
            state,
            MessageId::ActivityToolCalledSummary,
            &[
                ("tool", tool_name),
                ("preview", &legacy_activity_summary_preview(&info.preview, 120)),
                ("id", &id),
            ],
        ),
        detail: format!(
            "evidence: ProviderToolCall\nprovider: provider_native_stream\nexecution_path: provider_native_stream\ntool_id: {}\ntool_name: {tool_name}\ninput_preview: {}{}\nagent_result_visibility: provider_native_result",
            tool_id.unwrap_or("<none>"),
            info.preview,
            misroute_detail
        ),
        presentation: Some(ActivityPresentation::Tool(presentation)),
    }
}

fn provider_tool_request_row(
    state: &mut InlineState,
    run_id: &str,
    request_id: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_use_id: &str,
    audit_ref: Option<&str>,
    shell_evidence_tool_available: bool,
) -> RuntimeActivityRow {
    let id = next_activity_id(state, "tool");
    let input_str = serde_json::to_string(tool_input).unwrap_or_default();
    let presentation = presentation_for_tool(tool_name, &input_str);
    let info = display_for_tool(tool_name, &input_str);
    let preview = provider_tool_input_preview(tool_name, tool_input, &info.preview);
    let misroute_detail =
        terminal_output_misroute_detail(tool_name, &input_str, shell_evidence_tool_available);
    RuntimeActivityRow {
        id: id.clone(),
        audit_ref: audit_ref.map(str::to_string),
        run_id: run_id.to_string(),
        kind: ActivityKind::Tool,
        status: "requested".to_string(),
        subject: provider_tool_request_subject(tool_use_id, request_id),
        summary: legacy_activity_summary_message(
            state,
            MessageId::ActivityToolRequestedSummary,
            &[
                ("tool", &info.label),
                ("preview", &legacy_activity_summary_preview(&preview, 120)),
                ("id", &id),
            ],
        ),
        detail: format!(
            "evidence: ProviderToolRequest\nprovider: provider_control_protocol\nexecution_path: provider_control_protocol\nrequest_id: {request_id}\ntool_use_id: {tool_use_id}\ntool_name: {tool_name}\naudit_ref: {}\ninput_preview: {preview}{misroute_detail}\nagent_result_visibility: provider_native_result",
            audit_ref.unwrap_or("<none>")
        ),
        presentation: Some(ActivityPresentation::Tool(presentation)),
    }
}

fn provider_tool_request_subject(tool_use_id: &str, request_id: &str) -> String {
    if tool_use_id.trim().is_empty() {
        request_id.to_string()
    } else {
        tool_use_id.to_string()
    }
}

fn terminal_output_misroute_detail(
    tool_name: &str,
    input: &str,
    shell_evidence_tool_available: bool,
) -> String {
    let Some(output_id) = terminal_output_id_from_read_tool_input(tool_name, input) else {
        return String::new();
    };
    let recommended_action = if shell_evidence_tool_available {
        "cosh_shell_evidence_read_output"
    } else {
        "fenced_cosh_request_output"
    };
    format!(
        "\nvirtual_evidence_read_misroute: true\nmisrouted_output_id: {output_id}\nrecommended_action: {recommended_action}"
    )
}

fn terminal_output_id_from_read_tool_input(tool_name: &str, input: &str) -> Option<String> {
    if known_provider_tool(tool_name) != Some(KnownProviderTool::ReadFile) {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    ["path", "file_path"]
        .iter()
        .filter_map(|key| value.get(key).and_then(|value| value.as_str()))
        .find(|path| path.starts_with("terminal-output://"))
        .map(ToString::to_string)
}

fn provider_tool_input_preview(
    tool_name: &str,
    tool_input: &serde_json::Value,
    display_preview: &str,
) -> String {
    let preview = if is_shell_tool_name(tool_name) {
        tool_input
            .get("command")
            .and_then(|value| value.as_str())
            .map(|command| format!("$ {command}"))
            .unwrap_or_else(|| display_preview.to_string())
    } else {
        display_preview.to_string()
    };
    truncate_activity_preview(&preview, 4_000)
}

fn tool_completed_row(
    state: &mut InlineState,
    run_id: &str,
    tool_id: &str,
    status: &str,
    origin: AgentRunOrigin,
) -> RuntimeActivityRow {
    let id = next_activity_id(state, "tool");
    let interactive_handoff =
        maybe_queue_interactive_shell_handoff(state, run_id, tool_id, status, origin);
    let stderr = state.control.provider_tool().stderr(run_id, tool_id);
    let stderr_summary = stderr.and_then(first_error_line);
    let mut summary = if matches!(status, "error" | "failed" | "interrupted") {
        match stderr_summary.as_deref() {
            Some(line) => line.to_string(),
            None => status.to_string(),
        }
    } else {
        status.to_string()
    };
    let mut detail = format!("tool: {tool_id}\nstatus: {status}");
    if let Some(command) = state.control.provider_tool().command(run_id, tool_id) {
        detail.push_str(&format!(
            "\nprovider_native_shell_command: {}",
            command.command
        ));
    }
    if let Some(stderr) = stderr {
        detail.push_str("\nstderr:\n");
        detail.push_str(stderr);
    }
    if let Some(handoff) = interactive_handoff {
        let handoff_summary = legacy_activity_summary_message(
            state,
            MessageId::ActivityToolNeedsForegroundShellSummary,
            &[("handoff", &handoff.id), ("id", &id)],
        );
        summary = match stderr_summary.as_deref() {
            Some(line) => format!("{line}; {handoff_summary}"),
            None => handoff_summary,
        };
        detail.push_str(&format!(
            "\ninteractive_hint: may_require_foreground_shell\nsend_to_shell_action: {}\nexact_command: {}\nprovider_tool_id: {}\nfollow_up: start a new Agent turn after the shell command completes if analysis is needed",
            handoff.id, handoff.command, handoff.tool_id
        ));
    }
    RuntimeActivityRow {
        id,
        audit_ref: None,
        run_id: run_id.to_string(),
        kind: ActivityKind::Tool,
        status: status.to_string(),
        subject: tool_id.to_string(),
        summary,
        detail,
        presentation: None,
    }
}

fn maybe_queue_interactive_shell_handoff(
    state: &mut InlineState,
    run_id: &str,
    tool_id: &str,
    status: &str,
    origin: AgentRunOrigin,
) -> Option<PendingInteractiveShellHandoff> {
    state
        .control
        .queue_interactive_shell_handoff_for_tool_failure(run_id, tool_id, status, origin)
}

pub(super) fn truncate_activity_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}... <truncated>")
    } else {
        truncated
    }
}

fn legacy_activity_summary_preview(value: &str, max_chars: usize) -> String {
    truncate_activity_preview(&value.replace('\n', "\\n"), max_chars)
}

fn tool_output_row(
    state: &mut InlineState,
    run_id: &str,
    tool_id: &str,
    stream: &str,
    text: &str,
) -> RuntimeActivityRow {
    let id = next_activity_id(state, "out");
    let output_ref = state
        .activity
        .output_dir
        .as_deref()
        .and_then(|dir| write_tool_output_ref(dir, &id, text).ok())
        .map(|path| path.display().to_string());
    let provider_native_shell_command = state
        .control
        .provider_tool()
        .command(run_id, tool_id)
        .map(|command| command.command.as_str());
    let provider_shell_tool = state.control.provider_tool_is_shell(run_id, tool_id);
    RuntimeActivityRow {
        id: id.clone(),
        audit_ref: None,
        run_id: run_id.to_string(),
        kind: ActivityKind::ToolOutput,
        status: "captured".to_string(),
        subject: tool_id.to_string(),
        summary: tool_output_summary(state, stream, &id),
        detail: tool_output_detail(
            tool_id,
            stream,
            text.lines().count(),
            output_ref.as_deref(),
            text,
            provider_native_shell_command,
            provider_shell_tool,
        ),
        presentation: None,
    }
}

pub(crate) fn next_activity_id(state: &InlineState, prefix: &str) -> String {
    next_activity_id_excluding(state, prefix, std::iter::empty())
}

pub(super) fn next_activity_id_excluding<'a>(
    state: &'a InlineState,
    prefix: &str,
    excluded_ids: impl IntoIterator<Item = &'a str>,
) -> String {
    let prefix_with_dash = format!("{prefix}-");
    let mut used_ids = state
        .activity
        .rows
        .iter()
        .filter(|row| row.id.starts_with(&prefix_with_dash))
        .map(|row| row.id.clone())
        .collect::<HashSet<_>>();
    used_ids.extend(excluded_ids.into_iter().map(str::to_string));

    let mut next = 1;
    loop {
        let id = format!("{prefix}-{next}");
        if !used_ids.contains(&id) {
            return id;
        }
        next += 1;
    }
}

pub(super) fn legacy_activity_summary_message(
    state: &InlineState,
    id: MessageId,
    args: &[(&str, &str)],
) -> String {
    state.i18n().format(id, args)
}

fn tool_output_summary(state: &InlineState, stream: &str, id: &str) -> String {
    let message_id = match stream {
        "stdout" => MessageId::ToolOutputStdoutCapturedSummary,
        "stderr" => MessageId::ToolOutputStderrCapturedSummary,
        _ => MessageId::ActivityToolOutputCapturedSummary,
    };
    legacy_activity_summary_message(state, message_id, &[("stream", stream), ("id", id)])
}
