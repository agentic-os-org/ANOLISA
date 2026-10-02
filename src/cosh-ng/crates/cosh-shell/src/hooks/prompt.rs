use super::aggregate::{severity_label, AggregatedHookFinding};
use super::prelude::{CommandBlock, CommandStatus, I18n, MessageId};
use super::state::{PendingConsultation, RuntimeHookFinding};

const HOOK_ANALYSIS_EXCERPT_LINES: usize = 120;
const HOOK_ANALYSIS_EXCERPT_MAX_BYTES: usize = 12 * 1024;

pub(crate) fn hook_analysis_user_input(
    block: &CommandBlock,
    consultation: &PendingConsultation,
) -> String {
    let hook_id = consultation
        .hook_finding
        .as_ref()
        .map(|finding| finding.hook_id.as_str())
        .unwrap_or("unknown");
    let output_id = command_output_id(block);
    let mut prompt = format!(
        "Analyze hook finding `{hook_id}` for command `{}`. confidence={}; policy_reason={}; output_id={output_id}. Use included bounded evidence; terminal-output:// refs are not files. Do not execute follow-up commands automatically; route any command through existing command governance/approval.",
        block.command.trim(),
        consultation.confidence,
        consultation.display_reason
    );
    if consultation.confidence == "low" {
        prompt.push_str(
            " This finding is low confidence; first verify the evidence with read-only commands before giving a root-cause conclusion.",
        );
    }
    prompt.push_str(&hook_analysis_evidence_excerpt(block, &output_id));
    prompt
}

fn hook_analysis_evidence_excerpt(block: &CommandBlock, output_id: &str) -> String {
    let excerpt = crate::evidence::output_policy::bounded_output_excerpt_for_block(
        block,
        crate::evidence::model::OutputExcerptDirection::Tail,
        HOOK_ANALYSIS_EXCERPT_LINES,
        HOOK_ANALYSIS_EXCERPT_MAX_BYTES,
    );
    let output_excerpt_status =
        crate::evidence::output_policy::output_excerpt_status_for_block(block);
    let status = match block.status {
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    };
    let text = excerpt.text.as_deref().unwrap_or("<unavailable>");
    format!(
        "\n\nShellEvidenceExcerpt\n\
         output_id: {output_id}\n\
         command_id: {command_id}\n\
         command: {command}\n\
         cwd: {cwd}\n\
         end_cwd: {end_cwd}\n\
         status: {status}\n\
         exit_code: {exit_code}\n\
         duration_ms: {duration_ms}\n\
         output_bytes: {output_bytes}\n\
         output_excerpt_status: {output_excerpt_status}\n\
         direction: tail\n\
         lines_requested: {lines}\n\
         excerpt_status: {excerpt_status}\n\
         redaction_status: {redaction_status}\n\
         bounded_output_excerpt:\n{text}",
        command_id = block.id,
        command = block.command,
        cwd = block.cwd,
        end_cwd = block.end_cwd,
        exit_code = block.exit_code,
        duration_ms = block.duration_ms,
        output_bytes = block.output.terminal_output_bytes,
        lines = HOOK_ANALYSIS_EXCERPT_LINES,
        excerpt_status = excerpt.status,
        redaction_status = excerpt.redaction_status,
    )
}

pub(crate) fn prompt_hint_for_finding(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    _recommended_skill: Option<&str>,
) -> String {
    let output_id = command_output_id(block);
    let mut parts = vec![
        format!(
            "hook_finding={} severity={}",
            aggregate.primary.hook_id,
            severity_label(aggregate.primary.severity)
        ),
        aggregate.primary.title.clone(),
        format!("output_id={output_id}"),
    ];
    if let Some(cli_hint) = aggregate.primary.cli_hint.as_ref() {
        parts.push(format!("read_only_cli_hint={cli_hint}"));
    }
    if !aggregate.related.is_empty() {
        parts.push(format!("related_findings={}", aggregate.related.len()));
    }
    parts.push(
        "Use included bounded evidence; request more output through cosh-shell if needed."
            .to_string(),
    );
    parts.join("; ")
}

pub(crate) fn finding_markdown_for_aggregate(
    _block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    i18n: I18n,
) -> String {
    let mut lines = vec![
        format!("## {}", i18n.t(MessageId::HookFindingMarkdownTitle)),
        String::new(),
        i18n.format(
            MessageId::HookFindingMarkdownHookLine,
            &[("hook_id", aggregate.primary.hook_id.as_str())],
        ),
        i18n.format(
            MessageId::HookFindingMarkdownSeverityLine,
            &[("severity", severity_label(aggregate.primary.severity))],
        ),
        i18n.format(
            MessageId::HookFindingMarkdownFindingLine,
            &[("finding", aggregate.primary.title.as_str())],
        ),
        i18n.format(
            MessageId::HookFindingMarkdownSuggestionLine,
            &[("suggestion", aggregate.primary.suggestion.as_str())],
        ),
    ];
    if !aggregate.related.is_empty() {
        lines.push(
            i18n.t(MessageId::HookFindingMarkdownRelatedTitle)
                .to_string(),
        );
        lines.extend(aggregate.related.iter().map(|finding| {
            i18n.format(
                MessageId::HookFindingMarkdownRelatedLine,
                &[
                    ("hook_id", finding.hook_id.as_str()),
                    ("severity", severity_label(finding.severity)),
                    ("finding", finding.title.as_str()),
                ],
            )
        }));
    }
    lines.push(String::new());
    lines.push(
        i18n.t(MessageId::HookFindingMarkdownAgentFollowUpLine)
            .to_string(),
    );
    lines.join("\n")
}

pub(crate) fn format_runtime_hint(hint: &RuntimeHookFinding) -> String {
    let title = hint
        .hook_finding
        .as_ref()
        .map(|finding| finding.title.replace('`', ""))
        .unwrap_or_else(|| hint.prompt_hint.clone());
    format!(
        "{}\n{} block={} command={} ended_at_ms={} topic={} entity_key={} effective_severity={} confidence={} display={} reason={} suppression_key={} related_hook_ids={} {}",
        title,
        hint.id,
        hint.command_block_id,
        hint.command.trim(),
        hint.ended_at_ms,
        hint.topic,
        hint.entity_key,
        severity_label(hint.effective_severity),
        hint.confidence,
        hint.display.label(),
        hint.display_reason,
        hint.suppression_key,
        if hint.related_hook_ids.is_empty() {
            "<none>".to_string()
        } else {
            hint.related_hook_ids.join(",")
        },
        hint.prompt_hint
    )
}

fn command_output_id(block: &CommandBlock) -> String {
    if block.output.terminal_output_ref.is_some() {
        format!("terminal-output://{}/{}", block.session_id, block.id)
    } else {
        "<missing>".to_string()
    }
}
