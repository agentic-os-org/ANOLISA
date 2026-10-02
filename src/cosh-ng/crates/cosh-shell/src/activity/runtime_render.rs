use std::borrow::Cow;
use std::collections::HashSet;
use std::io::Write;

use crate::runtime::prelude::*;
use crate::tools::display::{ToolImpact, ToolPresentation, ToolPresentationKind};

use super::runtime::{
    ActivityKind, ActivityPresentation, RuntimeActivityRow, ToolInvocationPhase,
    ToolInvocationRecord, ToolOutputRef,
};

pub(crate) fn render_activity_rows<W: Write>(
    state: &InlineState,
    activity_ids: &[String],
    output: &mut W,
) -> std::io::Result<()> {
    let (cards, card_row_ids) = tool_invocation_cards_for_activity_ids(state, activity_ids);
    if !cards.is_empty() {
        RatatuiInlineRenderer::for_terminal()
            .with_language(state.language)
            .write_tool_invocation_cards(output, cards)?;
    }

    let rows = activity_ids
        .iter()
        .filter_map(|activity_id| {
            state
                .activity
                .rows
                .iter()
                .find(|row| row.id == *activity_id)
        })
        .filter(|row| !card_row_ids.contains(&row.id))
        .filter(|row| state.debug || row.status != "loading")
        .filter(|row| should_render_activity_row_with_state(row, state))
        .map(|row| activity_row_model(row, state.language))
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return Ok(());
    }

    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_activity_panel(output, ActivityPanelModel { rows })?;
    Ok(())
}

fn activity_row_model(row: &RuntimeActivityRow, language: Language) -> ActivityRowModel<'_> {
    ActivityRowModel {
        id: &row.id,
        kind: row.kind.label(),
        status: &row.status,
        subject: &row.subject,
        summary: &row.summary,
        tool: row
            .presentation
            .as_ref()
            .map(
                |ActivityPresentation::Tool(presentation)| ActivityToolRowModel {
                    kind: presentation.kind,
                    name: &presentation.canonical_name,
                    primary: activity_tool_primary_cow(presentation, language),
                },
            ),
    }
}

fn activity_tool_primary(presentation: &ToolPresentation, language: Language) -> String {
    if matches!(presentation.kind, ToolPresentationKind::ShellEvidence) {
        let mut parts = Vec::new();
        parts.push(
            presentation
                .target
                .as_deref()
                .unwrap_or(presentation.preview.as_str())
                .to_string(),
        );
        if presentation_field(presentation, "action") == Some("read_output") {
            if let (Some(direction), Some(lines)) = (
                presentation_field(presentation, "direction"),
                presentation_field(presentation, "lines"),
            ) {
                parts.push(match language {
                    Language::ZhCn => format!("{direction} {lines} 行"),
                    Language::EnUs => format!("{direction} {lines} lines"),
                });
            }
        }
        if let Some(reason) = presentation_field(presentation, "reason") {
            parts.push(match language {
                Language::ZhCn => format!("原因: {reason}"),
                Language::EnUs => format!("reason: {reason}"),
            });
        }
        return parts.join(" · ");
    }
    presentation
        .target
        .clone()
        .unwrap_or_else(|| presentation.preview.clone())
}

fn activity_tool_primary_cow(presentation: &ToolPresentation, language: Language) -> Cow<'_, str> {
    if matches!(presentation.kind, ToolPresentationKind::ShellEvidence) {
        Cow::Owned(activity_tool_primary(presentation, language))
    } else {
        Cow::Borrowed(
            presentation
                .target
                .as_deref()
                .unwrap_or(presentation.preview.as_str()),
        )
    }
}

fn presentation_field<'a>(presentation: &'a ToolPresentation, label: &str) -> Option<&'a str> {
    presentation
        .fields
        .iter()
        .find(|field| field.label == label)
        .map(|field| field.value.as_str())
}

fn tool_invocation_cards_for_activity_ids(
    state: &InlineState,
    activity_ids: &[String],
) -> (Vec<ToolInvocationCardModel>, HashSet<String>) {
    let mut cards = Vec::new();
    let mut seen = HashSet::new();
    let mut rendered_row_ids = HashSet::new();
    for activity_id in activity_ids {
        for record in &state.activity.tool_invocations {
            if record.activity_row_ids.iter().any(|id| id == activity_id)
                && seen.insert(record.invocation_id.clone())
            {
                if let Some(card) = tool_invocation_card_for_record(record, state) {
                    rendered_row_ids.extend(record.activity_row_ids.iter().cloned());
                    cards.push(card);
                }
            }
        }
    }
    (cards, rendered_row_ids)
}

#[cfg(test)]
pub(super) fn tool_invocation_cards_for_test(
    state: &InlineState,
    activity_ids: &[String],
) -> Vec<ToolInvocationCardModel> {
    tool_invocation_cards_for_activity_ids(state, activity_ids).0
}

fn tool_invocation_card_for_record(
    record: &ToolInvocationRecord,
    state: &InlineState,
) -> Option<ToolInvocationCardModel> {
    if record.is_question || record.suppress_normal_card {
        return None;
    }
    if record.phase == ToolInvocationPhase::Call {
        return None;
    }
    let i18n = I18n::new(state.language);
    let title = tool_invocation_title(&i18n, record);
    let density = tool_invocation_density(record);
    let primary = tool_invocation_primary(&i18n, record);
    let result = record
        .result
        .as_ref()
        .map(|result| result.headline.clone())
        .unwrap_or_else(|| record.status.clone());
    let mut metrics = record
        .result
        .as_ref()
        .map(|result| result.metrics.clone())
        .unwrap_or_default();
    if metrics.is_empty() {
        metrics = result_metrics_from_output(&i18n, record);
    }
    let tone = tone_for_status(&record.status, record);
    let action = record
        .result
        .as_ref()
        .and_then(|result| result.action.clone());
    Some(ToolInvocationCardModel {
        title,
        status: record.status.clone(),
        density,
        primary,
        result,
        metrics,
        action,
        debug_ref: if state.debug {
            Some(record.invocation_id.clone())
        } else {
            None
        },
        tone,
    })
}

fn tool_invocation_density(record: &ToolInvocationRecord) -> ToolInvocationDensity {
    if record.phase == ToolInvocationPhase::Call {
        return ToolInvocationDensity::ActionRequired;
    }
    match record.status.as_str() {
        "error" | "failed" | "interrupted" | "denied" | "cancelled" => {
            ToolInvocationDensity::Diagnostic
        }
        _ if matches!(
            record.presentation.kind,
            ToolPresentationKind::FileWrite | ToolPresentationKind::FileEdit
        ) =>
        {
            ToolInvocationDensity::Summary
        }
        _ => ToolInvocationDensity::Receipt,
    }
}

fn tool_invocation_primary(i18n: &I18n, record: &ToolInvocationRecord) -> String {
    if matches!(record.presentation.kind, ToolPresentationKind::Skill)
        && presentation_field(&record.presentation, "action") == Some("list")
    {
        return match i18n.language() {
            Language::ZhCn => "技能列表".to_string(),
            Language::EnUs => "skill list".to_string(),
        };
    }
    if matches!(record.presentation.kind, ToolPresentationKind::FileRead)
        && record.presentation.target.as_deref() == Some("Shell output bookmark")
        && i18n.language() == Language::ZhCn
    {
        return "Shell 输出书签".to_string();
    }
    if matches!(
        record.presentation.kind,
        ToolPresentationKind::MultiFileRead
    ) && i18n.language() == Language::ZhCn
    {
        if let Some(target) = record.presentation.target.as_deref() {
            if let Some(count) = target.strip_suffix(" files") {
                return format!("{count} 个文件");
            }
        }
    }
    record
        .presentation
        .target
        .clone()
        .unwrap_or_else(|| record.presentation.preview.clone())
}

fn tool_invocation_title(i18n: &I18n, record: &ToolInvocationRecord) -> String {
    let status = match record.phase {
        ToolInvocationPhase::Call => match record.lifecycle.as_str() {
            "requested" => i18n.t(MessageId::ToolCardRequestedStatus),
            "auto-approved" => i18n.t(MessageId::ToolCardAutoApprovedStatus),
            _ => i18n.t(MessageId::ToolCardCalledStatus),
        },
        ToolInvocationPhase::Result => match record.status.as_str() {
            "success" | "completed" => i18n.t(MessageId::ToolCardCompletedStatus),
            "captured" => i18n.t(MessageId::ToolCardCapturedStatus),
            "error" | "failed" => i18n.t(MessageId::ToolCardFailedStatus),
            "duplicate" => i18n.t(MessageId::ToolCardDuplicateStatus),
            "interrupted" => i18n.t(MessageId::ToolCardInterruptedStatus),
            other => other,
        },
    };
    format!("{} {status}", tool_kind_label(i18n, record))
}

fn tool_kind_label(i18n: &I18n, record: &ToolInvocationRecord) -> String {
    let label = match record.presentation.kind {
        ToolPresentationKind::ShellCommand => i18n.t(MessageId::ToolCardShellLabel),
        ToolPresentationKind::FileRead | ToolPresentationKind::MultiFileRead => {
            i18n.t(MessageId::ToolCardReadFileLabel)
        }
        ToolPresentationKind::FileWrite => i18n.t(MessageId::ToolCardWriteFileLabel),
        ToolPresentationKind::FileEdit => i18n.t(MessageId::ToolCardEditFileLabel),
        ToolPresentationKind::FileSearch | ToolPresentationKind::Lsp => {
            i18n.t(MessageId::ToolCardSearchFilesLabel)
        }
        ToolPresentationKind::FileGlob => i18n.t(MessageId::ToolCardFindFilesLabel),
        ToolPresentationKind::DirectoryList => i18n.t(MessageId::ToolCardListDirectoryLabel),
        ToolPresentationKind::WebFetch => i18n.t(MessageId::ToolCardWebFetchLabel),
        ToolPresentationKind::WebSearch => i18n.t(MessageId::ToolCardWebSearchLabel),
        ToolPresentationKind::Skill => i18n.t(MessageId::ToolCardSkillLabel),
        ToolPresentationKind::Agent if record.presentation.canonical_name != "Agent" => {
            return record.presentation.canonical_name.clone();
        }
        ToolPresentationKind::Agent => i18n.t(MessageId::ToolCardAgentLabel),
        ToolPresentationKind::Memory if record.presentation.canonical_name != "Memory" => {
            return record.presentation.canonical_name.clone();
        }
        ToolPresentationKind::Memory => i18n.t(MessageId::ToolCardMemoryLabel),
        ToolPresentationKind::ShellEvidence => i18n.t(MessageId::ToolCardEvidenceLabel),
        ToolPresentationKind::Question | ToolPresentationKind::Custom => {
            return record.presentation.canonical_name.clone();
        }
    };
    label.to_string()
}

fn result_metrics_from_output(i18n: &I18n, record: &ToolInvocationRecord) -> Vec<String> {
    let mut metrics = Vec::new();
    if matches!(
        record.presentation.kind,
        ToolPresentationKind::ShellEvidence
    ) {
        return metrics;
    } else if record.output.stdout_lines > 0 {
        metrics.push(i18n.format(
            MessageId::ToolCardStdoutMetric,
            &[("count", &record.output.stdout_lines.to_string())],
        ));
    }
    if record.output.stderr_lines > 0 {
        metrics.push(i18n.format(
            MessageId::ToolCardStderrMetric,
            &[("count", &record.output.stderr_lines.to_string())],
        ));
    }
    if record.output.truncated {
        metrics.push(i18n.t(MessageId::ToolCardTruncatedMetric).to_string());
    }
    metrics
}

fn tone_for_status(record_status: &str, record: &ToolInvocationRecord) -> ToolInvocationTone {
    match record_status {
        "success" | "completed" => {
            if matches!(record.presentation.kind, ToolPresentationKind::Custom) {
                ToolInvocationTone::Custom
            } else {
                ToolInvocationTone::Success
            }
        }
        "partial" | "truncated" | "no_match" | "redirected" => ToolInvocationTone::Warning,
        "duplicate" => ToolInvocationTone::Warning,
        "error" | "failed" | "interrupted" | "denied" | "cancelled" => ToolInvocationTone::Failure,
        _ => ToolInvocationTone::Pending,
    }
}

pub(crate) fn render_provider_native_shell_transcript<W: Write>(
    state: &mut InlineState,
    activity_ids: &[String],
    output: &mut W,
) -> std::io::Result<()> {
    let rows = activity_ids
        .iter()
        .filter_map(|activity_id| {
            state
                .activity
                .rows
                .iter()
                .find(|row| row.id == *activity_id)
                .cloned()
        })
        .collect::<Vec<_>>();

    for row in rows {
        match row.kind {
            ActivityKind::ToolOutput => {
                let tool_id = row.subject.as_str();
                let run_id = row.run_id.as_str();
                let Some(command) = state
                    .control
                    .provider_tool()
                    .command(run_id, tool_id)
                    .map(|command| command.command.clone())
                else {
                    continue;
                };
                if state
                    .control
                    .provider_shell_transcript_output_seen(run_id, tool_id)
                {
                    continue;
                }
                if state
                    .control
                    .claim_provider_shell_transcript_command(run_id, tool_id)
                {
                    writeln!(output, "$ {command}")?;
                }
                let text = state
                    .control
                    .provider_tool()
                    .output_text(run_id, tool_id)
                    .unwrap_or_default();
                output.write_all(text.as_bytes())?;
                if !text.is_empty() && !text.ends_with('\n') {
                    writeln!(output)?;
                }
                state
                    .control
                    .mark_provider_shell_transcript_output(run_id, tool_id);
            }
            ActivityKind::Tool => {
                if matches!(
                    row.status.as_str(),
                    "called" | "requested" | "auto-approved"
                ) {
                    continue;
                }
                let tool_id = row.subject.as_str();
                let run_id = row.run_id.as_str();
                let Some(command) = state
                    .control
                    .provider_tool()
                    .command(run_id, tool_id)
                    .map(|command| command.command.clone())
                else {
                    continue;
                };
                if matches!(row.status.as_str(), "success" | "completed")
                    && state
                        .control
                        .provider_tool()
                        .output_text(run_id, tool_id)
                        .is_none_or(|text| text.trim().is_empty())
                {
                    continue;
                }
                if state
                    .control
                    .provider_shell_transcript_output_seen(run_id, tool_id)
                {
                    continue;
                }
                if state
                    .control
                    .claim_provider_shell_transcript_command(run_id, tool_id)
                {
                    writeln!(output, "$ {command}")?;
                }
                if !matches!(
                    row.status.as_str(),
                    "success" | "completed" | "auto-approved"
                ) {
                    writeln!(output, "tool status: {}", row.status)?;
                }
            }
            _ => {}
        }
    }

    output.flush()
}

pub(crate) fn render_activity_details_by_id<W: Write>(
    state: &InlineState,
    id: &str,
    output: &mut W,
) -> Option<std::io::Result<()>> {
    state
        .activity
        .rows
        .iter()
        .find(|row| row.id == id)
        .map(|row| render_activity_details_with_state(state, row, output))
}

fn render_activity_details_with_state<W: Write>(
    state: &InlineState,
    row: &RuntimeActivityRow,
    output: &mut W,
) -> std::io::Result<()> {
    let detail = activity_detail_for_render_with_state(state, row);
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_activity_details_panel(
            output,
            ActivityDetailsPanelModel {
                id: &row.id,
                run_id: &row.run_id,
                kind: row.kind.label(),
                status: &row.status,
                subject: &row.subject,
                summary: &row.summary,
                detail: &detail,
            },
        )?;
    Ok(())
}

fn should_render_activity_row(row: &RuntimeActivityRow, approval_mode: CoshApprovalMode) -> bool {
    match row.kind {
        ActivityKind::ShellHandoff => row.status != "completed",
        ActivityKind::ToolOutput => false,
        ActivityKind::Tool => {
            if activity_row_is_question_tool(row) {
                return false;
            }
            let needs_foreground_shell = row
                .detail
                .contains("interactive_hint: may_require_foreground_shell");
            if activity_row_is_shell_tool(row) {
                return needs_foreground_shell;
            }
            if approval_mode != CoshApprovalMode::Recommend
                && activity_row_is_control_permission(row)
                && row.status == "requested"
            {
                return false;
            }
            if approval_mode == CoshApprovalMode::Recommend
                && activity_row_is_control_permission(row)
                && row.status == "requested"
            {
                return true;
            }
            matches!(row.status.as_str(), "error" | "failed" | "interrupted")
                || needs_foreground_shell
        }
    }
}

fn should_render_activity_row_with_state(row: &RuntimeActivityRow, state: &InlineState) -> bool {
    if state.debug {
        return !activity_row_is_shell_output_or_completion(row);
    }
    if activity_row_has_visible_tool_card_owner(row, state) {
        return false;
    }
    should_render_activity_row(row, state.approval_mode)
}

fn activity_row_has_visible_tool_card_owner(row: &RuntimeActivityRow, state: &InlineState) -> bool {
    if !matches!(row.kind, ActivityKind::Tool) {
        return false;
    }
    state.activity.tool_invocations.iter().any(|record| {
        record.activity_row_ids.iter().any(|id| id == &row.id)
            && tool_invocation_card_for_record(record, state).is_some()
    })
}

fn activity_row_is_shell_output_or_completion(row: &RuntimeActivityRow) -> bool {
    match row.kind {
        ActivityKind::ToolOutput => row.detail.contains("provider_native_shell_command: "),
        ActivityKind::Tool => {
            activity_row_is_shell_tool(row)
                && matches!(row.status.as_str(), "success" | "completed")
        }
        _ => false,
    }
}

fn activity_row_is_shell_tool(row: &RuntimeActivityRow) -> bool {
    row.detail
        .lines()
        .find_map(|line| line.strip_prefix("tool_name: "))
        .is_some_and(is_shell_tool_name)
        || row.detail.contains("provider_native_shell_command: ")
        || row.detail.contains("provider_tool_class: shell")
        || row.subject == "Bash"
}

fn activity_row_is_question_tool(row: &RuntimeActivityRow) -> bool {
    row.detail
        .lines()
        .find_map(|line| line.strip_prefix("tool_name: "))
        .is_some_and(|name| {
            matches!(
                name,
                "ask_user_question" | "AskUserQuestion" | "ask_user" | "AskUser"
            )
        })
}

fn activity_row_is_control_permission(row: &RuntimeActivityRow) -> bool {
    row.detail.contains("evidence: ProviderToolRequest")
}

fn activity_detail_for_render(row: &RuntimeActivityRow, debug: bool) -> String {
    let mut detail = if debug {
        row.detail.clone()
    } else {
        row.detail
            .lines()
            .filter(|line| !line.starts_with("debug_output_ref: "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if let Some(audit_ref) = &row.audit_ref {
        detail.push_str(&format!("\naudit_ref: {audit_ref}"));
    }
    detail
}

fn activity_detail_for_render_with_state(state: &InlineState, row: &RuntimeActivityRow) -> String {
    if matches!(row.kind, ActivityKind::Tool) {
        if let Some(record) = state
            .activity
            .tool_invocations
            .iter()
            .find(|record| record.activity_row_ids.iter().any(|id| id == &row.id))
        {
            return typed_tool_detail_for_render(state, row, record);
        }
        if let Some(ActivityPresentation::Tool(presentation)) = &row.presentation {
            return typed_tool_row_presentation_detail(row, presentation);
        }
    }
    activity_detail_for_render(row, state.debug)
}

fn typed_tool_row_presentation_detail(
    row: &RuntimeActivityRow,
    presentation: &crate::tools::display::ToolPresentation,
) -> String {
    let mut lines = Vec::new();
    push_section(&mut lines, "Tool");
    push_field(&mut lines, "Canonical", &presentation.canonical_name);
    push_field(&mut lines, "Original", &presentation.original_name);
    push_field(
        &mut lines,
        "Classification",
        tool_kind_detail_label(presentation.kind),
    );
    push_field(
        &mut lines,
        "Impact",
        tool_impact_detail_label(presentation.impact),
    );
    push_section(&mut lines, "Target");
    if let Some(target) = &presentation.target {
        push_field(&mut lines, "Primary", target);
    }
    push_section(&mut lines, "Raw input");
    if let Some(raw) = &presentation.raw_input_preview {
        push_field(&mut lines, "Preview", raw);
    } else {
        push_field(&mut lines, "Preview", &presentation.preview);
    }
    push_section(&mut lines, "Audit row");
    push_field(&mut lines, "Detail", &row.detail);
    lines.join("\n")
}

fn typed_tool_detail_for_render(
    state: &InlineState,
    row: &RuntimeActivityRow,
    record: &ToolInvocationRecord,
) -> String {
    let mut lines = Vec::new();
    push_section(&mut lines, "Tool");
    push_field(&mut lines, "Canonical", &record.presentation.canonical_name);
    push_field(&mut lines, "Original", &record.presentation.original_name);
    push_field(
        &mut lines,
        "Classification",
        tool_kind_detail_label(record.presentation.kind),
    );
    push_field(
        &mut lines,
        "Impact",
        tool_impact_detail_label(record.presentation.impact),
    );

    push_section(&mut lines, "Target");
    if let Some(target) = &record.presentation.target {
        push_field(&mut lines, "Primary", target);
    }
    if let Some(secondary) = &record.presentation.secondary {
        push_field(&mut lines, "Secondary", secondary);
    }
    if !record.presentation.fields.is_empty() {
        for field in &record.presentation.fields {
            push_field(&mut lines, &field.label, &field.value);
        }
    }

    push_section(&mut lines, "Execution");
    push_field(&mut lines, "Invocation", &record.invocation_id);
    push_field(&mut lines, "Lifecycle", &record.lifecycle);
    push_field(&mut lines, "Row", &row.id);
    for key in [
        "provider",
        "execution_path",
        "request_id",
        "tool_use_id",
        "agent_result_visibility",
        "virtual_evidence_read_misroute",
        "misrouted_output_id",
        "recommended_action",
    ] {
        if let Some(value) = detail_value(&row.detail, key) {
            push_field(&mut lines, key, &value);
        }
    }

    push_section(&mut lines, "Result");
    push_field(&mut lines, "Status", &record.status);
    if let Some(result) = &record.result {
        push_field(&mut lines, "Headline", &result.headline);
        for metric in &result.metrics {
            push_field(&mut lines, "Metric", metric);
        }
        if let Some(action) = &result.action {
            push_field(&mut lines, "Action", action);
        }
    }
    if record.output.stdout_lines > 0 {
        push_field(
            &mut lines,
            "Stdout",
            &format!(
                "{} lines, {} bytes",
                record.output.stdout_lines, record.output.stdout_bytes
            ),
        );
    }
    if record.output.stderr_lines > 0 {
        push_field(
            &mut lines,
            "Stderr",
            &format!(
                "{} lines, {} bytes",
                record.output.stderr_lines, record.output.stderr_bytes
            ),
        );
    }
    if let Some(output_ref) = detail_value(&row.detail, "output_ref") {
        push_field(&mut lines, "Output ref", &output_ref);
    }
    if let Some(output_ref) = &record.output.output_ref {
        match output_ref {
            ToolOutputRef::TerminalOutputId(id) | ToolOutputRef::OpaqueAuditRef(id) => {
                push_field(&mut lines, "Output ref", id);
            }
            ToolOutputRef::DebugLocalPath { audit_ref, path } => {
                push_field(&mut lines, "Output ref", audit_ref);
                if state.debug {
                    push_field(&mut lines, "Debug output ref", path);
                }
            }
        }
    }
    if state.debug {
        if let Some(debug_ref) = detail_value(&row.detail, "debug_output_ref") {
            push_field(&mut lines, "Debug output ref", &debug_ref);
        }
    }
    if record.suppress_normal_card {
        push_field(
            &mut lines,
            "Visible surface",
            "suppressed by transcript or approval",
        );
    } else if record.phase == ToolInvocationPhase::Call {
        push_field(&mut lines, "Visible surface", "status line or audit only");
    } else {
        push_field(&mut lines, "Visible surface", "tool invocation card");
    }

    push_section(&mut lines, "Raw input");
    let raw_preview = if row.detail.contains("virtual_evidence_read_misroute: true") {
        Some("shell output bookmark".to_string())
    } else {
        record
            .presentation
            .raw_input_preview
            .clone()
            .or_else(|| detail_value(&row.detail, "input_preview"))
    };
    if let Some(raw) = raw_preview {
        push_field(&mut lines, "Preview", &raw);
    } else {
        push_field(
            &mut lines,
            "Preview",
            "structured input captured by presentation",
        );
    }
    if state.debug {
        push_field(
            &mut lines,
            "Audit row",
            &activity_detail_for_render(row, true),
        );
    }

    lines.join("\n")
}

fn push_section(lines: &mut Vec<String>, name: &str) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!("{name}:"));
}

fn push_field(lines: &mut Vec<String>, label: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() || value == "<none>" {
        return;
    }
    lines.push(format!("  {label}: {value}"));
}

fn detail_value(detail: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    detail
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "<none>")
        .map(ToString::to_string)
}

fn tool_kind_detail_label(kind: ToolPresentationKind) -> &'static str {
    match kind {
        ToolPresentationKind::ShellCommand => "shell",
        ToolPresentationKind::FileRead => "file-read",
        ToolPresentationKind::FileWrite => "file-write",
        ToolPresentationKind::FileEdit => "file-edit",
        ToolPresentationKind::FileSearch => "file-search",
        ToolPresentationKind::FileGlob => "file-glob",
        ToolPresentationKind::DirectoryList => "directory-list",
        ToolPresentationKind::MultiFileRead => "multi-file-read",
        ToolPresentationKind::Lsp => "lsp",
        ToolPresentationKind::WebFetch => "web-fetch",
        ToolPresentationKind::WebSearch => "web-search",
        ToolPresentationKind::Skill => "skill",
        ToolPresentationKind::Agent => "agent",
        ToolPresentationKind::Memory => "memory",
        ToolPresentationKind::Question => "question",
        ToolPresentationKind::ShellEvidence => "shell-evidence",
        ToolPresentationKind::Custom => "custom",
    }
}

fn tool_impact_detail_label(impact: ToolImpact) -> &'static str {
    match impact {
        ToolImpact::ReadOnly => "read-only",
        ToolImpact::Write => "write",
        ToolImpact::Execute => "execute",
        ToolImpact::Destructive => "destructive",
        ToolImpact::OpenWorld => "open-world",
        ToolImpact::ContextMutation => "context-mutation",
        ToolImpact::Unknown => "unknown",
    }
}

impl ActivityKind {
    fn label(self) -> &'static str {
        match self {
            Self::ToolOutput => "output",
            Self::Tool => "tool",
            Self::ShellHandoff => "shell",
        }
    }
}
