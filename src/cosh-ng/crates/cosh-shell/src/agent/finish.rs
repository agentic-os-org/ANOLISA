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

    let failed_without_recovery = failed_run_has_no_recovery(&active_run);
    if failed_without_recovery {
        let remaining = &active_run.governed_events[active_run.rendered_governed_event_count..];
        crate::approval::requests::record_approval_requests(
            state,
            remaining,
            Some(&active_run.request),
            active_run.origin,
            adapter.capabilities().control_protocol,
        );
        crate::approval::runtime::cancel_pending_approvals_for_failed_run(
            state,
            &active_run.request.id,
            &active_run.handle,
            output,
        )?;
        crate::agent::structured_events::render_terminal_activity(
            state,
            remaining,
            active_run.origin,
            output,
            adapter,
        )?;
        active_run.rendered_governed_event_count = active_run.governed_events.len();
    }
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
    if failed_without_recovery {
        crate::approval::panel::render_current_approval_request(state, output)?;
    }
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

pub(crate) fn failed_run_has_no_recovery(active_run: &ActiveAgentRun) -> bool {
    active_run.governed_events.iter().any(|event| {
        matches!(
            &event.event, AgentEvent::AgentFailed { run_id, .. } if run_id == &active_run.request.id
        )
    }) && shell_handoff_resume_fallback_request(active_run).is_none()
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
#[path = "finish_tests.rs"]
mod tests;
