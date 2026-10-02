use crate::agent::failed_command::latest_pending_failed_block_before_event;
use crate::runtime::evidence_requests::clear_pending_evidence_requests;
use crate::runtime::prelude::*;
use crate::runtime::state::{ContinuityFactKind, InlineState};
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelOwnership {
    ForegroundCommand,
    ActiveApprovalCard,
    ActiveQuestionCard,
    ActiveEvidenceRequestCard,
    ActiveAgentTurn,
    PromptOnly,
    NotCancel,
}

pub(crate) fn cancel_ownership_for_event(
    event: &ShellEvent,
    state: &InlineState,
    foreground_command_active: bool,
) -> CancelOwnership {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return CancelOwnership::NotCancel;
    }
    if foreground_command_active && is_control_cancel_event(event) {
        return CancelOwnership::ForegroundCommand;
    }
    if is_control_esc_event(event) {
        if state.agent_run.active.is_some() {
            return CancelOwnership::ActiveAgentTurn;
        }
        return CancelOwnership::NotCancel;
    }
    if is_approval_card_cancel_event(event) {
        return CancelOwnership::ActiveApprovalCard;
    }
    if is_question_card_cancel_event(event) {
        return CancelOwnership::ActiveQuestionCard;
    }
    if is_evidence_card_cancel_event(event) {
        return CancelOwnership::ActiveEvidenceRequestCard;
    }
    if event_requests_agent_cancel(event) {
        if state.agent_run.active.is_some() {
            return CancelOwnership::ActiveAgentTurn;
        }
        return CancelOwnership::PromptOnly;
    }
    CancelOwnership::NotCancel
}

pub(crate) fn render_agent_cancel_actions<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let ownership = cancel_ownership_for_event(event, state, false);
        if !matches!(
            ownership,
            CancelOwnership::ActiveAgentTurn | CancelOwnership::PromptOnly
        ) {
            continue;
        }

        let key = format!("agent-cancel-{event_index}");
        if !state.handled_cancel_requests.insert(key) {
            continue;
        }

        if ownership == CancelOwnership::ActiveAgentTurn {
            mark_pending_failed_block_cancelled(blocks, state, event, event_index);
            cancel_active_agent_run(state, output)?;
            continue;
        }

        if ownership == CancelOwnership::PromptOnly && is_control_cancel_event(event) {
            continue;
        }

        let i18n = state.i18n();
        let body = if let Some(block) =
            mark_pending_failed_block_cancelled(blocks, state, event, event_index)
        {
            vec![i18n.format(
                MessageId::FailedAnalysisCancelledBody,
                &[("command", &block.command)],
            )]
        } else {
            vec![i18n
                .t(MessageId::FailedAnalysisCancelNoActiveBody)
                .to_string()]
        };
        RatatuiInlineRenderer::for_terminal().write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::FailedAnalysisCancelledTitle),
                body,
                footer: Some(i18n.t(MessageId::FailedAnalysisCancelledFooter)),
            },
        )?;
        output.flush()?;
    }

    Ok(())
}

fn cancel_active_agent_run<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(mut active_run) = state.agent_run.active.take() else {
        return Ok(());
    };
    // Turn-scope batch consent never outlives its run (issue #1773).
    state.control.trust.clear_run_batch_consent();

    let cancellation_details_id = state.provider_cancellation_artifacts.record_cancelled_run(
        active_run.request.id.clone(),
        None,
        active_run.provider_name,
        active_run.handle.pending_provider_session_id(),
        active_run.handle.cancellation_artifact_store(),
    );
    // #1940 run-terminal sweep before the handle is cancelled (G1 D2):
    // the interrupt already unblocks cosh-core, but the sweep keeps the
    // terminal-state invariant uniform without relying on peer behavior.
    crate::approval::runtime::drain_unhomed_control_requests_with_handle(
        state,
        &active_run.request.id,
        &active_run.handle,
    );
    active_run.handle.cancel();
    active_run.status_animation.clear(output)?;
    active_run.held_events.clear();
    active_run.deferred_events.clear();
    clear_active_run_request_buffers(&mut active_run);
    active_run.markdown_stream.finish(output, None)?;
    // Surface unconsumed hook notifications before teardown; dropping them
    // with the run is how block decisions went missing (#2067).
    crate::agent::run::drain_pending_hook_notifications(&mut active_run, output)?;
    state.agent_run.held_events.clear();
    suppress_pending_work_after_agent_cancel(state);
    state.agent_run.needs_prompt_after_run = true;

    let i18n = state.i18n();
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title: i18n.t(MessageId::AgentCancellationRequestedTitle),
            body: vec![
                i18n.t(MessageId::AgentCancellationRequestedBody)
                    .to_string(),
                format!("Details: {cancellation_details_id}"),
            ],
            footer: Some(i18n.t(MessageId::FailedAnalysisCancelledFooter)),
        },
    )?;

    let event = AgentEvent::AgentCancelled {
        run_id: active_run.request.id.clone(),
        reason: "user requested cancellation".to_string(),
    };
    let governed =
        govern_agent_events_with_language(&[event], &Policy::default(), active_run.language).events;
    active_run
        .renderer
        .write_governed_events(output, &governed)?;
    active_run.governed_events.extend(governed);
    active_run
        .handle
        .drain_cancelled_in_background(std::time::Duration::from_secs(5));
    state.continuity.facts.push(
        ContinuityFactKind::AgentResult,
        "cancelled: user requested cancellation",
    );
    output.flush()
}

fn clear_active_run_request_buffers(active_run: &mut crate::agent::run::ActiveAgentRun) {
    active_run.cosh_request_filter.clear();
    active_run.pending_cosh_requests.clear();
    active_run.pending_cosh_request_audits.clear();
}

fn suppress_pending_work_after_agent_cancel(state: &mut InlineState) {
    // Explicit user cancellation authorizes discarding the ENTIRE pending
    // queue — including control responses. This is a deliberate, user-driven
    // reset of the conversation, not an accidental loss: the user asked to
    // stop everything the Agent had in flight.
    state.agent_run.queued_requests.clear();
    state.hooks.pending_consultation = None;
    state.hooks.pending_consultation_queue.clear();
    clear_pending_evidence_requests(state);
}

fn mark_pending_failed_block_cancelled(
    blocks: &[CommandBlock],
    state: &mut InlineState,
    event: &ShellEvent,
    event_index: usize,
) -> Option<CommandBlock> {
    let block = latest_pending_failed_block_before_event(blocks, state, event)?;
    let block = block.clone();
    state.canceled_blocks.insert(block.id.clone());
    state
        .handled_cancellations
        .insert(format!("cancel-{event_index}"));
    Some(block)
}

fn is_control_cancel_event(event: &ShellEvent) -> bool {
    event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some("control")
        && event.input.as_deref() == Some("ctrl_c")
}

fn is_control_esc_event(event: &ShellEvent) -> bool {
    event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some("control")
        && event.input.as_deref() == Some("esc")
}

fn is_approval_card_cancel_event(event: &ShellEvent) -> bool {
    event.component.as_deref() == Some("card")
        && event
            .input
            .as_deref()
            .is_some_and(|id| id.starts_with("req-"))
        && event.message.as_deref() == Some("cancel")
}

/// Both ways of abandoning a question prompt release the run's cancel ownership: ESC
/// (`question_cancel`) and Ctrl+C (`question_abort`).
fn is_question_card_cancel_event(event: &ShellEvent) -> bool {
    event.component.as_deref() == Some("card")
        && matches!(
            event.message.as_deref(),
            Some("question_cancel") | Some("question_abort")
        )
}

fn is_evidence_card_cancel_event(event: &ShellEvent) -> bool {
    event.component.as_deref() == Some("card")
        && event.message.as_deref() == Some("evidence_cancel")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::run::ActiveAgentRun;
    use crate::agent::run::PendingAgentRequest;
    use crate::evidence::request::{CoshRequest, ParsedCoshRequest};
    use crate::hooks::state::{PendingConsultation, PendingConsultationState};
    use std::time::Instant;

    #[test]
    fn agent_cancel_suppresses_queued_agent_and_hook_work() {
        let mut state = InlineState::default();
        state
            .agent_run
            .queued_requests
            .push_back(PendingAgentRequest {
                request: agent_request("queued"),
                origin: AgentRunOrigin::Standard,
                intent: crate::agent::run::AgentStartIntent::UserInitiated,
                class: crate::agent::run::PendingRequestClass::Normal,
                selectable_after_event_index: None,
                before_held_text: false,
            });
        state.hooks.pending_consultation = Some(pending_consultation("active-card"));
        state
            .hooks
            .pending_consultation_queue
            .push_back(pending_consultation("queued-card"));

        suppress_pending_work_after_agent_cancel(&mut state);

        assert!(state.agent_run.queued_requests.is_empty());
        assert!(state.hooks.pending_consultation.is_none());
        assert!(state.hooks.pending_consultation_queue.is_empty());
    }

    #[test]
    fn agent_cancel_clears_request_block_buffers() {
        let mut active_run = test_active_run();
        assert_eq!(
            active_run
                .cosh_request_filter
                .filter_delta("```cosh-request\nhistory")
                .visible_text,
            ""
        );
        active_run.pending_cosh_requests.push(ParsedCoshRequest {
            request: CoshRequest::History,
            ignored_multiple_request_blocks: false,
        });

        clear_active_run_request_buffers(&mut active_run);

        assert!(active_run.cosh_request_filter.is_empty());
        assert!(active_run.pending_cosh_requests.is_empty());
        assert!(active_run
            .cosh_request_filter
            .finish()
            .visible_text
            .is_empty());
    }

    #[test]
    fn card_agent_cancel_stops_active_agent_run() {
        let mut state = InlineState::default();
        state.agent_run.active = Some(test_active_run());
        let mut event = ShellEvent::user_input_intercepted("session-1", "active");
        event.component = Some("card".to_string());
        event.message = Some("agent_cancel".to_string());
        let mut output = Vec::new();

        render_agent_cancel_actions(&[event], &[], &mut state, &mut output, 0)
            .expect("render card agent cancel");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(state.agent_run.active.is_none());
        assert!(
            rendered.contains("Agent cancellation requested"),
            "{rendered}"
        );
        assert!(rendered.contains("Details: provider-cancel-1"));
    }

    #[test]
    fn control_esc_cancels_active_agent_run() {
        let mut state = InlineState::default();
        state.agent_run.active = Some(test_active_run());
        let mut output = Vec::new();

        render_agent_cancel_actions(&[control_esc()], &[], &mut state, &mut output, 0)
            .expect("render ESC agent cancel");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(state.agent_run.active.is_none());
        assert!(
            rendered.contains("Agent cancellation requested"),
            "{rendered}"
        );
        assert!(rendered.contains("Details: provider-cancel-1"));
    }

    #[test]
    fn control_esc_without_active_run_is_ignored() {
        let mut state = InlineState::default();
        let mut output = Vec::new();

        render_agent_cancel_actions(&[control_esc()], &[], &mut state, &mut output, 0)
            .expect("render ESC with no active run");

        assert!(state.agent_run.active.is_none());
        assert!(output.is_empty());
    }

    #[test]
    fn cancel_ownership_classifies_foreground_cards_agent_and_prompt() {
        let mut state = InlineState::default();
        assert_eq!(
            cancel_ownership_for_event(&control_ctrl_c(), &state, true),
            CancelOwnership::ForegroundCommand
        );
        assert_eq!(
            cancel_ownership_for_event(&card_event("req-1", "cancel"), &state, false),
            CancelOwnership::ActiveApprovalCard
        );
        assert_eq!(
            cancel_ownership_for_event(&card_event("q-1", "question_cancel"), &state, false),
            CancelOwnership::ActiveQuestionCard
        );
        assert_eq!(
            cancel_ownership_for_event(&card_event("evidence-1", "evidence_cancel"), &state, false),
            CancelOwnership::ActiveEvidenceRequestCard
        );
        assert_eq!(
            cancel_ownership_for_event(&slash_cancel(), &state, false),
            CancelOwnership::PromptOnly
        );

        state.agent_run.active = Some(test_active_run());
        assert_eq!(
            cancel_ownership_for_event(&slash_cancel(), &state, false),
            CancelOwnership::ActiveAgentTurn
        );
        assert_eq!(
            cancel_ownership_for_event(&control_ctrl_c(), &state, false),
            CancelOwnership::ActiveAgentTurn
        );
        assert_eq!(
            cancel_ownership_for_event(&card_event("req-1", "deny"), &state, false),
            CancelOwnership::NotCancel
        );
    }

    fn slash_cancel() -> ShellEvent {
        ShellEvent::user_input_intercepted("session-1", "/cancel")
    }

    fn control_ctrl_c() -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("session-1", "ctrl_c");
        event.component = Some("control".to_string());
        event
    }

    fn control_esc() -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("session-1", "esc");
        event.component = Some("control".to_string());
        event
    }

    fn card_event(id: &str, message: &str) -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("session-1", id);
        event.component = Some("card".to_string());
        event.message = Some(message.to_string());
        event
    }

    fn pending_consultation(card_id: &str) -> PendingConsultation {
        PendingConsultation {
            finding_id: format!("finding-{card_id}"),
            card_id: card_id.to_string(),
            block_id: "cmd-1".to_string(),
            command: "echo test".to_string(),
            output_ref: None,
            state: PendingConsultationState::Queued,
            created_at_ms: 1,
            expires_at_ms: 2,
            ended_at_ms: 1,
            queued_at: std::time::Instant::now(),
            prompt_hint: String::new(),
            hook_finding: None,
            recommended_skill: None,
            context_hints: Vec::new(),
            suppression_key: format!("suppression-{card_id}"),
            topic: "test".to_string(),
            entity_key: "test".to_string(),
            confidence: "low".to_string(),
            display_reason: "test".to_string(),
        }
    }

    fn agent_request(id: &str) -> AgentRequest {
        AgentRequest {
            id: id.to_string(),
            session_id: "session-1".to_string(),
            command_block: CommandBlock {
                id: "cmd-1".to_string(),
                session_id: "session-1".to_string(),
                command: "echo test".to_string(),
                origin: Default::default(),
                cwd: "/tmp".to_string(),
                end_cwd: "/tmp".to_string(),
                started_at_ms: 0,
                ended_at_ms: 1,
                duration_ms: 1,
                exit_code: 0,
                status: CommandStatus::Completed,
                output: OutputRefs {
                    terminal_output_ref: None,
                    terminal_output_bytes: 0,
                },
                shell_environment_generation: None,
                audit_identity: None,
            },
            context_blocks: Vec::new(),
            context_hints: Vec::new(),
            user_input: Some("test".to_string()),
            findings: Vec::new(),
            mode: AgentMode::RecommendOnly,
            user_confirmed: true,
            hook_finding: None,
            recommended_skill: None,
        }
    }

    fn test_active_run() -> ActiveAgentRun {
        let request = agent_request("active");
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let handle = adapter.start_cancellable(request.clone(), CoshApprovalMode::Recommend);
        let renderer = RatatuiInlineRenderer::for_terminal();
        ActiveAgentRun {
            request,
            origin: AgentRunOrigin::Standard,
            handle,
            provider_name: "fake",
            language: Language::EnUs,
            renderer: renderer.clone(),
            status_animation: renderer.status_animation(),
            markdown_stream: renderer.stream_markdown_agent(),
            governed_events: Vec::new(),
            deferred_events: Vec::new(),
            held_events: Vec::new(),
            cosh_request_filter: crate::evidence::stream::CoshRequestStreamFilter::default(),
            pending_cosh_requests: Vec::new(),
            pending_cosh_request_audits: Vec::new(),
            rendered_governed_event_count: 0,
            selectable_after_event_index: None,
            started_at: Instant::now(),
            last_activity_at: Instant::now(),
            last_heartbeat_at: Instant::now(),
            current_phase: String::new(),
            current_message: String::new(),
            has_visible_text_delta: false,
            completed: false,
            host_completed_tool_ids: Vec::new(),
            pending_hook_notifications: Vec::new(),
        }
    }

    // F2-ext (#2067): cancelling a run with a pending hook block
    // notification must surface the decision through governance instead of
    // dropping it with the run.
    #[test]
    fn cancel_surfaces_pending_hook_block_notification() {
        let mut state = InlineState::default();
        let mut active_run = test_active_run();
        active_run
            .pending_hook_notifications
            .push(crate::agent::run::PendingHookNotification {
                tool_use_id: Some("toolu-1".to_string()),
                hook_name: "guard".to_string(),
                message: "no touch".to_string(),
                decision: Some("block".to_string()),
            });
        state.agent_run.active = Some(active_run);
        let mut output = Vec::new();

        cancel_active_agent_run(&mut state, &mut output).expect("cancel run");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(
            rendered.contains("Hook: guard")
                && rendered.contains("Message: no touch")
                && rendered.contains("Decision: block"),
            "cancel must surface the pending block decision: {rendered}"
        );
    }
}
