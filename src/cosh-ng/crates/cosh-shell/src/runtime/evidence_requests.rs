use std::collections::{HashSet, VecDeque};

use crate::agent::run::ActiveAgentRun;
use crate::evidence::model::{EvidenceExcerptRequest, OutputExcerptDirection};
use crate::evidence::output_policy::{
    bounded_output_excerpt_for_id, output_excerpt_status_for_block, parse_terminal_output_id,
    terminal_output_id,
};
use crate::evidence::request::{CoshRequest, ParsedCoshRequest};
use crate::evidence::stream::{CoshRequestAuditOutcome, CoshRequestAuditRecord};
use crate::runtime::prelude::*;

const DEFAULT_OUTPUT_LINES: usize = 120;
const MAX_OUTPUT_LINES: usize = 300;
const MAX_OUTPUT_BYTES: usize = 12 * 1024;
const HISTORY_LIMIT: usize = 20;

#[derive(Debug, Default)]
pub(crate) struct EvidenceRequestState {
    pub(super) pending: VecDeque<RuntimeEvidenceRequest>,
    pub(super) rendered: HashSet<String>,
    handled_actions: HashSet<String>,
    pub(super) audit_records: Vec<RuntimeCoshRequestAudit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeEvidenceRequest {
    pub(super) id: String,
    pub(super) origin: AgentRunOrigin,
    pub(super) kind: RuntimeEvidenceRequestKind,
    pub(super) ignored_multiple_request_blocks: bool,
    pub(super) audit_id: Option<String>,
    pub(super) audit_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeCoshRequestAudit {
    pub(crate) id: String,
    pub(crate) run_id: String,
    pub(crate) outcome: CoshRequestAuditOutcome,
    pub(crate) reason: &'static str,
    pub(crate) raw_block: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeEvidenceRequestKind {
    History,
    Output(EvidenceExcerptRequest),
}

pub(crate) struct RecordedEvidenceRequests {
    pub(crate) auto_requests: Vec<(AgentRequest, AgentRunOrigin)>,
    pub(crate) card_ids: Vec<String>,
    pub(crate) notices: Vec<String>,
}

pub(crate) fn record_cosh_requests_from_active_run(
    state: &mut InlineState,
    active_run: &mut ActiveAgentRun,
) -> RecordedEvidenceRequests {
    let mut recorded = RecordedEvidenceRequests {
        auto_requests: Vec::new(),
        card_ids: Vec::new(),
        notices: Vec::new(),
    };
    let parsed_requests = std::mem::take(&mut active_run.pending_cosh_requests);
    let first_parsed_audit_id = record_cosh_request_audits(
        state,
        &active_run.request.id,
        &mut active_run.pending_cosh_request_audits,
    );
    let mut parsed_requests = parsed_requests.into_iter();
    let Some(mut parsed) = parsed_requests.next() else {
        return recorded;
    };
    if parsed_requests.next().is_some() {
        parsed.ignored_multiple_request_blocks = true;
    }
    let id = format!("evidence-{}", state.evidence_requests.pending.len() + 1);
    let request = runtime_request_from_parsed(id.clone(), parsed, active_run.origin);
    let request = RuntimeEvidenceRequest {
        audit_id: first_parsed_audit_id,
        ..request
    };
    if active_run_has_unclosed_provider_tool_turn(active_run) {
        recorded.notices.push(
            "deferred evidence request because the provider tool turn is still open".to_string(),
        );
        return recorded;
    }
    if matches!(&request.kind, RuntimeEvidenceRequestKind::History)
        && !history_request_needs_confirmation(state)
    {
        match agent_request_from_history_request(&state.session_blocks, id_sequence(&id)) {
            Ok(agent_request) => recorded.auto_requests.push((agent_request, request.origin)),
            Err(message) => recorded.notices.push(message),
        }
    } else {
        state.evidence_requests.pending.push_back(request);
        recorded.card_ids.push(id);
    }
    recorded
}

pub(crate) fn cosh_request_audit_by_id<'a>(
    state: &'a InlineState,
    id: &str,
) -> Option<&'a RuntimeCoshRequestAudit> {
    state
        .evidence_requests
        .audit_records
        .iter()
        .find(|record| record.id == id)
}

pub(crate) fn pending_evidence_capture(state: &InlineState) -> Option<RawInputCapture> {
    state
        .evidence_requests
        .pending
        .front()
        .map(|request| RawInputCapture::Evidence {
            id: request.id.clone(),
        })
}

pub(crate) fn clear_pending_evidence_requests(state: &mut InlineState) {
    state.evidence_requests.pending.clear();
    state.evidence_requests.rendered.clear();
}

pub(crate) fn render_pending_evidence_requests<W: Write>(
    state: &mut InlineState,
    ids: &[String],
    output: &mut W,
) -> std::io::Result<()> {
    for id in ids {
        if !state.evidence_requests.rendered.insert(id.clone()) {
            continue;
        }
        let Some(request) = state
            .evidence_requests
            .pending
            .iter()
            .find(|request| request.id == *id)
        else {
            continue;
        };
        render_evidence_request_card(state.language, request, output)?;
    }
    Ok(())
}

pub(crate) fn render_evidence_request_actions<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some(action) = evidence_action_from_event(event) else {
            continue;
        };
        let key = stable_event_key("evidence-request", event_index, event);
        if !state.evidence_requests.handled_actions.insert(key) {
            continue;
        }
        // Sending an evidence card is an explicit user request. During a
        // background compaction the Agent is paused, so do NOT consume the
        // pending card here: consuming it and then having the start gate defer
        // the run would strand the user's action. Leave the card pending, show
        // the paused notice, and let the user send it again after compaction.
        if action.kind == EvidenceActionKind::Send
            && crate::slash::session::compaction_pending_or_active(state)
        {
            crate::slash::session::render_compaction_paused_notice(state, output)?;
            crate::slash::prompt::write_shell_prompt(state, output)?;
            output.flush()?;
            continue;
        }
        let Some(mut request) = take_pending_request(state, &action.id) else {
            continue;
        };
        state.evidence_requests.rendered.remove(&request.id);
        match action.kind {
            EvidenceActionKind::Send => {
                match agent_request_from_evidence_request(blocks, &request, event_index) {
                    Ok(agent_request) => {
                        if let Some(audit) = state.audit.as_mut() {
                            let (evidence_type, size_category, range_category) =
                                evidence_audit_categories(&request.kind);
                            request.audit_ref = audit.record_evidence_accessed(
                                evidence_type,
                                size_category,
                                range_category,
                                true,
                            );
                        }
                        let origin = request.origin;
                        let disposition = start_agent_run_with_origin_disposition(
                            &agent_request,
                            origin,
                            AgentStartIntent::UserInitiated,
                            adapter,
                            state,
                            output,
                            Some(event_index),
                        )?;
                        if disposition == AgentStartDisposition::QueueFull {
                            // The queue was full: restore the consumed card so
                            // the user's send is not lost, and surface why.
                            state.evidence_requests.pending.push_front(request);
                            crate::slash::session::render_agent_queue_full_notice(state, output)?;
                        }
                    }
                    Err(message) => {
                        render_evidence_notice(
                            state.language,
                            evidence_notice_title(state.language),
                            &message,
                            output,
                        )?;
                    }
                }
            }
            EvidenceActionKind::Ignore => {
                state.agent_run.needs_prompt_after_run = false;
                state.trigger_pty_prompt = false;
                render_evidence_notice(
                    state.language,
                    evidence_notice_title(state.language),
                    evidence_ignored_body(state.language),
                    output,
                )?;
            }
            EvidenceActionKind::Cancel => {
                state.agent_run.needs_prompt_after_run = false;
                state.trigger_pty_prompt = false;
            }
        }
        output.flush()?;
    }
    Ok(())
}

fn runtime_request_from_parsed(
    id: String,
    parsed: ParsedCoshRequest,
    origin: AgentRunOrigin,
) -> RuntimeEvidenceRequest {
    let kind = match parsed.request {
        CoshRequest::History => RuntimeEvidenceRequestKind::History,
        CoshRequest::Output(request) => RuntimeEvidenceRequestKind::Output(request),
    };
    RuntimeEvidenceRequest {
        id,
        origin,
        kind,
        ignored_multiple_request_blocks: parsed.ignored_multiple_request_blocks,
        audit_id: None,
        audit_ref: None,
    }
}

fn history_request_needs_confirmation(state: &InlineState) -> bool {
    if state.approval_mode == CoshApprovalMode::Recommend {
        return true;
    }
    state
        .session_blocks
        .iter()
        .rev()
        .take(HISTORY_LIMIT)
        .any(|block| {
            let redacted = redact_provider_command_text(&block.command);
            redacted != block.command
        })
}

fn active_run_has_unclosed_provider_tool_turn(active_run: &ActiveAgentRun) -> bool {
    let mut open = HashSet::new();
    let mut unknown_open_tool_call = false;
    for event in &active_run.governed_events {
        match &event.event {
            AgentEvent::ToolCall { tool_id, .. } => {
                if let Some(tool_id) = tool_id {
                    open.insert(tool_id.clone());
                } else {
                    unknown_open_tool_call = true;
                }
            }
            AgentEvent::ToolPermissionRequest { tool_use_id, .. } => {
                open.insert(tool_use_id.clone());
            }
            AgentEvent::ToolCompleted { tool_id, .. } => {
                open.remove(tool_id);
            }
            _ => {}
        }
    }
    unknown_open_tool_call || !open.is_empty()
}

fn record_cosh_request_audits(
    state: &mut InlineState,
    run_id: &str,
    audit_records: &mut Vec<CoshRequestAuditRecord>,
) -> Option<String> {
    let mut first_parsed = None;
    for audit in audit_records.drain(..) {
        let id = format!(
            "cosh-request-{}",
            state.evidence_requests.audit_records.len() + 1
        );
        if audit.outcome == CoshRequestAuditOutcome::Parsed && first_parsed.is_none() {
            first_parsed = Some(id.clone());
        }
        state
            .evidence_requests
            .audit_records
            .push(RuntimeCoshRequestAudit {
                id,
                run_id: run_id.to_string(),
                outcome: audit.outcome,
                reason: audit.reason,
                raw_block: audit.raw_block,
            });
    }
    first_parsed
}

fn id_sequence(id: &str) -> usize {
    id.strip_prefix("evidence-")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn render_evidence_request_card<W: Write>(
    language: Language,
    request: &RuntimeEvidenceRequest,
    output: &mut W,
) -> std::io::Result<()> {
    let body = match &request.kind {
        RuntimeEvidenceRequestKind::History => vec![
            evidence_request_history_body(language).to_string(),
            evidence_request_actions_body(language).to_string(),
        ],
        RuntimeEvidenceRequestKind::Output(request) => vec![
            format!(
                "{} {} {}",
                evidence_request_output_body(language),
                request.output_id,
                direction_label(request.direction)
            ),
            format!(
                "{} {}",
                evidence_request_lines_body(language),
                request.lines.unwrap_or(DEFAULT_OUTPUT_LINES)
            ),
            evidence_request_actions_body(language).to_string(),
        ],
    };
    let footer = request
        .audit_ref
        .as_ref()
        .map(|id| format!("audit_ref: {id}"))
        .or_else(|| request.audit_id.as_ref().map(|id| format!("Details: {id}")))
        .or_else(|| {
            request
                .ignored_multiple_request_blocks
                .then(|| evidence_multiple_footer(language).to_string())
        });
    RatatuiInlineRenderer::for_terminal()
        .with_language(language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title: evidence_request_title(language),
                body,
                footer: footer.as_deref(),
            },
        )
}

fn evidence_audit_categories(
    kind: &RuntimeEvidenceRequestKind,
) -> (&'static str, Option<&'static str>, Option<&'static str>) {
    match kind {
        RuntimeEvidenceRequestKind::History => ("command_history", Some("bounded"), None),
        RuntimeEvidenceRequestKind::Output(request) => (
            "command_output",
            Some("bounded"),
            Some(match request.direction {
                OutputExcerptDirection::Head => "head",
                OutputExcerptDirection::Tail => "tail",
            }),
        ),
    }
}

pub(super) fn agent_request_from_evidence_request(
    blocks: &[CommandBlock],
    request: &RuntimeEvidenceRequest,
    sequence: usize,
) -> Result<AgentRequest, String> {
    match &request.kind {
        RuntimeEvidenceRequestKind::History => agent_request_from_history_request(blocks, sequence),
        RuntimeEvidenceRequestKind::Output(output) => {
            agent_request_from_output_request(blocks, output, sequence)
        }
    }
}

fn agent_request_from_history_request(
    blocks: &[CommandBlock],
    sequence: usize,
) -> Result<AgentRequest, String> {
    let Some(anchor) = blocks.last() else {
        return Err("no shell history is available".to_string());
    };
    let history = blocks
        .iter()
        .rev()
        .take(HISTORY_LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|block| {
            let status = match block.status {
                CommandStatus::Completed => "completed",
                CommandStatus::Failed => "failed",
            };
            format!(
                "- command_id: {id}; output_id: {output_id}; status: {status}; exit_code: {exit_code}; command: {command}",
                id = block.id,
                output_id = terminal_output_id(&block.session_id, &block.id),
                exit_code = block.exit_code,
                command = redact_provider_command_text(&block.command)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut request = AgentRequest {
        id: format!("evidence-history-{sequence}"),
        session_id: anchor.session_id.clone(),
        command_block: anchor.clone(),
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some(format!(
            "ShellEvidenceExcerpt\nhistory_limit: {HISTORY_LIMIT}\nhistory_index:\n{history}"
        )),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    crate::types::set_request_context_binding(
        &mut request,
        AgentContextBinding::ControlProtocolEvidence,
    );
    Ok(request)
}

fn agent_request_from_output_request(
    blocks: &[CommandBlock],
    request: &EvidenceExcerptRequest,
    sequence: usize,
) -> Result<AgentRequest, String> {
    let parsed = parse_terminal_output_id(&request.output_id)
        .ok_or_else(|| format!("invalid output id: {}", request.output_id))?;
    let block = blocks
        .iter()
        .find(|block| block.session_id == parsed.shell_session_id && block.id == parsed.command_id)
        .ok_or_else(|| {
            format!(
                "output id is not part of this shell session: {}",
                request.output_id
            )
        })?;
    if block.output.terminal_output_ref.is_none() {
        return Err(format!("no captured output for {}", request.output_id));
    }
    let lines = request
        .lines
        .unwrap_or(DEFAULT_OUTPUT_LINES)
        .min(MAX_OUTPUT_LINES);
    let excerpt = bounded_output_excerpt_for_id(
        blocks,
        &request.output_id,
        request.direction,
        lines,
        MAX_OUTPUT_BYTES,
    );
    let Some(text) = excerpt.text.as_deref() else {
        return Err(format!(
            "captured output is unavailable: {}",
            request.output_id
        ));
    };
    let status = match block.status {
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    };
    let output_excerpt_status = output_excerpt_status_for_block(block);
    let user_input = format!(
        "ShellEvidenceExcerpt\n\
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
         direction: {direction}\n\
         lines_requested: {lines}\n\
         excerpt_status: {excerpt_status}\n\
         redaction_status: {redaction_status}\n\
         bounded_output_excerpt:\n{text}",
        output_id = request.output_id,
        command_id = block.id,
        command = redact_provider_command_text(&block.command),
        cwd = block.cwd,
        end_cwd = block.end_cwd,
        exit_code = block.exit_code,
        duration_ms = block.duration_ms,
        output_bytes = block.output.terminal_output_bytes,
        output_excerpt_status = output_excerpt_status,
        direction = direction_label(request.direction),
        excerpt_status = excerpt.status,
        redaction_status = excerpt.redaction_status,
    );
    let mut request = AgentRequest {
        id: format!("evidence-output-{sequence}"),
        session_id: block.session_id.clone(),
        command_block: block.clone(),
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some(user_input),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    crate::types::set_request_context_binding(
        &mut request,
        AgentContextBinding::ControlProtocolEvidence,
    );
    Ok(request)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvidenceAction {
    id: String,
    kind: EvidenceActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceActionKind {
    Send,
    Ignore,
    Cancel,
}

fn evidence_action_from_event(event: &ShellEvent) -> Option<EvidenceAction> {
    if event.component.as_deref() != Some("card") {
        return None;
    }
    let id = event.input.clone()?;
    let kind = match event.message.as_deref()? {
        "evidence_send" => EvidenceActionKind::Send,
        "evidence_ignore" => EvidenceActionKind::Ignore,
        "evidence_cancel" => EvidenceActionKind::Cancel,
        _ => return None,
    };
    Some(EvidenceAction { id, kind })
}

fn take_pending_request(state: &mut InlineState, id: &str) -> Option<RuntimeEvidenceRequest> {
    let index = state
        .evidence_requests
        .pending
        .iter()
        .position(|request| request.id == id)?;
    state.evidence_requests.pending.remove(index)
}

fn render_evidence_notice<W: Write>(
    language: Language,
    title: &str,
    body: &str,
    output: &mut W,
) -> std::io::Result<()> {
    RatatuiInlineRenderer::for_terminal()
        .with_language(language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title,
                body: vec![body.to_string()],
                footer: None,
            },
        )
}

fn direction_label(direction: OutputExcerptDirection) -> &'static str {
    match direction {
        OutputExcerptDirection::Head => "head",
        OutputExcerptDirection::Tail => "tail",
    }
}

fn evidence_request_title(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "Agent 请求更多证据",
        Language::EnUs => "Agent Requested Evidence",
    }
}

fn evidence_request_history_body(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "Agent 想查看最近的 shell 命令索引。",
        Language::EnUs => "Agent wants to inspect the recent shell command index.",
    }
}

fn evidence_request_output_body(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "Agent 想查看捕获输出:",
        Language::EnUs => "Agent wants to inspect captured output:",
    }
}

fn evidence_request_lines_body(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "最大行数:",
        Language::EnUs => "Max lines:",
    }
}

fn evidence_request_actions_body(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "Enter 发送片段 · i 忽略 · Esc/Ctrl+C 取消",
        Language::EnUs => "Enter sends excerpt · i ignores · Esc/Ctrl+C cancels",
    }
}

fn evidence_multiple_footer(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "同一回复中的其它请求已忽略。",
        Language::EnUs => "Other requests in the same response were ignored.",
    }
}

fn evidence_notice_title(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "证据请求",
        Language::EnUs => "Evidence Request",
    }
}

fn evidence_ignored_body(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "已忽略这次证据请求。",
        Language::EnUs => "Ignored this evidence request.",
    }
}
