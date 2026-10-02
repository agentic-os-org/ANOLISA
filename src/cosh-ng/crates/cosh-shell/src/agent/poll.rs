use std::time::Duration;

use crate::agent::continuation::run_request_is_analysis_only_continuation;
use crate::agent::events::{
    active_run_has_unrendered_interaction, render_active_agent_event,
    render_new_agent_structured_events, state_has_pending_interaction, TextHoldReason,
};
use crate::agent::finish::finish_active_agent_run;
use crate::agent::heartbeat::render_agent_heartbeat;
use crate::agent::run::{
    has_queued_run_before_held_text, start_agent_run_with_origin, ActiveAgentRun,
};
use crate::approval::broker::{provider_deny_response, ProviderResponseInput};
use crate::runtime::evidence_delivery::stalled_provider_shell_handoff_continuation_request;
use crate::runtime::prelude::*;

const DEFAULT_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS: u64 = 15;

pub(crate) fn poll_active_agent_run<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    adapter: &AdapterInstance,
) -> std::io::Result<()> {
    poll_active_agent_run_with_policy(state, output, adapter, false, true, true, false)
}

pub(crate) fn poll_active_agent_run_deferred<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    adapter: &AdapterInstance,
) -> std::io::Result<()> {
    if let Some(active_run) = state.agent_run.active.as_mut() {
        active_run.status_animation.clear(output)?;
        output.flush()?;
    }
    poll_active_agent_run_with_policy(state, output, adapter, true, true, false, true)
}

fn poll_active_agent_run_with_policy<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    adapter: &AdapterInstance,
    force_hold_output: bool,
    render_structured: bool,
    finish_completed: bool,
    suppress_heartbeat: bool,
) -> std::io::Result<()> {
    let mut should_finish = false;
    let mut stalled_shell_recovery: Option<(AgentRequest, AgentRunOrigin, Option<usize>)> = None;
    // cosh-core emits a versioned
    // `compaction_recommended_v1:<session>:<gen>:<rev>:<hist>:<usable>` status
    // at the idle boundary of a turn, delivered just before the turn's
    // buffered terminal result. Capture the payload here and apply it after
    // the borrow of the active run is released, so the shell can start the
    // background compactor from the next safe prompt boundary — bound to the
    // exact session and revision the recommendation names.
    let mut pending_recommendation: Option<String> = None;
    loop {
        let pending_interaction_before_poll = state_has_pending_interaction(state);
        let queued_before_held_text = has_queued_run_before_held_text(state);
        let unrendered_interaction_pending = state
            .agent_run
            .active
            .as_ref()
            .is_some_and(active_run_has_unrendered_interaction);
        let shell_handoff_in_progress = state.control.shell_handoff().has_active_handoff();
        let deny_shell_during_recovery =
            state.agent_run.active.as_ref().is_some_and(|active_run| {
                run_request_is_analysis_only_continuation(Some(&active_run.request))
            });
        let analysis_only_recovery_pending = deny_shell_during_recovery;
        let provider_native_shell_tool_call_pending = adapter.capabilities().control_protocol
            && state
                .agent_run
                .active
                .as_ref()
                .is_some_and(active_run_has_unrendered_provider_native_shell_tool_call);
        let provider_native_shell_transcript_pending = adapter.capabilities().control_protocol
            && state
                .agent_run
                .active
                .as_ref()
                .is_some_and(active_run_has_unrendered_provider_native_shell_transcript);
        let provider_native_shell_result_pending = adapter.capabilities().control_protocol
            && state.agent_run.active.as_ref().is_some_and(|active_run| {
                active_run_has_pending_provider_native_shell_result(active_run, state)
            });
        let provider_native_shell_result_idle = provider_native_shell_result_pending
            && state
                .agent_run
                .active
                .as_ref()
                .is_some_and(active_run_has_stalled_shell_evidence_delivery);
        let provider_shell_activity_pending = shell_handoff_in_progress
            || provider_native_shell_tool_call_pending
            || provider_native_shell_transcript_pending
            || (provider_native_shell_result_pending && !provider_native_shell_result_idle);
        let active_run_idle_for_stall = state
            .agent_run
            .active
            .as_ref()
            .is_some_and(active_run_has_stalled_shell_evidence_delivery);
        let stalled_provider_shell_fallback =
            should_start_stalled_provider_shell_fallback(StalledProviderShellFallbackInputs {
                provider_shell_activity_pending,
                pending_interaction: pending_interaction_before_poll,
                queued_before_held_text,
                unrendered_interaction: unrendered_interaction_pending,
                active_run_idle: active_run_idle_for_stall,
            })
            .then(|| stalled_provider_shell_handoff_continuation_request(state))
            .flatten();
        let poll_timeout = if provider_native_shell_tool_call_pending
            || provider_native_shell_transcript_pending
            || provider_native_shell_result_pending
            || analysis_only_recovery_pending
            || (state.agent_run.host_executed_shell_result_delivered
                && !pending_interaction_before_poll
                && !queued_before_held_text)
        {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(0)
        };
        let Some(active_run) = state.agent_run.active.as_mut() else {
            return Ok(());
        };
        if active_run.completed {
            active_run.status_animation.clear(output)?;
            should_finish = finish_completed;
            break;
        }

        let event = match active_run.handle.poll_event_timeout(poll_timeout) {
            Ok(AgentRunPoll::Event(event)) => event,
            Ok(AgentRunPoll::Timeout) => {
                if let Some((fallback, origin)) = stalled_provider_shell_fallback {
                    stalled_shell_recovery =
                        Some((fallback, origin, active_run.selectable_after_event_index));
                    break;
                }
                if pending_interaction_before_poll
                    || queued_before_held_text
                    || unrendered_interaction_pending
                {
                    active_run.status_animation.clear(output)?;
                    output.flush()?;
                    break;
                }
                render_agent_heartbeat(
                    active_run,
                    output,
                    suppress_heartbeat || shell_handoff_in_progress,
                )?;
                output.flush()?;
                break;
            }
            Ok(AgentRunPoll::Finished) => {
                should_finish = true;
                break;
            }
            Err(err) => AgentEvent::AgentFailed {
                run_id: active_run.request.id.clone(),
                error: err.message,
                error_code: None,
                max_turns: None,
            },
        };

        if let AgentEvent::ShellEvidenceRequest {
            run_id,
            request_id,
            tool_use_id,
            action,
        } = &event
        {
            crate::agent::heartbeat::render_agent_shell_evidence_pending_status(
                active_run, output,
            )?;
            output.flush()?;
            let duplicate_provider_request = state
                .shell_evidence
                .record_action_signature(run_id, shell_evidence_action_signature(action));
            let result = match action {
                crate::adapter::ShellEvidenceAction::ListCommands { limit, cursor } => {
                    crate::runtime::shell_evidence::list_shell_evidence_commands(
                        &state.session_blocks,
                        *limit,
                        cursor.as_deref(),
                    )
                }
                crate::adapter::ShellEvidenceAction::ReadOutput {
                    output_id,
                    direction,
                    lines,
                    bypass_recent_filter,
                } => {
                    let excerpt_recently_delivered =
                        state.shell_evidence.read_output_excerpt_recently_delivered(
                            output_id,
                            Some(run_id.as_str()),
                            direction.as_str(),
                            *lines,
                        );
                    let recently_delivered = excerpt_recently_delivered
                        || (!*bypass_recent_filter
                            && state.shell_evidence.read_output_recently_delivered(
                                output_id,
                                Some(run_id.as_str()),
                                direction.as_str(),
                                *lines,
                            ));
                    if recently_delivered {
                        crate::runtime::shell_evidence::shell_evidence_read_unavailable_guard(
                            &state.session_blocks,
                            state.approval_mode,
                            output_id,
                            direction.as_str(),
                            *lines,
                        )
                        .unwrap_or_else(|| {
                            crate::runtime::shell_evidence::already_delivered_shell_evidence_result(
                                output_id,
                                direction.as_str(),
                                *lines,
                            )
                        })
                    } else {
                        crate::runtime::shell_evidence::read_shell_evidence_output(
                            &state.session_blocks,
                            state.approval_mode,
                            output_id,
                            direction.as_str(),
                            *lines,
                        )
                    }
                }
            };
            let status =
                if result.metadata.reason.as_deref() == Some("redacted_confirmation_required") {
                    "redacted_confirmation_required".to_string()
                } else {
                    result.metadata.excerpt_status.clone()
                };
            let output_id = match action {
                crate::adapter::ShellEvidenceAction::ReadOutput { output_id, .. } => {
                    Some(output_id.clone())
                }
                crate::adapter::ShellEvidenceAction::ListCommands { .. } => None,
            };
            state.shell_evidence.last_action =
                Some(crate::runtime::state::ShellEvidenceActionRecord {
                    mode: "control_protocol_tool",
                    request_id: request_id.clone(),
                    tool_use_id: tool_use_id.clone(),
                    action: action.as_str().to_string(),
                    output_id: output_id.clone(),
                    status: status.clone(),
                    failure_reason: result.metadata.reason.clone(),
                });
            if let crate::adapter::ShellEvidenceAction::ReadOutput {
                output_id,
                direction,
                lines,
                ..
            } = action
            {
                let command =
                    crate::runtime::shell_evidence::command_preview_for_terminal_output_id(
                        &state.session_blocks,
                        output_id,
                    );
                if result.metadata.excerpt_status == "available" && !result.metadata.is_error {
                    state.shell_evidence.record_shell_evidence_read_output(
                        output_id.clone(),
                        Some(run_id.clone()),
                        direction.as_str().to_string(),
                        *lines,
                    );
                }
                crate::activity::runtime::record_shell_evidence_action(
                    state.language,
                    &mut state.activity.rows,
                    &mut state.activity.tool_invocations,
                    run_id,
                    request_id,
                    tool_use_id,
                    action.as_str(),
                    Some(output_id),
                    Some(direction.as_str()),
                    Some(*lines),
                    &status,
                    result.metadata.reason.as_deref(),
                    command.as_deref(),
                    None,
                    false,
                    duplicate_provider_request,
                );
            } else {
                crate::activity::runtime::record_shell_evidence_action(
                    state.language,
                    &mut state.activity.rows,
                    &mut state.activity.tool_invocations,
                    run_id,
                    request_id,
                    tool_use_id,
                    action.as_str(),
                    output_id.as_deref(),
                    None,
                    None,
                    &status,
                    result.metadata.reason.as_deref(),
                    None,
                    result.metadata.command_count,
                    result.metadata.next_cursor.is_some(),
                    duplicate_provider_request,
                );
            }
            let _ = active_run.handle.respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: None,
                tool_input: None,
                decision: ApprovalDecision::ShellEvidence {
                    result: Box::new(result),
                },
            });
        }
        let terminal_event = matches!(
            event,
            AgentEvent::AgentCompleted { .. }
                | AgentEvent::AgentFailed { .. }
                | AgentEvent::AgentCancelled { .. }
        );
        let structured_result_boundary = matches!(
            event,
            AgentEvent::ToolCompleted { .. }
                | AgentEvent::ShellEvidenceRequest { .. }
                | AgentEvent::UserQuestion { .. }
                | AgentEvent::AuthRequired { .. }
                | AgentEvent::Action { .. }
                | AgentEvent::ToolPermissionRequest { .. }
        );
        register_control_approval_on_first_sight(
            active_run,
            state.control.approval_ledger_mut(),
            state.audit.as_mut(),
            &event,
        );
        let deny_reentrant_shell_request = deny_shell_during_recovery;
        if let Some((denied_run_id, denied_request_id)) =
            deny_reentrant_shell_request_after_foreground_evidence(
                active_run,
                &event,
                deny_reentrant_shell_request,
            )
        {
            state
                .control
                .approval_ledger_mut()
                .mark_responded(&denied_run_id, &denied_request_id);
        }
        let provider_progress_observed = shell_evidence_provider_progress_observed(&event);
        let text_hold_reason = text_hold_reason_for_poll(TextHoldInputs {
            pending_interaction_before_poll,
            queued_before_held_text,
            unrendered_interaction: unrendered_interaction_pending,
            provider_native_shell_transcript_pending,
            provider_native_shell_result_pending,
            force_hold_output,
        });
        if let Some(model) = foreground_model_from_event(&event) {
            state.personalization.foreground_model = Some(model.to_string());
        }
        if let AgentEvent::StatusChanged { phase, .. } = &event {
            if let Some(payload) = phase.strip_prefix("compaction_recommended_v1:") {
                pending_recommendation = Some(payload.to_string());
            }
        }
        render_active_agent_event(active_run, event, output, text_hold_reason)?;
        if provider_progress_observed {
            state
                .evidence
                .mark_provider_progress_observed(terminal_event);
        }
        output.flush()?;
        if terminal_event {
            active_run.status_animation.clear(output)?;
            active_run.completed = true;
            should_finish = finish_completed;
            break;
        }
        if structured_result_boundary {
            break;
        }
    }

    // The active-run borrow is released; record any idle-boundary compaction
    // recommendation so the next `poll_background_compaction` can start the
    // background compactor without blocking the shell prompt.
    if let Some(payload) = pending_recommendation {
        crate::slash::session::note_compaction_recommendation(state, &payload);
    }

    if let Some((fallback, origin, selectable_after_event_index)) = stalled_shell_recovery {
        if let Some(mut active_run) = state.agent_run.active.take() {
            // #1940: fresh-turn recovery discards the run; sweep it first
            // so dropped control requests still reach a terminal state
            // and the ledger cannot grow across turns.
            crate::approval::runtime::drain_unhomed_control_requests_with_handle(
                state,
                &active_run.request.id,
                &active_run.handle,
            );
            active_run.handle.cancel();
            active_run.status_animation.clear(output)?;
        }
        // Turn-scope batch consent never outlives its run (issue #1773).
        state.control.trust.clear_run_batch_consent();
        // Evidence-idle recovery is an internal resumable continuation.
        start_agent_run_with_origin(
            &fallback,
            origin,
            AgentStartIntent::InternalBestEffort,
            adapter,
            state,
            output,
            selectable_after_event_index,
        )?;
        return Ok(());
    }

    if render_structured {
        render_new_agent_structured_events(state, output, adapter)?;
        output.flush()?;
        if !suppress_heartbeat
            && !state_has_pending_interaction(state)
            && !state.control.shell_handoff().has_active_handoff()
        {
            if let Some(active_run) = state.agent_run.active.as_mut() {
                if !active_run.completed {
                    render_agent_heartbeat(active_run, output, false)?;
                    output.flush()?;
                }
            }
        }
    }

    if should_finish {
        finish_active_agent_run(state, output, adapter)?;
    }

    Ok(())
}

/// #1940: register every control approval owned by the active run on
/// first sight so the batch drain and run-terminal sweeps can prove it
/// reached a terminal state even if a later pipeline stage drops it, and
/// send a receipt back to the core so its residual approval timeout
/// disarms. A request under a foreign run id is a protocol violation the
/// ledger could never sweep: it is denied at the door instead — no
/// ledger entry, no receipt, exactly one terminal response.
fn register_control_approval_on_first_sight(
    active_run: &ActiveAgentRun,
    ledger: &mut crate::runtime::approval_ledger::ApprovalLifecycleLedger,
    audit: Option<&mut crate::journal::audit::ShellAuditRecorder>,
    event: &AgentEvent,
) {
    let AgentEvent::ToolPermissionRequest {
        run_id, request_id, ..
    } = event
    else {
        return;
    };
    if run_id != &active_run.request.id {
        // #1940 fail-closed: every drain and sweep is scoped to the owning
        // run, so the shell can never take terminal ownership of a request
        // registered under a foreign run id — registering or receipting it
        // would disarm the core's last-resort guard and then wait forever.
        // Reject at the door instead: deny so the core's wait reaches a
        // terminal state, audit the protocol violation, and keep the
        // request out of the ledger.
        tracing::warn!(
            event_run_id = %run_id,
            active_run_id = %active_run.request.id,
            request_id = %request_id,
            "control approval arrived under a non-active run id; denying without registering"
        );
        if let Some(audit) = audit {
            audit.record_approval_dropped(run_id, request_id, "foreign_run_rejected");
        }
        let _ = active_run.handle.respond_approval(
            crate::approval::runtime::foreign_run_request_deny_response(request_id),
        );
        return;
    }
    ledger.register(run_id, request_id);
    // #1940 receipt protocol: prove to the core that this request reached
    // the shell main thread so it can disarm the residual approval
    // timeout. Best-effort and idempotent: a lost receipt only means the
    // core keeps its last-resort guard, and repeat receipts for the same
    // request are harmless.
    let _ = active_run.handle.send_approval_receipt(request_id);
}

fn foreground_model_from_event(event: &AgentEvent) -> Option<&str> {
    let AgentEvent::StatusChanged { message, .. } = event else {
        return None;
    };
    message
        .strip_prefix("model initialized ")
        .or_else(|| message.strip_prefix("model status: model_switched:"))
        .map(str::trim)
        .filter(|model| !model.is_empty())
}

fn shell_evidence_action_signature(action: &crate::adapter::ShellEvidenceAction) -> String {
    match action {
        crate::adapter::ShellEvidenceAction::ListCommands { limit, cursor } => {
            format!(
                "list_commands:limit={limit}:cursor={}",
                cursor.as_deref().unwrap_or("<none>")
            )
        }
        crate::adapter::ShellEvidenceAction::ReadOutput {
            output_id,
            direction,
            lines,
            ..
        } => format!(
            "read_output:output_id={output_id}:direction={}:lines={lines}",
            direction.as_str()
        ),
    }
}

fn active_run_has_stalled_shell_evidence_delivery(active_run: &ActiveAgentRun) -> bool {
    active_run.last_activity_at.elapsed() >= shell_evidence_idle_timeout()
}

fn shell_evidence_idle_timeout() -> Duration {
    shell_evidence_idle_timeout_from_config(
        std::env::var("COSH_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

// Resolve the idle window from its configured value, falling back to the
// default when it is missing, malformed, or zero. Zero has to be rejected
// rather than honoured: `elapsed() >= Duration::ZERO` holds on the very first
// poll of a run, so a zero window would report every active run as stalled and
// restart the fallback continuation on every poll cycle. Parsing is split out
// of the env read so the boundaries stay testable without mutating the
// process-global environment.
fn shell_evidence_idle_timeout_from_config(configured: Option<&str>) -> Duration {
    Duration::from_secs(
        configured
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .unwrap_or(DEFAULT_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS),
    )
}

#[derive(Debug, Clone, Copy, Default)]
struct StalledProviderShellFallbackInputs {
    provider_shell_activity_pending: bool,
    pending_interaction: bool,
    queued_before_held_text: bool,
    unrendered_interaction: bool,
    active_run_idle: bool,
}

fn should_start_stalled_provider_shell_fallback(
    inputs: StalledProviderShellFallbackInputs,
) -> bool {
    inputs.active_run_idle
        && !inputs.provider_shell_activity_pending
        && !inputs.pending_interaction
        && !inputs.queued_before_held_text
        && !inputs.unrendered_interaction
}

fn active_run_has_unrendered_provider_native_shell_tool_call(active_run: &ActiveAgentRun) -> bool {
    active_run.governed_events[active_run.rendered_governed_event_count..]
        .iter()
        .any(|event| {
            matches!(
                &event.event,
                AgentEvent::ToolCall { name, .. } if is_shell_tool_name(name)
            )
        })
}

fn active_run_has_unrendered_provider_native_shell_transcript(active_run: &ActiveAgentRun) -> bool {
    let shell_tool_ids = active_run
        .governed_events
        .iter()
        .filter_map(|event| match &event.event {
            AgentEvent::ToolCall {
                tool_id: Some(tool_id),
                name,
                ..
            } if is_shell_tool_name(name) => Some(tool_id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();

    active_run.governed_events[active_run.rendered_governed_event_count..]
        .iter()
        .any(|event| match &event.event {
            AgentEvent::ToolOutputDelta { tool_id, .. }
            | AgentEvent::ToolCompleted { tool_id, .. } => {
                shell_tool_ids.contains(tool_id.as_str())
            }
            _ => false,
        })
}

fn deny_reentrant_shell_request_after_foreground_evidence(
    active_run: &ActiveAgentRun,
    event: &AgentEvent,
    deny_shell_after_foreground_evidence: bool,
) -> Option<(String, String)> {
    if !deny_shell_after_foreground_evidence {
        return None;
    }
    let AgentEvent::ToolPermissionRequest {
        run_id,
        request_id,
        tool_name,
        tool_input,
        tool_use_id,
        ..
    } = event
    else {
        return None;
    };
    // #1940: a foreign-run request was already denied at the registration
    // door above; a second response here would contradict that terminal
    // deny, and the ledger holds no entry to mark responded.
    if run_id != &active_run.request.id {
        return None;
    }
    if !is_shell_tool_name(tool_name) {
        return None;
    }
    // #1940: report the response either way so the ledger sweep does not
    // double-deny this request; a failed send means the channel is gone
    // and the sweep could not deliver a second response anyway.
    let _ = active_run.handle.respond_approval(provider_deny_response(
        ProviderResponseInput {
            request_id,
            tool_use_id: Some(tool_use_id),
            tool_input: Some(tool_input),
        },
        "The foreground shell command already completed and its output was injected. Summarize the existing shell evidence or ask the user to start a new request before running another shell command.".to_string(),
    ));
    Some((run_id.clone(), request_id.clone()))
}

fn active_run_has_pending_provider_native_shell_result(
    active_run: &ActiveAgentRun,
    state: &InlineState,
) -> bool {
    active_run.governed_events.iter().any(|event| {
        let AgentEvent::ToolCall {
            tool_id: Some(tool_id),
            name,
            ..
        } = &event.event
        else {
            return false;
        };
        is_shell_tool_name(name)
            && !active_run
                .governed_events
                .iter()
                .any(|event| matches!(&event.event, AgentEvent::ToolCompleted { tool_id: completed_tool_id, .. } if completed_tool_id == tool_id))
            && !state
                .control
                .provider_shell_transcript_output_seen(&active_run.request.id, tool_id)
    })
}

fn shell_evidence_provider_progress_observed(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TextDelta { .. }
            | AgentEvent::ToolCall { .. }
            | AgentEvent::ToolPermissionRequest { .. }
            | AgentEvent::ToolOutputDelta { .. }
            | AgentEvent::ToolCompleted { .. }
            | AgentEvent::UserQuestion { .. }
            | AgentEvent::ShellEvidenceRequest { .. }
            | AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. }
    )
}

#[derive(Debug, Clone, Copy, Default)]
struct TextHoldInputs {
    pending_interaction_before_poll: bool,
    queued_before_held_text: bool,
    unrendered_interaction: bool,
    provider_native_shell_transcript_pending: bool,
    provider_native_shell_result_pending: bool,
    force_hold_output: bool,
}

fn text_hold_reason_for_poll(inputs: TextHoldInputs) -> Option<TextHoldReason> {
    if inputs.pending_interaction_before_poll {
        return Some(TextHoldReason::InteractionPending);
    }
    if inputs.queued_before_held_text {
        return Some(TextHoldReason::QueuedBeforeHeldText);
    }
    if inputs.unrendered_interaction {
        return Some(TextHoldReason::UnrenderedInteraction);
    }
    if inputs.provider_native_shell_transcript_pending {
        return Some(TextHoldReason::PostToolShellTranscript);
    }
    if inputs.provider_native_shell_result_pending {
        return Some(TextHoldReason::PostToolShellResult);
    }
    if inputs.force_hold_output {
        return Some(TextHoldReason::ForcedDeferredPoll);
    }
    None
}

#[cfg(test)]
mod tests;
