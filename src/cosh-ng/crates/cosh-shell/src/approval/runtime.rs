use crate::agent::run::ActiveAgentRun;
use crate::approval::broker::ApprovalOutcome;
use crate::approval::cards::write_approval_receipt;
use crate::approval::handoff::{queue_approved_shell_handoff, queue_interactive_shell_handoff};
use crate::approval::panel::{
    approval_focus_from_event, approval_is_pending, clear_active_approval_panel,
    redraw_current_approval_request, render_current_approval_request,
};
use crate::approval::provider::{mark_provider_approval_resolved, provider_approval_response};
use crate::approval::resolution::{
    apply_approval_decision, apply_batch_consent_decision, approval_outcome_for_request,
    approval_resolution_agent_request, batch_consent_covers_request,
    request_can_receive_host_executed_result, should_send_approval_resolution_to_agent,
};
use crate::runtime::details::agent_request_from_details_input;
use crate::runtime::prelude::*;

pub(crate) fn render_approval_actions<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        if let Some((id, action)) = approval_focus_from_event(event, &state.approvals.requests) {
            let key = format!("approval-focus-{event_index}");
            if !state.approvals.handled_actions.insert(key) {
                continue;
            }
            if approval_is_pending(state, &id) {
                state.approvals.focus.insert(id, action);
                redraw_current_approval_request(state, output)?;
                output.flush()?;
            }
            continue;
        }

        let Some(command) = approval_command_from_event(event) else {
            continue;
        };

        let key = format!("approval-{event_index}");
        if !state.approvals.handled_actions.insert(key) {
            continue;
        }

        if command.kind == ApprovalCommandKind::Details {
            if event.component.as_deref() == Some("card") {
                state
                    .approvals
                    .focus
                    .insert(command.id.clone(), ApprovalPanelAction::Details);
                state.approvals.expanded_cards.insert(command.id.clone());
                redraw_current_approval_request(state, output)?;
            } else {
                if let Some(input) = event.input.as_deref() {
                    if let Some(result) =
                        agent_request_from_details_input(blocks, input, event_index)
                    {
                        match result {
                            Ok(request) => {
                                state.agent_run.needs_prompt_after_run = event.cwd.is_none();
                                start_agent_run(
                                    &request,
                                    AgentStartIntent::UserInitiated,
                                    adapter,
                                    state,
                                    output,
                                    Some(event_index),
                                )?;
                            }
                            Err(message) => {
                                let i18n = state.i18n();
                                RatatuiInlineRenderer::for_terminal().write_notice_panel(
                                    output,
                                    NoticePanelModel {
                                        title: i18n.t(MessageId::RuntimeDetailsUnavailableTitle),
                                        body: vec![message],
                                        footer: None,
                                    },
                                )?;
                            }
                        }
                        output.flush()?;
                        continue;
                    }
                }
                render_runtime_details(state, blocks, &command.id, output)?;
            }
            output.flush()?;
            continue;
        }

        if command.kind == ApprovalCommandKind::SendToShell {
            queue_interactive_shell_handoff(state, &command.id, output)?;
            output.flush()?;
            continue;
        }

        let Some(request_index) = state
            .approvals
            .requests
            .iter()
            .position(|request| request.id == command.id)
        else {
            let i18n = state.i18n();
            RatatuiInlineRenderer::for_terminal().write_notice_panel(
                output,
                NoticePanelModel {
                    title: i18n.t(MessageId::ApprovalNotFoundTitle),
                    body: vec![i18n.format(
                        MessageId::ApprovalNotFoundBody,
                        &[("id", command.id.as_str())],
                    )],
                    footer: None,
                },
            )?;
            output.flush()?;
            continue;
        };

        if state.approvals.requests[request_index].status != ApprovalRequestStatus::Pending {
            continue;
        }

        // Reserve a control-queue slot BEFORE `apply_approval_decision`
        // consumes durable approval state (status, journal, trust) — but only
        // when resolving would actually enqueue a fallback Agent
        // continuation. Direct delivery to the owning provider run, foreground
        // shell handoffs, and paths that stop the run first consume no queue
        // slot and must never be blocked: the provider is waiting for exactly
        // this resolution, and rejecting it would deadlock until it times out.
        if approval_resolution_needs_queue_slot(state, &state.approvals.requests[request_index])
            && !control_queue_has_capacity(state)
        {
            crate::slash::session::render_control_queue_full_notice(state, output)?;
            output.flush()?;
            continue;
        }

        if let Some(decision) = apply_approval_decision(state, request_index, command.kind) {
            // Capture the consented run before delivery: fallback handoff
            // paths may stop the run (clearing the consent state) while the
            // remaining queued requests of this turn still must be swept.
            let sweep_run_id = (command.kind == ApprovalCommandKind::ApproveTurn
                && decision.request.status == ApprovalRequestStatus::Approved)
                .then(|| decision.request.run_id.clone());
            // When a sweep follows, it owns the final next-card repaint so
            // requests it is about to resolve never flash their own card.
            let render_next_card = sweep_run_id.is_none();
            deliver_approval_decision(
                decision,
                Some(event_index),
                adapter,
                state,
                output,
                render_next_card,
            )?;
            if let Some(run_id) = sweep_run_id {
                // Swept resolutions are not tied to the user's input event:
                // pass None so any recovery/continuation run they spawn does
                // not anchor selectable-command state to an unrelated event
                // index (review note on PR #1825).
                sweep_batch_consented_requests(&run_id, None, adapter, state, output)?;
            }
        }
        output.flush()?;
    }

    Ok(())
}

/// Deliver a resolved approval decision to its provider/handoff path and
/// refresh the card panel. Shared by user decisions and the batch-consent
/// sweep so both go through the exact same delivery pipeline; the sweep
/// suppresses next-card rendering until it finishes so requests it is
/// about to resolve never flash their own card.
fn deliver_approval_decision<W: Write>(
    decision: crate::approval::resolution::AppliedApprovalDecision,
    event_index: Option<usize>,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    render_next_card: bool,
) -> std::io::Result<()> {
    if decision.request.kind == ApprovalRequestKind::TurnExtension {
        clear_active_approval_panel(state, output)?;
        render_approval_resolution(state, &decision.request, decision.title, output)?;
        crate::agent::turn_extension::resolve_turn_extension(
            &decision.request,
            adapter,
            state,
            output,
            event_index,
        )?;
        if render_next_card {
            render_current_approval_request(state, output)?;
        }
        return Ok(());
    }

    if let Some(ref ctrl_request_id) = decision.request.request_id {
        let outcome = approval_outcome_for_request(state, &decision.request);
        if outcome == ApprovalOutcome::ProviderNativeShellFallback {
            let response = provider_approval_response(&decision.request, ctrl_request_id);
            let delivery = respond_provider_approval_to_owner(state, &decision.request, response);
            if delivery == ProviderApprovalDelivery::Responded {
                mark_provider_approval_resolved(state);
            }
            clear_active_approval_panel(state, output)?;
            render_approval_resolution(state, &decision.request, decision.title, output)?;
            if render_next_card {
                render_current_approval_request(state, output)?;
            }
            if delivery == ProviderApprovalDelivery::Responded {
                flush_held_agent_events(state, output)?;
            } else {
                recover_undelivered_provider_approval(
                    delivery,
                    &decision.request,
                    event_index,
                    adapter,
                    state,
                    output,
                )?;
            }
            return Ok(());
        }

        if outcome == ApprovalOutcome::ForegroundShellHandoff {
            render_approval_resolution(state, &decision.request, decision.title, output)?;
            let active_owner = state
                .agent_run
                .active
                .as_ref()
                .is_some_and(|run| active_run_owns_provider_approval(run, &decision.request));
            if decision.request.status == ApprovalRequestStatus::Approved && active_owner {
                mark_provider_approval_resolved(state);
            }
            if active_owner && !request_can_receive_host_executed_result(state, &decision.request) {
                stop_active_agent_run_without_rendering(state, output)?;
            }
            queue_approved_shell_handoff(state, &decision.request);
            if render_next_card {
                render_current_approval_request(state, output)?;
            }
            return Ok(());
        }

        let response = provider_approval_response(&decision.request, ctrl_request_id);
        let delivery = respond_provider_approval_to_owner(state, &decision.request, response);
        if decision.request.status == ApprovalRequestStatus::Approved
            && delivery == ProviderApprovalDelivery::Responded
        {
            mark_provider_approval_resolved(state);
        }
        clear_active_approval_panel(state, output)?;
        render_approval_resolution(state, &decision.request, decision.title, output)?;
        if render_next_card {
            render_current_approval_request(state, output)?;
        }
        if delivery == ProviderApprovalDelivery::Responded {
            flush_held_agent_events(state, output)?;
        } else {
            recover_undelivered_provider_approval(
                delivery,
                &decision.request,
                event_index,
                adapter,
                state,
                output,
            )?;
        }
    } else {
        render_approval_resolution(state, &decision.request, decision.title, output)?;
        if decision.run_approved_tool {
            mark_provider_approval_resolved(state);
            stop_active_agent_run_without_rendering(state, output)?;
            queue_approved_shell_handoff(state, &decision.request);
        } else if should_send_approval_resolution_to_agent(state, &decision.request) {
            stop_active_agent_run_without_rendering(state, output)?;
            let request = approval_resolution_agent_request(&decision.request);
            // The approval was already resolved (state, journal, and
            // possibly trust updated); this continuation must not be
            // rejected by a full queue, so it is guaranteed a slot.
            start_agent_run_control_response(
                &request,
                decision.request.origin,
                adapter,
                state,
                output,
                event_index,
            )?;
        }
        if render_next_card {
            render_current_approval_request(state, output)?;
        }
    }
    Ok(())
}

/// After the user grants turn-scope batch consent, sweep the queue: resolve
/// every remaining pending request of the consented run that the consent
/// covers, each through the same resolution + delivery pipeline as a user
/// decision (issue #1773). Entries that would need a control-queue slot when
/// none is available stay pending and fall back to the regular card flow.
pub(crate) fn sweep_batch_consented_requests<W: Write>(
    run_id: &str,
    event_index: Option<usize>,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    while let Some(request_index) = state
        .approvals
        .requests
        .iter()
        .position(|request| batch_consent_covers_request(request, run_id))
    {
        if approval_resolution_needs_queue_slot(state, &state.approvals.requests[request_index])
            && !control_queue_has_capacity(state)
        {
            break;
        }
        let Some(decision) = apply_batch_consent_decision(state, request_index) else {
            break;
        };
        deliver_approval_decision(decision, event_index, adapter, state, output, false)?;
    }
    // Render whatever is still pending (high risk, hooks, foreign runs)
    // exactly once after the sweep settles.
    render_current_approval_request(state, output)?;
    Ok(())
}

fn respond_active_run_approval(
    active_run: &mut ActiveAgentRun,
    response: ApprovalResponse,
) -> bool {
    let responded = active_run.handle.respond_approval(response).is_ok();
    if responded {
        active_run.last_activity_at = std::time::Instant::now();
    }
    responded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderApprovalDelivery {
    Responded,
    OwnerUnavailable,
    DeliveryFailed,
}

fn respond_provider_approval_to_owner(
    state: &mut InlineState,
    request: &RuntimeApprovalRequest,
    response: ApprovalResponse,
) -> ProviderApprovalDelivery {
    let Some(active_run) = state.agent_run.active.as_mut() else {
        return ProviderApprovalDelivery::OwnerUnavailable;
    };
    if !active_run_owns_provider_approval(active_run, request) {
        return ProviderApprovalDelivery::OwnerUnavailable;
    }
    if respond_active_run_approval(active_run, response) {
        // #1940: settle the lifecycle ledger so the drain sweeps know this
        // control request reached its terminal state.
        if let Some(request_id) = request.request_id.as_deref() {
            state
                .control
                .approval_ledger_mut()
                .mark_responded(&request.run_id, request_id);
        }
        ProviderApprovalDelivery::Responded
    } else {
        ProviderApprovalDelivery::DeliveryFailed
    }
}

/// #1940 terminal-state guarantee: deny message for a control request the
/// rendering pipeline dropped before it reached any decision surface.
pub(crate) const DROPPED_CONTROL_REQUEST_DENY_MESSAGE: &str = "cosh-shell could not route this approval request to a decision surface, so it was denied by the terminal-state guarantee. The request was not executed; retry it in a new turn if still needed.";

fn unhomed_control_request_ids(state: &InlineState, run_id: &str) -> Vec<String> {
    state
        .control
        .approval_ledger()
        .unresponded_for_run(run_id)
        .into_iter()
        .filter(|request_id| {
            !state.approvals.requests.iter().any(|request| {
                request.run_id == run_id
                    && request.request_id.as_deref() == Some(request_id.as_str())
            })
        })
        .collect()
}

fn dropped_control_request_deny_response(request_id: &str) -> ApprovalResponse {
    crate::approval::broker::provider_deny_response(
        crate::approval::broker::ProviderResponseInput {
            request_id,
            tool_use_id: None,
            tool_input: None,
        },
        DROPPED_CONTROL_REQUEST_DENY_MESSAGE.to_string(),
    )
}

/// #1940 terminal-state guarantee: deny message for a control request that
/// arrived under a run id other than the active run. Every sweep is scoped
/// to the owning run, so the shell can never take terminal ownership of
/// such a request — it is rejected at the registration door instead.
pub(crate) const FOREIGN_RUN_REQUEST_DENY_MESSAGE: &str = "cosh-shell rejected this approval request because it arrived under a run that does not own it (protocol violation). The request was not executed.";

pub(crate) fn foreign_run_request_deny_response(request_id: &str) -> ApprovalResponse {
    crate::approval::broker::provider_deny_response(
        crate::approval::broker::ProviderResponseInput {
            request_id,
            tool_use_id: None,
            tool_input: None,
        },
        FOREIGN_RUN_REQUEST_DENY_MESSAGE.to_string(),
    )
}

/// #1940 I1 batch drain: after a governed-event batch settles, every
/// registered control request must either have a home in
/// `approvals.requests` (card flow, auto-approve, or decision pipeline own
/// its response) or have been responded to already. Anything else was
/// dropped mid-pipeline and is denied on the spot instead of silently
/// starving the provider. Requests with a home are never touched here, so
/// a surfaced card or a running host-executed command is never timed out
/// or overridden.
pub(crate) fn drain_unhomed_control_requests(state: &mut InlineState) {
    let Some(run_id) = state
        .agent_run
        .active
        .as_ref()
        .map(|run| run.request.id.clone())
    else {
        return;
    };
    for request_id in unhomed_control_request_ids(state, &run_id) {
        // Audit the drop before responding so a user denial and a
        // dropped-before-decision request stay distinguishable, with the
        // drop site attached.
        if let Some(audit) = state.audit.as_mut() {
            audit.record_approval_dropped(&run_id, &request_id, "batch_drain");
        }
        if let Some(active_run) = state.agent_run.active.as_ref() {
            let _ = active_run
                .handle
                .respond_approval(dropped_control_request_deny_response(&request_id));
        }
        state
            .control
            .approval_ledger_mut()
            .mark_responded(&run_id, &request_id);
    }
}

/// #1940 run-terminal sweep: same contract as the batch drain, for the
/// paths where the run has already been detached from `InlineState`
/// (finish/stop/cancel). Also clears the run's ledger entries so the
/// accounting index cannot grow across turns. Pending requests that still
/// have a card keep their existing late-decision recovery path
/// (`OwnerUnavailable` -> continuation) and are not denied here.
pub(crate) fn drain_unhomed_control_requests_with_handle(
    state: &mut InlineState,
    run_id: &str,
    handle: &AgentRunHandle,
) {
    for request_id in unhomed_control_request_ids(state, run_id) {
        if let Some(audit) = state.audit.as_mut() {
            audit.record_approval_dropped(run_id, &request_id, "run_terminal_sweep");
        }
        let _ = handle.respond_approval(dropped_control_request_deny_response(&request_id));
        state
            .control
            .approval_ledger_mut()
            .mark_responded(run_id, &request_id);
    }
    state.control.approval_ledger_mut().clear_run(run_id);
}

fn active_run_owns_provider_approval(
    active_run: &ActiveAgentRun,
    request: &RuntimeApprovalRequest,
) -> bool {
    active_run.governed_events.iter().any(|event| {
        matches!(
            &event.event,
            AgentEvent::ToolPermissionRequest {
                run_id,
                request_id,
                ..
            } if run_id == &request.run_id
                && Some(request_id.as_str()) == request.request_id.as_deref()
        )
    })
}

/// Whether resolving this approval would consume a control-queue slot.
///
/// Mirrors the delivery plan in [`render_approval_actions`]:
/// - a pending or running compaction holds every continuation in the queue;
/// - with no active run the recovery continuation starts immediately;
/// - a control request owned by the active run is delivered directly (a
///   runtime delivery failure stops that run first, so its recovery also
///   starts immediately);
/// - approvals without a control request id stop the run before any
///   continuation;
/// - only a non-owner active run is kept alive by the `OwnerUnavailable`
///   recovery and forces the continuation into the queue. (This is slightly
///   conservative for handoff outcomes that never enqueue, which is safe:
///   the card stays pending and retryable.)
fn approval_resolution_needs_queue_slot(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    if request.kind == ApprovalRequestKind::TurnExtension {
        return false;
    }
    if crate::slash::session::compaction_pending_or_active(state) {
        return true;
    }
    let Some(active_run) = state.agent_run.active.as_ref() else {
        return false;
    };
    if request.request_id.is_none() {
        return false;
    }
    !active_run_owns_provider_approval(active_run, request)
}

fn recover_undelivered_provider_approval<W: Write>(
    delivery: ProviderApprovalDelivery,
    request: &RuntimeApprovalRequest,
    event_index: Option<usize>,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if delivery == ProviderApprovalDelivery::DeliveryFailed {
        stop_active_agent_run_without_rendering(state, output)?;
    }
    let continuation = approval_resolution_agent_request(request);
    // Recovery of an undelivered approval resolution: the approval is already
    // resolved, so this control-protocol continuation is guaranteed a queue
    // slot rather than risking a queue-full rejection it cannot retry.
    start_agent_run_control_response(
        &continuation,
        request.origin,
        adapter,
        state,
        output,
        event_index,
    )
    .map(|_disposition| ())
}

pub(crate) fn render_approval_resolution<W: Write>(
    state: &mut InlineState,
    request: &RuntimeApprovalRequest,
    title: MessageId,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_approval_panel(state, output)?;
    write_approval_receipt(state.language, request, state.i18n().t(title), output)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
