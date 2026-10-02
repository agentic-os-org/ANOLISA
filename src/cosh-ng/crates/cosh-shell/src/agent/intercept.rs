use crate::evidence::model::OutputExcerptDirection;
use crate::evidence::output_policy::bounded_output_excerpt_for_block;
use crate::insight::evidence::{
    build_provider_evidence_payload, provider_target_facts, take_bound_insight_metadata,
    trim_optional_context_hints, EvidenceBundleInput,
};
use crate::recommendation::personal_integration::activity_context;
use crate::runtime::prelude::*;
use crate::runtime::state::PendingInputGhostBinding;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalPromptAction<'a> {
    Accepted,
    Dismissed,
    Submitted(&'a str),
}

pub(crate) fn render_intercept_agent_guidance<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        handle_personal_prompt_feedback(event, state);
        clear_dismissed_prompt_ghost_context(event, state);
        if is_prompt_ghost_feedback_event(event) {
            continue;
        }
        if !is_standalone_agent_intercept(event) {
            continue;
        }

        let key = stable_event_key("intercept", event_index, event);
        if !state.handled_intercepts.insert(key) {
            continue;
        }

        // Before handling this input, drive the background-compaction poll at
        // this safe boundary: harvest a finished compactor, render its
        // completion notice, and resume any user request that was held back
        // for the compaction. This keeps FIFO order — the pre-compaction user
        // queue starts (and becomes `agent_run.active`) before this fresh
        // input, so the new input queues behind it rather than jumping ahead —
        // and stops a just-finished compaction from making the user's first
        // input believe the Agent is still paused. It never blocks waiting on
        // the compactor; only already-finished results are processed.
        crate::slash::session::poll_background_compaction(state, output, adapter, false)?;

        // Reserve a control-queue slot BEFORE the pending question is
        // consumed below — but only when one is actually needed. This path
        // always stops the active run before starting the answer
        // continuation, so the continuation queues only while a compaction is
        // pending or running; an ordinary busy Agent delivers/starts without
        // consuming queue capacity and must not be blocked. (When the total
        // cap is hit, plain input is rejected by the same admission rule, so
        // skipping this event is consistent.)
        if state.questions.pending_id.is_some()
            && crate::slash::session::compaction_pending_or_active(state)
            && !control_queue_has_capacity(state)
        {
            crate::slash::session::render_control_queue_full_notice(state, output)?;
            output.flush()?;
            continue;
        }

        if let Some(answer_run) =
            agent_request_from_pending_question_answer(event, event_index, state)
        {
            render_question_answer_notice(state, &answer_run, output)?;
            stop_active_agent_run_without_rendering(state, output)?;
            state.agent_run.needs_prompt_after_run = event.cwd.is_none();
            // The pending question is consumed here; this control-protocol
            // response is guaranteed a queue slot so it cannot be lost to a
            // full queue (it would be unrecoverable for the user).
            start_agent_run_control_response(
                &answer_run.request,
                answer_run.origin,
                adapter,
                state,
                output,
                Some(event_index),
            )?;
            output.flush()?;
            continue;
        }

        let (request, origin) = match agent_request_from_pending_insight(event, blocks, state) {
            Some(mut request) => {
                attach_bound_insight_evidence(&mut request, state);
                (Some(request), AgentRunOrigin::InsightPrompt)
            }
            None => (
                agent_request_from_intercepted_input(event, event_index, true),
                AgentRunOrigin::Standard,
            ),
        };
        if let Some(mut request) = request {
            if let Some(submission) = state.pending_agent_composer_submission.take() {
                if request.user_input.as_deref() == Some(submission.text.trim()) {
                    if let Some(cwd) = submission.workspace_cwd.as_deref() {
                        request.command_block.cwd = cwd.to_string();
                        request.command_block.end_cwd = cwd.to_string();
                    }
                    let feedback = crate::agent::composer::attach_submission(
                        &mut request,
                        submission.workspace_cwd.as_deref(),
                    );
                    crate::runtime::prompt_draft::render_agent_composer_rejections(
                        state,
                        output,
                        &feedback.rejected_references,
                    )?;
                }
            }
            let user_input = request.user_input.clone();
            if let Some(input) = user_input.as_deref() {
                bind_pending_input_ghost_context(&mut request, state, event);
                if let Some(hint) = continuity_prompt_hint(state, input) {
                    request.context_hints.push(hint);
                }
            }
            state.agent_run.needs_prompt_after_run = event.cwd.is_none();
            render_path_prompt_notice(event, &request, state, output)?;
            // Natural-language input and user-chosen prompt ghosts are both
            // explicit user requests: they always go through the central gate,
            // which queues them (FIFO) while a compaction is pending/active
            // rather than starting a model or routing to the shell.
            dispatch_user_intercept_request(&request, origin, adapter, state, output, event_index)?;
            if let Some(input) = user_input.as_deref() {
                record_user_intent(state, input);
            }
        }
        output.flush()?;
    }

    Ok(())
}

fn render_path_prompt_notice<W: Write>(
    event: &ShellEvent,
    request: &AgentRequest,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if event.component.as_deref() != Some("path_prompt") {
        return Ok(());
    }
    let input = if crate::types::request_has_sensitive_input(request) {
        "<redacted>"
    } else {
        request.user_input.as_deref().unwrap_or_default()
    };
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title: state.i18n().t(MessageId::InterceptNoticeTitle),
            body: vec![state
                .i18n()
                .format(MessageId::InterceptNoticeBody, &[("input", input)])],
            footer: Some(state.i18n().t(MessageId::InterceptNoticeFooter)),
        },
    )
}

fn is_prompt_ghost_feedback_event(event: &ShellEvent) -> bool {
    event.kind == ShellEventKind::UserInputIntercepted
        && prompt_ghost_suggestion_id(event).is_some()
        && matches!(event.message.as_deref(), Some("accepted" | "dismissed"))
}

/// Routes an explicit user intercept request through the central Agent gate
/// and renders the appropriate notice for its disposition.
///
/// The natural-language and prompt-ghost paths never bypass the queue policy:
/// during a background compaction the gate queues the request (paused notice),
/// and when the pending queue is full it is rejected with a visible notice —
/// the input is never started as a model run or leaked to the shell here.
fn dispatch_user_intercept_request<W: Write>(
    request: &AgentRequest,
    origin: AgentRunOrigin,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index: usize,
) -> std::io::Result<()> {
    let paused_by_compaction = crate::slash::session::compaction_pending_or_active(state);
    let disposition = start_agent_run_with_origin_disposition(
        request,
        origin,
        AgentStartIntent::UserInitiated,
        adapter,
        state,
        output,
        Some(event_index),
    )?;
    match disposition {
        AgentStartDisposition::QueueFull => {
            crate::slash::session::render_agent_queue_full_notice(state, output)?;
            crate::slash::prompt::write_shell_prompt(state, output)?;
            output.flush()?;
        }
        AgentStartDisposition::Queued if paused_by_compaction => {
            crate::slash::session::render_compaction_paused_notice(state, output)?;
            crate::slash::prompt::write_shell_prompt(state, output)?;
            output.flush()?;
        }
        _ => {}
    }
    Ok(())
}

fn attach_bound_insight_evidence(request: &mut AgentRequest, state: &mut InlineState) {
    if request.command_block.exit_code != 0 {
        crate::agent::failed_command::attach_failure_evidence_bundle(request);
        return;
    }
    let bounded_excerpt = bounded_output_excerpt_for_block(
        &request.command_block,
        OutputExcerptDirection::Head,
        120,
        12 * 1024,
    );
    let evidence_status = bounded_excerpt.evidence_status();
    let truncation_status = bounded_excerpt.truncation_status();
    let redaction_status = bounded_excerpt.redaction_status;
    let excerpt = bounded_excerpt.text.unwrap_or_default();
    let scope = crate::insight::scope::resolve_execution_scope(
        &request.command_block.session_id,
        &request.command_block.command,
    );
    let related_facts = state.insight_correlation.recent_memory_pressure_facts(
        &scope,
        request.command_block.ended_at_ms,
        &request.command_block.id,
    );
    let metadata =
        take_bound_insight_metadata(&mut request.context_hints, "Candidate", "High", Vec::new());
    let target_facts = provider_target_facts(
        &request.command_block,
        &format!("{scope:?}"),
        &format!("{:?}", request.command_block.origin),
        evidence_status,
        redaction_status,
        truncation_status,
        &metadata,
    );
    trim_optional_context_hints(&mut request.context_hints);
    let other_context_bytes = request
        .context_hints
        .iter()
        .map(|hint| hint.len() + 1)
        .sum();
    request.context_hints.push(build_provider_evidence_payload(
        EvidenceBundleInput {
            target_facts,
            target_excerpt: excerpt,
            related_facts,
        },
        other_context_bytes,
    ));
}

fn clear_dismissed_prompt_ghost_context(event: &ShellEvent, state: &mut InlineState) {
    if event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some("prompt_ghost")
        && event.message.as_deref() == Some("dismissed")
        && !matches!(
            state.pending_input_ghost_binding,
            Some(PendingInputGhostBinding::Personal(_))
        )
    {
        state.pending_input_ghost_binding = None;
    }
}

fn handle_personal_prompt_feedback(event: &ShellEvent, state: &mut InlineState) {
    if event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some("prompt_ghost")
        && event.message.as_deref() == Some("dismissed")
    {
        ignore_pending_personal_suggestions(event, state, None);
        state.pending_prompt_suggestion_bindings.clear();
        state.pending_input_ghost_binding = None;
        return;
    }
    if let Some(Some(candidate_id)) = prompt_ghost_suggestion_id(event) {
        let selected = state
            .pending_prompt_suggestion_bindings
            .get(candidate_id)
            .and_then(|binding| match binding {
                PendingInputGhostBinding::Personal(binding) => Some(binding.clone()),
                _ => None,
            });
        match selected {
            Some(binding) => {
                ignore_pending_personal_suggestions(event, state, Some(candidate_id));
                state
                    .pending_prompt_suggestion_bindings
                    .remove(candidate_id);
                if let Some(writer) = state.personalization.writer.as_mut() {
                    writer.arm_frozen_prompt(binding.clone());
                }
                state.pending_input_ghost_binding =
                    Some(PendingInputGhostBinding::Personal(binding));
            }
            None if state
                .pending_prompt_suggestion_bindings
                .contains_key(candidate_id) =>
            {
                ignore_pending_personal_suggestions(event, state, None);
            }
            None => {}
        }
    }
    let Some(PendingInputGhostBinding::Personal(binding)) =
        state.pending_input_ghost_binding.as_ref().cloned()
    else {
        return;
    };
    let Some(action) = personal_prompt_action(event, &binding.candidate_id) else {
        return;
    };
    let terminal = !matches!(action, PersonalPromptAction::Accepted);
    let final_text = match action {
        PersonalPromptAction::Submitted(text) => Some(text),
        _ => None,
    };
    let feedback = {
        let Some(writer) = state.personalization.writer.as_mut() else {
            return;
        };
        match action {
            PersonalPromptAction::Accepted => writer.accept_frozen_prompt(),
            PersonalPromptAction::Dismissed => writer
                .dismiss_frozen_prompt()
                .or_else(|| writer.ignore_frozen_prompt()),
            PersonalPromptAction::Submitted(text) => writer.submit_frozen_prompt(text),
        }
    };
    if terminal {
        state.pending_input_ghost_binding = None;
    }
    if final_text.is_some() {
        state
            .personalization
            .signals
            .set_pending_intent_lifecycle(binding.intent_lifecycle_id.clone());
    }
    let Some(feedback) = feedback else {
        return;
    };
    record_personal_feedback(state, event, feedback);
}

pub(crate) fn finalize_personal_prompt_feedback_on_exit(
    event: &ShellEvent,
    state: &mut InlineState,
) {
    finalize_unresolved_personal_prompt_feedback(event, state);
    state.pending_prompt_suggestion_bindings.clear();
    state.pending_input_ghost_binding = None;
}

pub(crate) fn finalize_unresolved_personal_prompt_feedback(
    event: &ShellEvent,
    state: &mut InlineState,
) {
    ignore_pending_personal_suggestions(event, state, None);
}

fn ignore_pending_personal_suggestions(
    event: &ShellEvent,
    state: &mut InlineState,
    selected_id: Option<&str>,
) {
    let mut ignored = state
        .pending_prompt_suggestion_bindings
        .iter()
        .filter_map(|(candidate_id, binding)| match binding {
            PendingInputGhostBinding::Personal(binding)
                if selected_id != Some(candidate_id.as_str()) =>
            {
                Some(binding.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if selected_id.is_none() {
        if let Some(PendingInputGhostBinding::Personal(binding)) =
            state.pending_input_ghost_binding.as_ref()
        {
            if !ignored
                .iter()
                .any(|candidate| candidate.candidate_id == binding.candidate_id)
            {
                ignored.push(binding.clone());
            }
        }
    }
    state
        .pending_prompt_suggestion_bindings
        .retain(|candidate_id, binding| {
            !matches!(binding, PendingInputGhostBinding::Personal(_))
                || selected_id == Some(candidate_id.as_str())
        });
    for binding in ignored {
        let mut lifecycle =
            crate::recommendation::personal_feedback::FeedbackLifecycle::new(binding);
        if let Some(feedback) = lifecycle.ignore() {
            record_personal_feedback(state, event, feedback);
        }
    }
    if selected_id.is_none() {
        if let Some(writer) = state.personalization.writer.as_mut() {
            writer.clear_frozen_prompt();
        }
    }
}

fn personal_prompt_action<'a>(
    event: &'a ShellEvent,
    candidate_id: &str,
) -> Option<PersonalPromptAction<'a>> {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return None;
    }
    if event.component.as_deref() == Some("prompt_ghost")
        || event
            .component
            .as_deref()
            .and_then(|component| component.strip_prefix("prompt_ghost:"))
            == Some(candidate_id)
    {
        match event.message.as_deref() {
            Some("accepted") => return Some(PersonalPromptAction::Accepted),
            Some("dismissed") => return Some(PersonalPromptAction::Dismissed),
            _ => {}
        }
    }
    let submitted_id = event.component.as_deref()?.strip_prefix("prompt_ghost:")?;
    if submitted_id != candidate_id {
        return None;
    }
    event.input.as_deref().map(PersonalPromptAction::Submitted)
}

fn record_personal_feedback(
    state: &mut InlineState,
    event: &ShellEvent,
    feedback: crate::recommendation::personal_feedback::FeedbackEvent,
) {
    let Some(writer) = state.personalization.writer.as_mut() else {
        return;
    };
    let cwd = event
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let Some(context) = cwd.as_deref().and_then(|cwd| activity_context(writer, cwd)) else {
        return;
    };
    let event_time_ms = event
        .started_at_ms
        .or(event.ended_at_ms)
        .unwrap_or_default();
    let observed_hour_bucket = event_time_ms / 3_600_000;
    let identity = format!(
        "{}\0{:?}\0{}",
        feedback.candidate_id, feedback.action, event_time_ms
    );
    let Ok(Some(record)) =
        writer.feedback_record(feedback, observed_hour_bucket, context, identity.as_bytes())
    else {
        return;
    };
    let _ = writer.try_enqueue_identified_deferred(record);
}

fn bind_pending_input_ghost_context(
    request: &mut AgentRequest,
    state: &mut InlineState,
    event: &ShellEvent,
) {
    if crate::types::request_context_binding(request) != AgentContextBinding::FreeForm {
        return;
    }
    if prompt_ghost_suggestion_id(event).is_none() {
        return;
    }
    let binding = prompt_ghost_suggestion_id(event)
        .flatten()
        .and_then(|id| state.pending_prompt_suggestion_bindings.remove(id))
        .and_then(|binding| match binding {
            PendingInputGhostBinding::Health(binding) => Some(binding),
            _ => None,
        })
        .or_else(|| match state.pending_input_ghost_binding.take() {
            Some(PendingInputGhostBinding::Health(binding)) => Some(binding),
            _ => None,
        });
    if let Some(binding) = binding {
        crate::types::set_request_context_binding(request, binding);
    }
}

fn agent_request_from_pending_insight(
    event: &ShellEvent,
    blocks: &[CommandBlock],
    state: &mut InlineState,
) -> Option<AgentRequest> {
    let submitted_suggestion_id = prompt_ghost_suggestion_id(event)?;
    if !matches!(
        state.pending_input_ghost_binding.as_ref(),
        Some(PendingInputGhostBinding::Insight(_))
    ) {
        return None;
    }
    let PendingInputGhostBinding::Insight(binding) = state.pending_input_ghost_binding.take()?
    else {
        unreachable!("binding kind checked before take");
    };
    if submitted_suggestion_id != Some(binding.suggestion_id.as_str()) {
        return None;
    }
    let findings = findings_from_blocks(blocks);
    agent_request_from_insight_binding(
        &binding,
        &event.session_id,
        event.input.as_deref().unwrap_or_default(),
        blocks,
        &findings,
    )
}

fn is_standalone_agent_intercept(event: &ShellEvent) -> bool {
    event.kind == ShellEventKind::UserInputIntercepted
        && (matches!(
            event.component.as_deref(),
            Some("natural_language") | Some("path_prompt") | Some("agent_marker")
        ) || prompt_ghost_suggestion_id(event).is_some())
}

fn prompt_ghost_suggestion_id(event: &ShellEvent) -> Option<Option<&str>> {
    let component = event.component.as_deref()?;
    if component == "prompt_ghost" {
        return Some(None);
    }
    component
        .strip_prefix("prompt_ghost:")
        .filter(|id| !id.is_empty())
        .map(Some)
}

#[cfg(test)]
#[path = "intercept_tests.rs"]
mod tests;
