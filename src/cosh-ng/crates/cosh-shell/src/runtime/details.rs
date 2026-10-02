use crate::activity::runtime::render_activity_details_by_id;
use crate::approval::cards::{render_approval_details, render_approval_journal};
use crate::evidence::model::OutputExcerptDirection;
use crate::evidence::output_policy::{
    bounded_output_excerpt_for_block, output_excerpt_status_for_block, parse_terminal_output_id,
    terminal_output_id,
};
use crate::evidence::stream::CoshRequestAuditOutcome;
use crate::runtime::evidence_requests::{cosh_request_audit_by_id, RuntimeCoshRequestAudit};
use crate::runtime::prelude::*;
#[cfg(test)]
use crate::types::HookFinding;

mod command;

use command::render_command_details;

const DETAILS_AGENT_DEFAULT_LINES: usize = 120;
const DETAILS_AGENT_MAX_LINES: usize = 300;
const DETAILS_AGENT_MAX_BYTES: usize = 12 * 1024;

pub(crate) fn render_runtime_details<W: Write>(
    state: &InlineState,
    blocks: &[CommandBlock],
    id: &str,
    output: &mut W,
) -> std::io::Result<()> {
    if id == "approvals" {
        return render_approval_journal(state, output);
    }

    if let Some(request) = state
        .approvals
        .requests
        .iter()
        .find(|request| request.id == id)
    {
        return render_approval_details(state.language, request, output);
    }

    if let Some(result) = render_activity_details_by_id(state, id, output) {
        return result;
    }

    if let Some(record) = cosh_request_audit_by_id(state, id) {
        return render_cosh_request_audit_details(state, record, output);
    }

    if let Some(record) = state.provider_cancellation_artifacts.by_id(id) {
        return render_provider_cancellation_artifact_details(state, record, output);
    }

    if let Some(consultation) = state
        .hooks
        .findings
        .iter()
        .find(|finding| finding.id == id)
        .and_then(crate::hooks::queue::consultation_from_hint)
    {
        return crate::hooks::render_consultation_details(
            &consultation,
            state.i18n(),
            state.debug,
            output,
        );
    }

    if let Some(block) = command_block_by_details_id(blocks, id) {
        return render_command_details(state, block, output);
    }

    let i18n = state.i18n();
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title: i18n.t(MessageId::RuntimeDetailsUnavailableTitle),
            body: vec![i18n.format(MessageId::RuntimeDetailsUnavailableBody, &[("id", id)])],
            footer: None,
        },
    )
}

pub(crate) fn render_runtime_details_card_actions<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        if !event_requests_runtime_details(event) {
            continue;
        }

        let event_index = event_index_base + idx;
        let key = stable_event_key("runtime-details", event_index, event);
        if !state.handled_details_actions.insert(key) {
            continue;
        }

        let Some(id) = event
            .input
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        render_runtime_details(state, blocks, id, output)?;
        output.flush()?;
    }

    Ok(())
}

fn event_requests_runtime_details(event: &ShellEvent) -> bool {
    event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some("card")
        && matches!(
            event.message.as_deref(),
            Some("runtime_details" | "activity_details")
        )
}

fn render_provider_cancellation_artifact_details<W: Write>(
    state: &InlineState,
    record: &crate::runtime::provider_cancellation_artifacts::RuntimeProviderCancellationArtifactRecord,
    output: &mut W,
) -> std::io::Result<()> {
    let title = match state.language {
        Language::ZhCn => "Provider cancel 详情",
        Language::EnUs => "Provider cancel details",
    };
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title,
            body: record.detail_lines(),
            footer: None,
        },
    )
}

fn render_cosh_request_audit_details<W: Write>(
    state: &InlineState,
    record: &RuntimeCoshRequestAudit,
    output: &mut W,
) -> std::io::Result<()> {
    let title = match state.language {
        Language::ZhCn => "cosh-request 详情",
        Language::EnUs => "cosh-request details",
    };
    let outcome = match record.outcome {
        CoshRequestAuditOutcome::Parsed => "parsed",
        CoshRequestAuditOutcome::Invalid => "invalid",
    };
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title,
            body: vec![
                format!("request_id: {}", record.id),
                format!("run_id: {}", record.run_id),
                format!("outcome: {outcome}"),
                format!("reason: {}", record.reason),
                "raw_block:".to_string(),
                record.raw_block.clone(),
            ],
            footer: None,
        },
    )
}

pub(crate) fn agent_request_from_details_input(
    blocks: &[CommandBlock],
    input: &str,
    sequence: usize,
) -> Option<Result<AgentRequest, String>> {
    let parsed = parse_details_agent_input(input)?;
    Some(build_details_agent_request(blocks, parsed, sequence))
}

fn parse_details_agent_input(input: &str) -> Option<DetailsAgentInput<'_>> {
    let mut tokens = input.split_whitespace();
    if tokens.next()? != "/details" {
        return None;
    }
    let id = tokens.next()?;
    if tokens.next()? != "--agent" {
        return None;
    }
    let mut direction = OutputExcerptDirection::Tail;
    let mut lines = DETAILS_AGENT_DEFAULT_LINES;
    match tokens.next() {
        None => {}
        Some("--head") => {
            direction = OutputExcerptDirection::Head;
            lines = parse_details_agent_lines(tokens.next())?;
        }
        Some("--tail") => {
            direction = OutputExcerptDirection::Tail;
            lines = parse_details_agent_lines(tokens.next())?;
        }
        Some(_) => return None,
    }
    if tokens.next().is_some() {
        return None;
    }
    Some(DetailsAgentInput {
        id,
        direction,
        lines,
    })
}

fn parse_details_agent_lines(token: Option<&str>) -> Option<usize> {
    let parsed = token?.parse::<usize>().ok()?;
    (parsed > 0).then_some(parsed.min(DETAILS_AGENT_MAX_LINES))
}

struct DetailsAgentInput<'a> {
    id: &'a str,
    direction: OutputExcerptDirection,
    lines: usize,
}

fn build_details_agent_request(
    blocks: &[CommandBlock],
    parsed: DetailsAgentInput<'_>,
    sequence: usize,
) -> Result<AgentRequest, String> {
    let block = command_block_by_details_id(blocks, parsed.id)
        .ok_or_else(|| format!("details target not found: {}", parsed.id))?;
    if block.output.terminal_output_ref.is_none() {
        return Err(format!(
            "details target has no captured output: {}",
            parsed.id
        ));
    }

    let output_id = terminal_output_id(&block.session_id, &block.id);
    let excerpt = bounded_output_excerpt_for_block(
        block,
        parsed.direction,
        parsed.lines,
        DETAILS_AGENT_MAX_BYTES,
    );
    let Some(text) = excerpt.text.as_deref() else {
        return Err(format!(
            "details target output is unavailable: {}",
            parsed.id
        ));
    };
    let direction = match parsed.direction {
        OutputExcerptDirection::Head => "head",
        OutputExcerptDirection::Tail => "tail",
    };
    let output_excerpt_status = output_excerpt_status_for_block(block);
    let status = match block.status {
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    };
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
        command_id = block.id,
        command = block.command,
        cwd = block.cwd,
        end_cwd = block.end_cwd,
        exit_code = block.exit_code,
        duration_ms = block.duration_ms,
        output_bytes = block.output.terminal_output_bytes,
        output_excerpt_status = output_excerpt_status,
        lines = parsed.lines,
        excerpt_status = excerpt.status,
        redaction_status = excerpt.redaction_status,
    );

    let mut request = AgentRequest {
        id: format!("details-evidence-{sequence}"),
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
    crate::types::set_request_context_binding(&mut request, AgentContextBinding::SelectedCommand);
    Ok(request)
}

fn command_block_by_details_id<'a>(
    blocks: &'a [CommandBlock],
    id: &str,
) -> Option<&'a CommandBlock> {
    if let Some(parsed) = parse_terminal_output_id(id) {
        return blocks.iter().find(|block| {
            block.session_id == parsed.shell_session_id && block.id == parsed.command_id
        });
    }
    blocks.iter().find(|block| block.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::runtime::{ActivityKind, RuntimeActivityRow};

    #[test]
    fn details_renders_command_block_with_opaque_output_id() {
        let state = InlineState::default();
        let output_ref = test_output_ref("details-available", "one\ntwo\n");
        let block = command_block(
            "session-1",
            "cmd-1",
            Some(output_ref.to_str().expect("utf8 output path")),
        );
        let mut output = Vec::new();

        render_runtime_details(&state, &[block], "cmd-1", &mut output).expect("render details");
        let _ = std::fs::remove_file(&output_ref);

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(rendered.contains("Command details"), "{rendered}");
        assert!(rendered.contains("command_id: cmd-1"), "{rendered}");
        assert!(
            rendered.contains("output_id: terminal-output://session-1/cmd-1"),
            "{rendered}"
        );
        assert!(
            rendered.contains("output_excerpt_status: available"),
            "{rendered}"
        );
        assert!(
            !rendered.contains(output_ref.to_str().unwrap()),
            "{rendered}"
        );
        let _ = std::fs::remove_file(output_ref);
    }

    #[test]
    fn details_card_action_renders_activity_details() {
        let mut state = InlineState::default();
        state.activity.rows.push(RuntimeActivityRow {
            id: "tool-1".to_string(),
            audit_ref: None,
            run_id: "run-1".to_string(),
            kind: ActivityKind::Tool,
            status: "called".to_string(),
            subject: "Read".to_string(),
            summary: "Read called".to_string(),
            detail: "tool_name: Read\ninput_preview: README.md".to_string(),
            presentation: None,
        });
        let mut event = ShellEvent::user_input_intercepted("session-1", "tool-1");
        event.component = Some("card".to_string());
        event.message = Some("activity_details".to_string());
        let mut output = Vec::new();

        render_runtime_details_card_actions(&[event], &[], &mut state, &mut output, 0)
            .expect("render card details");

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(rendered.contains("Activity details tool-1"), "{rendered}");
        assert!(rendered.contains("tool_name: Read"), "{rendered}");
    }

    #[test]
    fn details_marks_capture_truncated_without_internal_path() {
        let state = InlineState::default();
        let output_ref = test_output_ref("details-truncated", "one\ntwo\n");
        let mut block = command_block(
            "session-1",
            "cmd-1",
            Some(output_ref.to_str().expect("utf8 output path")),
        );
        block.output.terminal_output_bytes = COMMAND_OUTPUT_REF_MAX_BYTES as u64 + 1;
        let mut output = Vec::new();

        render_runtime_details(&state, &[block], "cmd-1", &mut output).expect("render details");
        let _ = std::fs::remove_file(&output_ref);

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(
            rendered.contains("output_excerpt_status: truncated_at_capture"),
            "{rendered}"
        );
        assert!(
            rendered.contains("output_id: terminal-output://session-1/cmd-1"),
            "{rendered}"
        );
        assert!(
            !rendered.contains(output_ref.to_str().unwrap()),
            "{rendered}"
        );
        let _ = std::fs::remove_file(output_ref);
    }

    #[test]
    fn details_marks_missing_output_unavailable() {
        let state = InlineState::default();
        let block = command_block("session-1", "cmd-1", None);
        let mut output = Vec::new();

        render_runtime_details(&state, &[block], "cmd-1", &mut output).expect("render details");

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(
            rendered.contains("output_excerpt_status: unavailable"),
            "{rendered}"
        );
        assert!(rendered.contains("output_id: <none>"), "{rendered}");
    }

    #[test]
    fn details_accepts_full_terminal_output_id_for_command_block() {
        let state = InlineState::default();
        let block = command_block("session-1", "cmd-2", Some("/tmp/internal-output-ref.txt"));
        let mut output = Vec::new();

        render_runtime_details(
            &state,
            &[block],
            "terminal-output://session-1/cmd-2",
            &mut output,
        )
        .expect("render details");

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(rendered.contains("command_id: cmd-2"), "{rendered}");
        assert!(
            !rendered.contains("/tmp/internal-output-ref.txt"),
            "{rendered}"
        );
    }

    #[test]
    fn details_rejects_cross_session_terminal_output_id() {
        let state = InlineState::default();
        let block = command_block("session-1", "cmd-1", Some("/tmp/internal-output-ref.txt"));
        let mut output = Vec::new();

        render_runtime_details(
            &state,
            &[block],
            "terminal-output://session-2/cmd-1",
            &mut output,
        )
        .expect("render details");

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(rendered.contains("Details unavailable"), "{rendered}");
        assert!(
            !rendered.contains("/tmp/internal-output-ref.txt"),
            "{rendered}"
        );
    }

    #[test]
    fn details_renders_hook_finding_by_id_without_internal_output_ref() {
        let mut state = InlineState::default();
        state
            .hooks
            .findings
            .push(crate::hooks::state::RuntimeHookFinding {
                id: "hook-cmd-1-memory-pressure".to_string(),
                command_block_id: "cmd-1".to_string(),
                command: "free -m".to_string(),
                output_ref: Some("/tmp/internal-hook-output.txt".to_string()),
                ended_at_ms: 2,
                prompt_hint:
                    "hook_finding=memory-pressure output_id=terminal-output://session-1/cmd-1"
                        .to_string(),
                finding_markdown: None,
                hook_finding: Some(HookFinding {
                    hook_id: "memory-pressure".to_string(),
                    severity: FindingSeverity::Critical,
                    title: "Available memory is low".to_string(),
                    description: "description".to_string(),
                    suggestion: "suggestion".to_string(),
                    skill: Some("memory-analysis".to_string()),
                    cli_hint: None,
                    context_refs: Vec::new(),
                }),
                recommended_skill: Some("memory-analysis".to_string()),
                display: crate::hooks::state::RuntimeHookDisplay::Consultation,
                display_reason: "allowed".to_string(),
                related_hook_ids: Vec::new(),
                topic: "memory".to_string(),
                entity_key: "system-memory".to_string(),
                effective_severity: FindingSeverity::Critical,
                confidence: "high".to_string(),
                suppression_key: "memory:memory-pressure:free".to_string(),
            });
        let mut output = Vec::new();

        render_runtime_details(&state, &[], "hook-cmd-1-memory-pressure", &mut output)
            .expect("render hook details");

        let rendered = String::from_utf8(output).expect("utf8 details");
        assert!(rendered.contains("Hook finding details"), "{rendered}");
        assert!(rendered.contains("Output capture: captured"), "{rendered}");
        assert!(
            rendered.contains("terminal-output://session-1/cmd-1"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("/tmp/internal-hook-output.txt"),
            "{rendered}"
        );
    }

    #[test]
    fn details_agent_request_injects_bounded_excerpt_without_path() {
        let dir =
            std::env::temp_dir().join(format!("cosh-shell-details-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let output_ref = dir.join("cmd-1.txt");
        std::fs::write(&output_ref, "one\ntwo\nthree\n").expect("write output");
        let block = command_block(
            "session-1",
            "cmd-1",
            Some(output_ref.to_str().expect("utf8 output path")),
        );

        let request =
            agent_request_from_details_input(&[block], "/details cmd-1 --agent --tail 2", 7)
                .expect("details agent syntax")
                .expect("details agent request");

        assert_eq!(request.id, "details-evidence-7");
        assert_eq!(request.mode, AgentMode::RecommendOnly);
        assert!(request.user_confirmed);
        let input = request.user_input.expect("user input");
        assert!(input.starts_with("ShellEvidenceExcerpt\n"), "{input}");
        assert!(
            input.contains("output_id: terminal-output://session-1/cmd-1"),
            "{input}"
        );
        assert!(input.contains("direction: tail"), "{input}");
        assert!(input.contains("lines_requested: 2"), "{input}");
        assert!(
            input.contains("bounded_output_excerpt:\ntwo\nthree"),
            "{input}"
        );
        assert!(!input.contains(output_ref.to_str().unwrap()), "{input}");
    }

    #[test]
    fn details_agent_request_keeps_selected_command_context_bound() {
        let dir = std::env::temp_dir().join(format!(
            "cosh-shell-details-selected-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let output_ref = dir.join("cmd-1.txt");
        std::fs::write(&output_ref, "Killed\n").expect("write output");
        let mut block = command_block(
            "session-1",
            "cmd-1",
            Some(output_ref.to_str().expect("utf8 output path")),
        );
        block.status = CommandStatus::Failed;
        block.exit_code = 137;
        let mut request =
            agent_request_from_details_input(&[block], "/details cmd-1 --agent --tail 1", 7)
                .expect("details agent syntax")
                .expect("details agent request");
        assert_eq!(
            crate::types::request_context_binding(&request),
            AgentContextBinding::SelectedCommand
        );
        crate::agent::skill_context::finalize_agent_request_skill_context(&mut request, None);
        let input = request.user_input.expect("user input");
        assert!(input.contains("ShellEvidenceExcerpt"), "{input}");
        let hints = request.context_hints.join("\n");
        assert!(!hints.contains("__cosh_context_binding"), "{hints}");
        assert!(!hints.contains("failed_command_context"), "{hints}");
        assert!(!hints.contains("diagnostic_context"), "{hints}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn details_agent_request_rejects_missing_output() {
        let block = command_block("session-1", "cmd-1", None);
        let err = agent_request_from_details_input(&[block], "/details cmd-1 --agent", 7)
            .expect("details agent syntax")
            .expect_err("missing output should not start agent");

        assert!(err.contains("no captured output"), "{err}");
    }

    #[test]
    fn details_agent_request_rejects_invalid_syntax() {
        for input in [
            "/details cmd-1 --agent --tail",
            "/details cmd-1 --agent --tail 0",
            "/details cmd-1 --agent --middle 2",
            "/details cmd-1 --agent --tail 2 extra",
        ] {
            assert!(
                agent_request_from_details_input(&[], input, 1).is_none(),
                "{input}"
            );
        }
    }

    fn command_block(session_id: &str, id: &str, output_ref: Option<&str>) -> CommandBlock {
        CommandBlock {
            id: id.to_string(),
            session_id: session_id.to_string(),
            command: "df -h".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: output_ref.map(ToString::to_string),
                terminal_output_bytes: 123,
            },
            shell_environment_generation: None,
            audit_identity: None,
        }
    }

    fn test_output_ref(name: &str, text: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-{name}-{}-{unique}.txt",
            std::process::id()
        ));
        std::fs::write(&path, text).expect("write test output ref");
        path
    }
}
