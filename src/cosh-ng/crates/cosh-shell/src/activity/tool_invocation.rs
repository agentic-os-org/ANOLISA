use crate::runtime::prelude::*;
use crate::tools::display::{
    presentation_for_tool, ToolImpact, ToolPresentation, ToolPresentationKind,
};

use super::runtime::{ActivityKind, ActivityPresentation, RuntimeActivityRow};
use super::tool_result_summary::result_for_status;

#[derive(Debug, Clone)]
pub(crate) struct ToolInvocationRecord {
    pub(crate) invocation_id: String,
    pub(crate) run_id: String,
    pub(crate) tool_name: String,
    pub(crate) phase: ToolInvocationPhase,
    pub(crate) lifecycle: String,
    pub(crate) status: String,
    pub(crate) presentation: ToolPresentation,
    pub(crate) result: Option<ToolResultPresentation>,
    pub(crate) output: ToolResultAccumulator,
    pub(crate) activity_row_ids: Vec<String>,
    pub(crate) suppress_normal_card: bool,
    pub(crate) is_question: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolInvocationPhase {
    Call,
    Result,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolResultPresentation {
    pub(crate) headline: String,
    pub(crate) metrics: Vec<String>,
    pub(crate) action: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolResultAccumulator {
    pub(crate) stdout_lines: usize,
    pub(crate) stderr_lines: usize,
    pub(crate) stdout_bytes: usize,
    pub(crate) stderr_bytes: usize,
    pub(crate) output_sample: String,
    pub(crate) first_stdout_line: Option<String>,
    pub(crate) first_error_line: Option<String>,
    pub(crate) truncated: bool,
    pub(crate) output_ref: Option<ToolOutputRef>,
}

#[derive(Debug, Clone)]
pub(crate) enum ToolOutputRef {
    TerminalOutputId(String),
    OpaqueAuditRef(String),
    DebugLocalPath { audit_ref: String, path: String },
}

const TOOL_RESULT_SAMPLE_LIMIT: usize = 8 * 1024;

pub(super) fn control_tool_invocation_id(tool_use_id: &str, request_id: &str) -> String {
    if tool_use_id.trim().is_empty() {
        request_id.to_string()
    } else {
        tool_use_id.to_string()
    }
}

pub(super) fn upsert_tool_call_invocation(
    state: &mut InlineState,
    run_id: &str,
    invocation_id: &str,
    tool_name: &str,
    input: &str,
    lifecycle: &str,
    row_id: &str,
) {
    let presentation = presentation_for_tool(tool_name, input);
    let is_question = matches!(presentation.kind, ToolPresentationKind::Question);
    let record = tool_invocation_record_mut(
        state,
        invocation_id,
        run_id,
        tool_name,
        presentation,
        is_question,
    );
    record.phase = ToolInvocationPhase::Call;
    record.lifecycle = lifecycle.to_string();
    record.status = lifecycle.to_string();
    push_unique(&mut record.activity_row_ids, row_id);
}

pub(super) fn update_tool_output_invocation(
    state: &mut InlineState,
    run_id: &str,
    invocation_id: &str,
    stream: &str,
    text: &str,
    row_id: Option<&str>,
    output_ref: Option<ToolOutputRef>,
    event_index: Option<usize>,
) {
    let incoming_id = invocation_id;
    let invocation_id = resolve_tool_invocation_id(state, run_id, incoming_id, event_index);
    let language = state.language;
    let had_invocation = state
        .activity
        .tool_invocations
        .iter()
        .any(|record| record.invocation_id == invocation_id);
    let mut presentation = state
        .activity
        .tool_invocations
        .iter()
        .find(|record| record.invocation_id == invocation_id)
        .map(|record| record.presentation.clone())
        .unwrap_or_else(|| {
            presentation_for_tool(
                incoming_id,
                &serde_json::json!({ "name": incoming_id }).to_string(),
            )
        });
    if !had_invocation {
        presentation.kind = ToolPresentationKind::Custom;
        presentation.impact = ToolImpact::Unknown;
        presentation.target = Some(incoming_id.to_string());
        presentation.preview = incoming_id.to_string();
    }
    let original_name = presentation.original_name.clone();
    let is_question = matches!(presentation.kind, ToolPresentationKind::Question);
    let record = tool_invocation_record_mut(
        state,
        &invocation_id,
        run_id,
        &original_name,
        presentation,
        is_question,
    );
    match stream {
        "stderr" => {
            record.output.stderr_lines += text.lines().count();
            record.output.stderr_bytes += text.len();
            if record.output.first_error_line.is_none() {
                record.output.first_error_line = first_error_line(text);
            }
        }
        _ => {
            record.output.stdout_lines += text.lines().count();
            record.output.stdout_bytes += text.len();
            if record.output.first_stdout_line.is_none() {
                record.output.first_stdout_line = first_non_empty_line(text);
            }
            append_output_sample(&mut record.output.output_sample, text);
        }
    }
    if text.len() > 4_000 {
        record.output.truncated = true;
    }
    if let Some(row_id) = row_id {
        push_unique(&mut record.activity_row_ids, row_id);
    }
    if output_ref.is_some() {
        record.output.output_ref = output_ref;
    } else if let Some(row_id) = row_id {
        record.output.output_ref = Some(ToolOutputRef::OpaqueAuditRef(row_id.to_string()));
    }
    if !had_invocation {
        record.phase = ToolInvocationPhase::Result;
        record.lifecycle = "captured".to_string();
        record.status = "captured".to_string();
    }
    if record.phase == ToolInvocationPhase::Result || record.result.is_some() {
        record.result = Some(result_for_status(
            &record.presentation,
            &record.output,
            &record.status,
            language,
        ));
    }
}

pub(super) fn complete_tool_invocation(
    state: &mut InlineState,
    run_id: &str,
    invocation_id: &str,
    status: &str,
    row_id: Option<&str>,
    event_index: Option<usize>,
) {
    let incoming_id = invocation_id;
    let invocation_id = resolve_tool_invocation_id(state, run_id, incoming_id, event_index);
    let transcript_seen = state
        .control
        .provider_shell_transcript_seen(run_id, &invocation_id);
    let Some(idx) = state
        .activity
        .tool_invocations
        .iter()
        .position(|record| record.invocation_id == invocation_id)
    else {
        let presentation = presentation_for_tool(incoming_id, "{}");
        state.activity.tool_invocations.push(ToolInvocationRecord {
            invocation_id: invocation_id.to_string(),
            run_id: run_id.to_string(),
            tool_name: incoming_id.to_string(),
            phase: ToolInvocationPhase::Result,
            lifecycle: "completed".to_string(),
            status: status.to_string(),
            is_question: matches!(presentation.kind, ToolPresentationKind::Question),
            result: Some(result_for_status(
                &presentation,
                &ToolResultAccumulator::default(),
                status,
                state.language,
            )),
            presentation,
            output: ToolResultAccumulator::default(),
            activity_row_ids: row_id.into_iter().map(str::to_string).collect(),
            suppress_normal_card: false,
        });
        return;
    };
    let suppress_shell_success = state
        .activity
        .tool_invocations
        .get(idx)
        .is_some_and(|record| {
            shell_success_uses_transcript_surface(
                state,
                run_id,
                &invocation_id,
                &record.presentation,
                status,
                transcript_seen,
            )
        });
    let record = &mut state.activity.tool_invocations[idx];
    record.phase = ToolInvocationPhase::Result;
    record.status = status.to_string();
    record.lifecycle = "completed".to_string();
    record.result = Some(result_for_status(
        &record.presentation,
        &record.output,
        status,
        state.language,
    ));
    if let Some(row_id) = row_id {
        push_unique(&mut record.activity_row_ids, row_id);
    }
    if suppress_shell_success {
        record.suppress_normal_card = true;
    }
}

fn shell_success_uses_transcript_surface(
    state: &InlineState,
    run_id: &str,
    invocation_id: &str,
    presentation: &ToolPresentation,
    status: &str,
    transcript_seen: bool,
) -> bool {
    matches!(status, "success" | "completed")
        && matches!(presentation.kind, ToolPresentationKind::ShellCommand)
        && (transcript_seen
            || (state.control.provider_tool_is_shell(run_id, invocation_id)
                && state
                    .control
                    .provider_tool()
                    .output_text(run_id, invocation_id)
                    .is_some()))
}

fn tool_invocation_record_mut<'a>(
    state: &'a mut InlineState,
    invocation_id: &str,
    run_id: &str,
    tool_name: &str,
    presentation: ToolPresentation,
    is_question: bool,
) -> &'a mut ToolInvocationRecord {
    if let Some(idx) = state
        .activity
        .tool_invocations
        .iter()
        .position(|record| record.invocation_id == invocation_id)
    {
        let record = &mut state.activity.tool_invocations[idx];
        record.run_id = run_id.to_string();
        record.presentation = presentation;
        record.tool_name = tool_name.to_string();
        record.is_question = is_question;
        return record;
    }
    state.activity.tool_invocations.push(ToolInvocationRecord {
        invocation_id: invocation_id.to_string(),
        run_id: run_id.to_string(),
        tool_name: tool_name.to_string(),
        phase: ToolInvocationPhase::Call,
        lifecycle: "called".to_string(),
        status: "called".to_string(),
        presentation,
        result: None,
        output: ToolResultAccumulator::default(),
        activity_row_ids: Vec::new(),
        suppress_normal_card: false,
        is_question,
    });
    state
        .activity
        .tool_invocations
        .last_mut()
        .expect("pushed invocation")
}

fn resolve_tool_invocation_id(
    state: &InlineState,
    run_id: &str,
    incoming_id: &str,
    event_index: Option<usize>,
) -> String {
    if state
        .activity
        .tool_invocations
        .iter()
        .any(|record| record.invocation_id == incoming_id)
    {
        return incoming_id.to_string();
    }
    let matches = state
        .activity
        .tool_invocations
        .iter()
        .filter(|record| {
            record.run_id == run_id
                && record.phase == ToolInvocationPhase::Call
                && tool_identity_matches(record, incoming_id)
        })
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        return matches[0].invocation_id.clone();
    }
    if matches.len() > 1 {
        if let Some(record) = state.activity.tool_invocations.iter().rev().find(|record| {
            record.run_id == run_id
                && record.phase == ToolInvocationPhase::Result
                && record.status == "captured"
                && record
                    .invocation_id
                    .starts_with(&format!("{run_id}:event-"))
                && tool_identity_matches(record, incoming_id)
        }) {
            return record.invocation_id.clone();
        }
        return event_index
            .map(|event_index| super::runtime::tool_call_invocation_id(run_id, None, event_index))
            .unwrap_or_else(|| incoming_id.to_string());
    }
    incoming_id.to_string()
}

fn tool_identity_matches(record: &ToolInvocationRecord, incoming_id: &str) -> bool {
    record.tool_name.eq_ignore_ascii_case(incoming_id)
        || record
            .presentation
            .original_name
            .eq_ignore_ascii_case(incoming_id)
        || record
            .presentation
            .canonical_name
            .eq_ignore_ascii_case(incoming_id)
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

pub(super) fn tool_output_ref_for_row(row: &RuntimeActivityRow) -> Option<ToolOutputRef> {
    row.detail
        .lines()
        .find_map(|line| line.strip_prefix("output_ref: "))
        .filter(|value| value.starts_with("terminal-output://"))
        .map(|id| ToolOutputRef::TerminalOutputId(id.to_string()))
        .or_else(|| {
            row.detail
                .lines()
                .find_map(|line| line.strip_prefix("debug_output_ref: "))
                .map(|path| ToolOutputRef::DebugLocalPath {
                    audit_ref: row.id.clone(),
                    path: path.to_string(),
                })
        })
        .or_else(|| Some(ToolOutputRef::OpaqueAuditRef(row.id.clone())))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_shell_evidence_action(
    language: Language,
    rows: &mut Vec<RuntimeActivityRow>,
    tool_invocations: &mut Vec<ToolInvocationRecord>,
    run_id: &str,
    request_id: &str,
    tool_use_id: &str,
    action: &str,
    output_id: Option<&str>,
    direction: Option<&str>,
    lines: Option<u16>,
    status: &str,
    failure_reason: Option<&str>,
    command: Option<&str>,
    command_count: Option<usize>,
    has_more: bool,
    duplicate_provider_request: bool,
) -> String {
    let id = next_activity_id_from_rows(rows, "evidence");
    let invocation_id = control_tool_invocation_id(tool_use_id, request_id);
    let detail_reason = failure_reason.unwrap_or("<none>");
    let output_id = output_id.unwrap_or("<none>");
    let direction = direction.unwrap_or("<none>");
    let lines = lines
        .map(|lines| lines.to_string())
        .unwrap_or_else(|| "<none>".to_string());
    let command = command.unwrap_or("<none>");
    let command_label =
        shell_evidence_command_label(output_id, command).unwrap_or_else(|| "<none>".to_string());
    let preview = shell_evidence_summary_preview(action);
    let mut presentation_input = serde_json::json!({
        "action": action,
        "output_id": output_id,
        "direction": direction,
        "lines": lines,
        "status": status,
        "command": command,
        "command_label": command_label,
        "command_count": command_count.map(|count| count.to_string()).unwrap_or_else(|| "<none>".to_string()),
        "has_more": has_more.to_string(),
        "duplicate_provider_request": duplicate_provider_request.to_string(),
    });
    if let Some(reason) = failure_reason {
        presentation_input["reason"] = serde_json::json!(reason);
    }
    let presentation =
        presentation_for_tool("cosh_shell_evidence", &presentation_input.to_string());
    let row_status = if duplicate_provider_request {
        "duplicate"
    } else {
        status
    };
    let row = RuntimeActivityRow {
        id: id.clone(),
        audit_ref: None,
        run_id: run_id.to_string(),
        kind: ActivityKind::Tool,
        status: row_status.to_string(),
        subject: invocation_id.clone(),
        summary: I18n::new(language).format(
            MessageId::ActivityToolRequestedSummary,
            &[
                ("tool", "cosh_shell_evidence"),
                ("preview", preview),
                ("id", &id),
            ],
        ),
        detail: format!(
            "evidence: ShellEvidenceAction\nprovider: provider_control_protocol\nexecution_path: control_protocol_shell_evidence\nrequest_id: {request_id}\ntool_use_id: {tool_use_id}\ntool_name: cosh_shell_evidence\naction: {action}\noutput_id: {output_id}\ndirection: {direction}\nlines: {lines}\nstatus: {status}\nfailure_reason: {detail_reason}\ncommand: {command}\ncommand_count: {}\nhas_more: {has_more}\nduplicate_provider_request: {duplicate_provider_request}\nagent_result_visibility: provider_tool_result",
            command_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "<none>".to_string())
        ),
        presentation: Some(ActivityPresentation::Tool(presentation.clone())),
    };
    rows.push(row);
    let invocation_status = if duplicate_provider_request {
        "duplicate"
    } else if matches!(
        status,
        "unavailable" | "redacted_confirmation_required" | "failed" | "error"
    ) {
        "failed"
    } else {
        "success"
    };
    let result = result_for_status(
        &presentation,
        &ToolResultAccumulator::default(),
        invocation_status,
        language,
    );
    if let Some(record) = tool_invocations
        .iter_mut()
        .find(|record| record.invocation_id == invocation_id)
    {
        record.phase = ToolInvocationPhase::Result;
        record.lifecycle = "completed".to_string();
        record.status = invocation_status.to_string();
        record.presentation = presentation;
        record.result = Some(result);
        push_unique(&mut record.activity_row_ids, &id);
    } else {
        tool_invocations.push(ToolInvocationRecord {
            invocation_id,
            run_id: run_id.to_string(),
            tool_name: "cosh_shell_evidence".to_string(),
            phase: ToolInvocationPhase::Result,
            lifecycle: "completed".to_string(),
            status: invocation_status.to_string(),
            presentation,
            result: Some(result),
            output: ToolResultAccumulator::default(),
            activity_row_ids: vec![id.clone()],
            suppress_normal_card: false,
            is_question: false,
        });
    }
    id
}

fn shell_evidence_command_label(output_id: &str, command: &str) -> Option<String> {
    if command == "<none>" {
        return None;
    }
    let command_id = output_id
        .strip_prefix("terminal-output://")?
        .rsplit_once('/')?
        .1;
    if command_id.is_empty() || command_id.contains('/') {
        return None;
    }
    Some(format!("#{command_id} $ {command}"))
}

fn shell_evidence_summary_preview(action: &str) -> &'static str {
    match action {
        "list_commands" => "command history",
        "read_output" => "shell output excerpt",
        "already_delivered" => "already delivered shell evidence",
        _ => "shell evidence",
    }
}

pub(super) fn first_error_line(text: &str) -> Option<String> {
    first_non_empty_line(text)
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| super::runtime::truncate_activity_preview(line, 160))
}

fn append_output_sample(sample: &mut String, text: &str) {
    let remaining = TOOL_RESULT_SAMPLE_LIMIT.saturating_sub(sample.len());
    if remaining == 0 {
        return;
    }
    if text.len() <= remaining {
        sample.push_str(text);
        return;
    }
    for ch in text.chars() {
        if sample.len() + ch.len_utf8() > TOOL_RESULT_SAMPLE_LIMIT {
            break;
        }
        sample.push(ch);
    }
}

fn next_activity_id_from_rows(rows: &[RuntimeActivityRow], prefix: &str) -> String {
    let prefix_with_dash = format!("{prefix}-");
    let used_ids = rows
        .iter()
        .filter(|row| row.id.starts_with(&prefix_with_dash))
        .map(|row| row.id.clone())
        .collect::<HashSet<_>>();

    let mut next = 1;
    loop {
        let id = format!("{prefix}-{next}");
        if !used_ids.contains(&id) {
            return id;
        }
        next += 1;
    }
}
