use crate::agent::continuation::{
    render_fallback_recovery_notice, shell_handoff_recovery_approval_id,
    shell_handoff_resume_fallback_request,
};
use crate::agent::events::{
    flush_cosh_request_filter_into_active_run, render_agent_structured_events,
    render_held_events_into_active_run, state_has_pending_interaction,
};
use crate::agent::governance::{
    hook_notification_display_text, project_hook_notifications_for_display,
};
use crate::agent::run::{
    has_queued_run_before_held_text, start_agent_run_with_origin, start_pending_agent_run,
    ActiveAgentRun, PendingHookNotification, PendingRequestClass,
};
use crate::recommendation::personal_integration::record_finished_agent_run;
use crate::runtime::evidence_requests::{
    record_cosh_requests_from_active_run, render_pending_evidence_requests,
};
use crate::runtime::prelude::*;
use crate::runtime::question_terminal::cleanup_question_for_terminal_owner;
use crate::types::PROVIDER_TIMEOUT_ERROR_CODE;

pub(crate) fn finish_active_agent_run<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    adapter: &AdapterInstance,
) -> std::io::Result<()> {
    let Some(mut active_run) = state.agent_run.active.take() else {
        return Ok(());
    };
    // Turn-scope batch consent never outlives its run (issue #1773).
    state.control.trust.clear_run_batch_consent();

    cleanup_question_for_terminal_owner(state, output, &active_run.request.id)?;
    active_run.status_animation.clear(output)?;
    if !active_run.held_events.is_empty() {
        if state
            .control
            .shell_handoff()
            .has_active_handoff_for_run(&active_run.request.id)
        {
            // This run ended while its own foreground shell handoff was still
            // outstanding, so its text cannot be based on the real command
            // result. Drop it instead of rendering or transferring it; the
            // shell-evidence continuation regenerates the answer once the result
            // lands. Scoped to this run: another run's in-flight handoff says
            // nothing about whether this text is stale.
            active_run.held_events.clear();
        } else if state_has_pending_interaction(state) || has_queued_run_before_held_text(state) {
            state
                .agent_run
                .held_events
                .append(&mut active_run.held_events);
        } else {
            let held_events = std::mem::take(&mut active_run.held_events);
            render_held_events_into_active_run(&mut active_run, &held_events, output)?;
        }
    }
    flush_cosh_request_filter_into_active_run(&mut active_run, output)?;
    active_run.markdown_stream.finish(output, None)?;
    let provider_timed_out = active_run_provider_timed_out(&active_run);
    record_finished_agent_run(state, &active_run.request, &active_run.governed_events);
    let resume_fallback = shell_handoff_resume_fallback_request(&active_run);
    if let Some((fallback, origin, reason)) = resume_fallback {
        if let Some(approval_id) = shell_handoff_recovery_approval_id(&active_run.request) {
            state.evidence.mark_recovery_reason(approval_id, reason);
        }
        render_recovery_context_before_notice(state, &active_run, output, adapter)?;
        render_fallback_recovery_notice(state, &fallback, reason, output)?;
        // #1940: the fallback abandons this run; sweep dropped control
        // requests before starting its continuation so the ledger
        // cannot grow across turns.
        crate::approval::runtime::drain_unhomed_control_requests_with_handle(
            state,
            &active_run.request.id,
            &active_run.handle,
        );
        // Failed provider resume escalates through the tier chain: a
        // same-session retry first, then one fresh fallback.
        start_agent_run_with_origin(
            &fallback,
            origin,
            AgentStartIntent::InternalBestEffort,
            adapter,
            state,
            output,
            active_run.selectable_after_event_index,
        )?;
        return Ok(());
    }
    // Drain any unconsumed pending hook notifications into deferred_events
    // (orphan case: hook returned block, so no ToolPermissionRequest was
    // emitted). Route them through governance so the rendered block carries
    // real display text instead of an empty card (#2067).
    let i18n = I18n::new(active_run.language);
    drain_orphan_hook_notifications(&mut active_run);
    if !active_run.deferred_events.is_empty() {
        // Projection is throwaway and display-only: `deferred_events` itself
        // still holds every original notification for approval linking and
        // audit.
        let displayed = project_hook_notifications_for_display(&active_run.deferred_events, &i18n);
        active_run
            .renderer
            .write_governed_events(output, &displayed)?;
    }
    let evidence_requests = record_cosh_requests_from_active_run(state, &mut active_run);
    for notice in &evidence_requests.notices {
        active_run.renderer.write_notice_panel(
            output,
            NoticePanelModel {
                title: "Evidence Request",
                body: vec![notice.clone()],
                footer: None,
            },
        )?;
    }
    render_pending_evidence_requests(state, &evidence_requests.card_ids, output)?;

    let remaining_structured_events =
        active_run.governed_events[active_run.rendered_governed_event_count..].to_vec();
    render_agent_structured_events(
        state,
        &remaining_structured_events,
        Some(&active_run.request),
        active_run.origin,
        output,
        adapter,
    )?;
    // #1940 run-terminal sweep: the run is detached from InlineState here,
    // so the batch drain in render_new_agent_structured_events no longer
    // covers it; deny every registered control request that still has no
    // home (e.g. trailing requests parked in deferred_events by the
    // question-rejection path) and clear the run's ledger entries.
    crate::approval::runtime::drain_unhomed_control_requests_with_handle(
        state,
        &active_run.request.id,
        &active_run.handle,
    );
    record_selectable_recommendations(
        state,
        &active_run.governed_events,
        active_run.origin,
        active_run.selectable_after_event_index,
    );
    render_selectable_recommendations(
        &active_run.governed_events,
        active_run.origin,
        active_run.language,
        output,
    )?;
    record_agent_run_facts(state, &active_run);
    crate::agent::turn_extension::note_capped_run(
        state,
        &active_run,
        adapter.committed_session_id(),
    );
    state.auth.state = None;
    if provider_timed_out {
        let dropped = trim_queued_requests_after_provider_timeout(state);
        if dropped > 0 {
            active_run.renderer.write_notice_panel(
                output,
                NoticePanelModel {
                    title: state.i18n().t(MessageId::AgentStatusTitle),
                    body: vec![state.i18n().format(
                        MessageId::AgentProviderTimeoutDroppedQueuedBody,
                        &[("dropped", &dropped.to_string())],
                    )],
                    footer: None,
                },
            )?;
        }
    }
    output.flush()?;

    // A recommended automatic compaction has top priority at the idle
    // boundary: do not start any internal continuation or dequeue a run, since
    // that would keep `agent_run.active` set and postpone the compaction
    // indefinitely. Drop stale internal continuations (their captured context
    // is about to be rewritten by the compactor) and hold explicit user
    // requests in the queue so they resume in FIFO order after compaction.
    if state.control.session().compaction().has_pending_auto() {
        state
            .agent_run
            .queued_requests
            .retain(|pending| pending.intent == AgentStartIntent::UserInitiated);
        return Ok(());
    }

    if crate::agent::turn_extension::activate_pending_turn_extension(state, output)? {
        output.flush()?;
        return Ok(());
    }

    for (request, origin) in evidence_requests.auto_requests {
        // Evidence auto-follow-ups are internal best-effort continuations; the
        // gate drops them while a compaction is pending or active.
        start_agent_run_with_origin(
            &request,
            origin,
            AgentStartIntent::InternalBestEffort,
            adapter,
            state,
            output,
            None,
        )?;
    }

    if let Some(pending) = state.agent_run.queued_requests.pop_front() {
        // Restart with the stored admission class so a control response that
        // gets re-queued (e.g. behind a fresh compaction) keeps its class.
        start_pending_agent_run(pending, adapter, state, output)?;
    }

    Ok(())
}

fn drain_orphan_hook_notifications(active_run: &mut ActiveAgentRun) {
    let events = crate::agent::run::take_pending_hook_notification_events(active_run);
    if events.is_empty() {
        return;
    }
    let mut governed =
        govern_agent_events_with_language(&events, &Policy::default(), active_run.language).events;
    active_run.deferred_events.append(&mut governed);
}

fn active_run_provider_timed_out(active_run: &ActiveAgentRun) -> bool {
    active_run
        .governed_events
        .iter()
        .any(governed_event_is_provider_timeout)
}

fn render_recovery_deferred_context<W: Write>(
    active_run: &ActiveAgentRun,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = I18n::new(active_run.language);
    let mut events = active_run
        .deferred_events
        .iter()
        .filter(|event| !governed_event_is_provider_timeout(event))
        .cloned()
        .collect::<Vec<_>>();
    events.extend(
        active_run
            .pending_hook_notifications
            .iter()
            .map(|notification| {
                pending_hook_notification_event(&active_run.request.id, notification, &i18n)
            }),
    );
    if events.is_empty() {
        return Ok(());
    }
    let events = project_hook_notifications_for_display(&events, &i18n);
    active_run.renderer.write_governed_events(output, &events)
}

fn pending_hook_notification_event(
    run_id: &str,
    notification: &PendingHookNotification,
    i18n: &I18n,
) -> GovernedEvent {
    GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::HookNotification {
            run_id: run_id.to_string(),
            hook_name: notification.hook_name.clone(),
            message: notification.message.clone(),
            tool_use_id: notification.tool_use_id.clone(),
            decision: notification.decision.clone(),
        },
        reason: "orphan hook notification".to_string(),
        display_text: hook_notification_display_text(
            &notification.hook_name,
            &notification.message,
            notification.decision.as_deref(),
            i18n,
        ),
        auto_execute: false,
    }
}

fn render_recovery_context_before_notice<W: Write>(
    state: &mut InlineState,
    active_run: &ActiveAgentRun,
    output: &mut W,
    adapter: &AdapterInstance,
) -> std::io::Result<()> {
    render_recovery_deferred_context(active_run, output)?;
    let remaining_structured_events = active_run.governed_events
        [active_run.rendered_governed_event_count..]
        .iter()
        .filter(|event| !governed_event_is_provider_timeout(event))
        .cloned()
        .collect::<Vec<_>>();
    render_agent_structured_events(
        state,
        &remaining_structured_events,
        Some(&active_run.request),
        active_run.origin,
        output,
        adapter,
    )
}

fn governed_event_is_provider_timeout(event: &GovernedEvent) -> bool {
    matches!(
        &event.event,
        AgentEvent::AgentFailed { error_code, .. }
            if error_code.as_deref() == Some(PROVIDER_TIMEOUT_ERROR_CODE)
    )
}

/// Sheds queue backlog after a provider timeout without ever dropping a
/// control-protocol response.
///
/// Retained (in original FIFO order, so replay order stays deterministic):
/// - every [`PendingRequestClass::ControlResponse`] entry — its question or
///   approval state was already consumed and the user cannot re-issue it;
/// - the oldest normal request, so the user's next intent survives.
///
/// Every other normal request is dropped; the returned count covers exactly
/// those, which is what the user-visible notice reports.
fn trim_queued_requests_after_provider_timeout(state: &mut InlineState) -> usize {
    let before = state.agent_run.queued_requests.len();
    let mut kept_normal = false;
    state
        .agent_run
        .queued_requests
        .retain(|pending| match pending.class {
            PendingRequestClass::ControlResponse => true,
            PendingRequestClass::Normal => {
                if kept_normal {
                    false
                } else {
                    kept_normal = true;
                    true
                }
            }
        });
    before - state.agent_run.queued_requests.len()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::agent::events::render_active_agent_event;
    use crate::agent::run::{PendingAgentRequest, PendingRequestClass};
    use crate::runtime::state::InlineState;
    use crate::types::{AgentMode, AgentRequest, CommandBlock, CommandStatus, OutputRefs};

    fn request(id: &str) -> AgentRequest {
        AgentRequest {
            id: id.to_string(),
            session_id: "shell-session".to_string(),
            command_block: CommandBlock {
                id: format!("cmd-{id}"),
                session_id: "shell-session".to_string(),
                command: "echo hi".to_string(),
                origin: Default::default(),
                cwd: "/repo".to_string(),
                end_cwd: "/repo".to_string(),
                started_at_ms: 1,
                ended_at_ms: 2,
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
            user_input: Some("queued".to_string()),
            findings: Vec::new(),
            mode: AgentMode::RecommendOnly,
            user_confirmed: true,
            hook_finding: None,
            recommended_skill: None,
        }
    }

    fn pending(id: &str, class: PendingRequestClass) -> PendingAgentRequest {
        PendingAgentRequest {
            request: request(id),
            origin: AgentRunOrigin::Standard,
            intent: AgentStartIntent::UserInitiated,
            class,
            selectable_after_event_index: None,
            before_held_text: false,
        }
    }

    // A provider that ends its turn while its own Bash handoff is still running
    // produced that text without the command's result, so finishing the run must
    // drop it instead of rendering it or moving it to the shell-wide hold. The
    // shell-evidence continuation regenerates the answer.
    #[test]
    fn finish_drops_held_text_while_its_own_shell_handoff_is_still_outstanding() {
        let (rendered, state) = finish_run_with_handoff_owned_by("run-1");

        assert!(!rendered.contains("STALE ANSWER"), "{rendered}");
        assert!(state.agent_run.held_events.is_empty());
    }

    // The drop is scoped to the run that owns the handoff: text from a run whose
    // own work is complete must still be shown even while some other run's
    // handoff is in flight.
    #[test]
    fn finish_keeps_held_text_when_another_run_owns_the_outstanding_handoff() {
        let (rendered, _state) = finish_run_with_handoff_owned_by("unrelated-run");

        assert!(rendered.contains("STALE ANSWER"), "{rendered}");
    }

    #[test]
    fn finish_renders_orphan_hook_notification_with_fallbacks() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        let mut active_run = active_run(&adapter, "run-1", Language::ZhCn);
        active_run
            .pending_hook_notifications
            .push(PendingHookNotification {
                tool_use_id: Some("tool-1".to_string()),
                hook_name: "  ".to_string(),
                message: String::new(),
                decision: Some("\n".to_string()),
            });
        state.agent_run.active = Some(active_run);
        let mut output = Vec::new();

        finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

        let rendered = String::from_utf8_lossy(&output);
        assert!(rendered.contains("Hook: 未知 Hook"), "{rendered}");
        assert!(rendered.contains("消息: 未提供消息"), "{rendered}");
        assert!(rendered.contains("决策: 未指定"), "{rendered}");
        assert!(!rendered.contains("unknown hook"), "{rendered}");
    }

    // #2197: a hook that fires on every tool call used to dump one three-line
    // block per hit into the same Governance panel. The finish path must render
    // the collapsed projection while `deferred_events` keeps every original.
    #[test]
    fn finish_collapses_repeated_allow_hook_notifications_into_one_line() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
        for _ in 0..3 {
            active_run
                .pending_hook_notifications
                .push(PendingHookNotification {
                    tool_use_id: Some("tool-1".to_string()),
                    hook_name: "pii-checker".to_string(),
                    message: "[pii-checker] card hit".to_string(),
                    decision: Some("allow".to_string()),
                });
        }
        state.agent_run.active = Some(active_run);
        let mut output = Vec::new();

        finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

        let rendered = String::from_utf8_lossy(&output);
        assert_eq!(rendered.matches("pii-checker").count(), 1, "{rendered}");
        assert!(
            rendered.contains("• pii-checker: card hit ×3"),
            "{rendered}"
        );
        assert!(!rendered.contains("Decision: allow"), "{rendered}");
    }

    #[test]
    fn recovery_context_collapses_allow_notices_and_omits_provider_timeout() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
        let mut ingress_output = Vec::new();
        for _ in 0..3 {
            render_active_agent_event(
                &mut active_run,
                AgentEvent::HookNotification {
                    run_id: "run-1".to_string(),
                    hook_name: "pii-checker".to_string(),
                    message: "[pii-checker] card hit".to_string(),
                    tool_use_id: Some("tool-1".to_string()),
                    decision: Some("allow".to_string()),
                },
                &mut ingress_output,
                None,
            )
            .expect("ingest hook notification");
        }
        render_active_agent_event(
            &mut active_run,
            AgentEvent::AgentFailed {
                run_id: "run-1".to_string(),
                error: "PROVIDER TIMEOUT MUST STAY HIDDEN".to_string(),
                error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                max_turns: None,
            },
            &mut ingress_output,
            None,
        )
        .expect("ingest provider timeout");
        assert_eq!(active_run.pending_hook_notifications.len(), 3);
        let mut output = Vec::new();

        render_recovery_deferred_context(&active_run, &mut output)
            .expect("render recovery context");

        let rendered = String::from_utf8_lossy(&output);
        assert_eq!(rendered.matches("pii-checker").count(), 1, "{rendered}");
        assert!(
            rendered.contains("• pii-checker: card hit ×3"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("PROVIDER TIMEOUT MUST STAY HIDDEN"),
            "{rendered}"
        );
    }

    // Finishes an active `run-1` holding one text delta, with an approved handoff
    // owned by `handoff_run_id` still outstanding. Returns what was rendered plus
    // the resulting state.
    fn finish_run_with_handoff_owned_by(handoff_run_id: &str) -> (String, InlineState) {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
        active_run.held_events.push(GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::TextDelta {
                run_id: "run-1".to_string(),
                text: "STALE ANSWER".to_string(),
            },
            reason: "held".to_string(),
            display_text: "STALE ANSWER".to_string(),
            auto_execute: false,
        });
        state.agent_run.active = Some(active_run);
        state.control.shell_handoff_mut().enqueue_approved_request(
            ShellHandoffRequest::new(
                "df -h",
                "$ df -h",
                "provider-tool-call",
                "agent",
                "req-1",
                handoff_run_id,
                1,
            )
            .expect("handoff"),
        );
        let mut output = Vec::new();

        finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

        (String::from_utf8_lossy(&output).to_string(), state)
    }

    fn active_run(adapter: &AdapterInstance, id: &str, language: Language) -> ActiveAgentRun {
        let run_request = request(id);
        let handle = adapter.start_cancellable(run_request.clone(), CoshApprovalMode::Recommend);
        let renderer = RatatuiInlineRenderer::for_terminal().with_language(language);
        ActiveAgentRun {
            request: run_request,
            origin: AgentRunOrigin::Standard,
            handle,
            provider_name: "fake",
            language,
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
            completed: true,
            host_completed_tool_ids: Vec::new(),
            pending_hook_notifications: Vec::new(),
        }
    }

    #[test]
    fn provider_timeout_trim_keeps_control_responses_and_oldest_normal_in_order() {
        let mut state = InlineState::default();
        for (id, class) in [
            ("normal-a", PendingRequestClass::Normal),
            ("control-b", PendingRequestClass::ControlResponse),
            ("normal-c", PendingRequestClass::Normal),
            ("control-d", PendingRequestClass::ControlResponse),
            ("normal-e", PendingRequestClass::Normal),
        ] {
            state
                .agent_run
                .queued_requests
                .push_back(pending(id, class));
        }

        let dropped = trim_queued_requests_after_provider_timeout(&mut state);

        // Only the surplus normal requests were dropped and counted; every
        // control response survives, and FIFO order is untouched.
        assert_eq!(dropped, 2);
        let ids: Vec<&str> = state
            .agent_run
            .queued_requests
            .iter()
            .map(|pending| pending.request.id.as_str())
            .collect();
        assert_eq!(ids, ["normal-a", "control-b", "control-d"]);
    }

    #[test]
    fn provider_timeout_trim_drops_nothing_without_surplus_normals() {
        let mut state = InlineState::default();
        state
            .agent_run
            .queued_requests
            .push_back(pending("control-a", PendingRequestClass::ControlResponse));
        state
            .agent_run
            .queued_requests
            .push_back(pending("normal-b", PendingRequestClass::Normal));

        assert_eq!(trim_queued_requests_after_provider_timeout(&mut state), 0);
        assert_eq!(state.agent_run.queued_requests.len(), 2);
    }

    // F2 (#2067): an orphan hook block notification must drain through
    // governance so the deferred render carries real display text instead of
    // an empty card.
    #[test]
    fn finish_drains_orphan_hook_notification_with_visible_display_text() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
        active_run
            .pending_hook_notifications
            .push(crate::agent::run::PendingHookNotification {
                tool_use_id: Some("toolu-1".to_string()),
                hook_name: "guard".to_string(),
                message: String::new(),
                decision: Some("block".to_string()),
            });
        state.agent_run.active = Some(active_run);
        let mut output = Vec::new();

        finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(
            rendered.contains("Hook: guard")
                && rendered.contains("Message: no message provided")
                && rendered.contains("Decision: block"),
            "the orphan block must render visible governance text: {rendered}"
        );
    }
}
