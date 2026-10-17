use std::io::Write;
use std::time::Duration;

use crate::question::runtime::pending_question_capture;
use crate::runtime::approval_state::ApprovalRequestStatus;
use crate::runtime::prelude::*;
use crate::runtime::question_terminal::redraw_active_question_if_width_changed;
use crate::runtime::state::InlineState;
use crate::shell_host::ShellEventView;

use super::dispatcher::RuntimeDispatcher;
use super::events::ShellEventSnapshot;
use super::terminal::CrLfWriter;

mod bootstrap;
mod failopen;
mod input_wait;

use input_wait::input_wait_timeout_recovery_action;

pub(crate) use bootstrap::{
    run_adapter_demo, run_demo, run_host_demo, run_interactive, run_interactive_demo, run_raw,
};

fn render_raw_inline_event_view<W: Write>(
    view: ShellEventView<'_>,
    output: &mut W,
    adapter: &AdapterInstance,
    shell_label: &str,
    inline_state: &mut InlineState,
) -> std::io::Result<RawObserverAction> {
    inline_state
        .approval_slice
        .capture_representation_reset
        .take_committed_emit();
    let snapshot = ShellEventSnapshot::with_base(view.base(), view.events());
    inline_state.approval_slice.observer_event_end = snapshot.cursor().position();
    inline_state.approval_slice.observed_focus = snapshot
        .batch_since(inline_state.control.event_cursor())
        .events
        .iter()
        .filter_map(|event| {
            crate::approval::panel::approval_focus_from_event(
                event,
                &inline_state.approvals.requests,
            )
            .or_else(|| {
                let command = approval_command_from_event(event)?;
                (event.component.as_deref() == Some("card")
                    && command.kind == ApprovalCommandKind::Details)
                    .then_some((command.id, ApprovalPanelAction::Details))
            })
        })
        .rfind(|(id, _)| crate::approval::panel::approval_is_pending(inline_state, id));
    if let Some(audit) = inline_state.audit.as_mut() {
        let batch = snapshot.batch_since(inline_state.approval_slice.audit_event_cursor);
        audit.observe_shell_event_batch(batch.events);
        inline_state.approval_slice.audit_event_cursor = batch.to;
    }
    let reset = inline_state
        .approval_slice
        .capture_representation_reset
        .clone();
    if let Some(token) = reset.display_handoff_token() {
        if inline_state
            .control
            .shell_handoff()
            .pending_front()
            .is_some_and(|handoff| handoff.request().token == token)
        {
            inline_state
                .control
                .observe_shell_command_activity(view.events());
            let ledger = build_command_blocks(view.events());
            crate::activity::runtime::record_approved_shell_handoff_blocks(
                inline_state,
                &ledger.blocks,
            );
            crate::activity::runtime::close_untracked_shell_handoffs_with_history_base(
                inline_state,
                view.events(),
                view.base(),
            );
        }
        if reset.display_prompt_restore_pending()
            && !reset.display_is_failed()
            && !inline_state
                .control
                .shell_handoff()
                .pending_front()
                .is_some_and(|handoff| handoff.request().token == token)
        {
            // Recovery consumes native prompt bytes before the provider's final output.
            inline_state.agent_run.needs_prompt_after_run = true;
            inline_state.agent_run.native_prompt_after_run = false;
            return Ok(RawObserverAction::RestorePrompt {
                ghost_text: None,
                ghost_route: Default::default(),
            });
        }
        if reset.display_is_failed() || reset.display_prompt_restore_pending() {
            crate::approval::slice_present::invalidate_representation(inline_state);
            if let Some(session) = inline_state.approval_slice.session.as_mut() {
                session.relinquish_untrusted();
            }
            inline_state.approval_slice.session = None;
            inline_state.approval_slice.disabled = true;
            let mut physical = crate::shell_host::PhysicalDisplayWriter(output);
            if inline_state.control.shell_busy()
                || inline_state
                    .control
                    .shell_handoff()
                    .pending_front()
                    .is_some()
            {
                if let Some(action) = shell_handoff_timeout_recovery_action(
                    inline_state,
                    inline_state.control.shell_busy(),
                    &mut physical,
                )? {
                    return Ok(action);
                }
            }
            return Ok(RawObserverAction::HoldShellOutput);
        }
    }
    let mut physical = crate::shell_host::PhysicalDisplayWriter(output);
    let mut terminal_output = CrLfWriter::new(&mut physical);
    // A recovered worker must not publish at coordinates invalidated by resize.
    crate::approval::slice_present::repaint_worker_slice_if_viewport_changed(
        inline_state,
        &mut terminal_output,
    )?;
    crate::approval::slice_present::pump_worker_slice(inline_state, &mut terminal_output)?;
    if inline_state.questions.active_panel_id.is_some() {
        redraw_active_question_if_width_changed(
            inline_state,
            &mut terminal_output,
            RatatuiInlineRenderer::for_terminal().with_language(inline_state.language),
        )?;
    }
    let dispatch_result = RuntimeDispatcher::dispatch_inline_batch(
        &snapshot,
        adapter,
        shell_label,
        inline_state,
        &mut terminal_output,
    );
    match dispatch_result {
        Ok(actions) => RuntimeDispatcher::apply_actions(actions, inline_state),
        Err(error) => {
            let committed = inline_state
                .control
                .shell_handoff()
                .ready_front()
                .is_some_and(|request| committed_slice_handoff(inline_state, request));
            if !committed || !crate::shell_host::physical_error(&error) {
                return Err(error);
            }
            reset.mark_committed_display_failure();
            tracing::warn!(error = ?error, "committed approval presentation failed");
            crate::approval::slice_present::invalidate_representation(inline_state);
            if let Some(session) = inline_state.approval_slice.session.as_mut() {
                session.relinquish_untrusted();
            }
            inline_state.approval_slice.session = None;
            inline_state.approval_slice.disabled = true;
        }
    }
    if let Some(request) = inline_state
        .control
        .shell_handoff_mut()
        .emit_next_approved(snapshot.cursor().position())
    {
        if committed_slice_handoff(inline_state, &request) {
            inline_state
                .approval_slice
                .capture_representation_reset
                .permit_committed_emit();
        }
        if inline_state.trigger_pty_prompt {
            inline_state.trigger_pty_prompt = false;
            inline_state.pending_input_ghost = None;
            inline_state.pending_input_ghost_route = Default::default();
            inline_state.pending_input_ghost_binding = None;
            return Ok(RawObserverAction::EmitToPtyWithPromptRestore(request));
        }
        return Ok(RawObserverAction::EmitToPty(request));
    }
    if reset.display_handoff_token().is_some()
        && inline_state
            .control
            .shell_handoff()
            .pending_front()
            .is_none()
    {
        reset.end_display_handoff();
    }
    if let Some(capture) = pending_card_capture(inline_state) {
        return Ok(RawObserverAction::CaptureInput(capture));
    }
    if inline_state.trigger_pty_prompt {
        inline_state.trigger_pty_prompt = false;
        return Ok(RawObserverAction::RestorePrompt {
            ghost_text: inline_state.pending_input_ghost.take(),
            ghost_route: std::mem::take(&mut inline_state.pending_input_ghost_route),
        });
    }
    let shell_busy = inline_state.control.shell_busy();
    if let Some(action) =
        shell_handoff_timeout_recovery_action(inline_state, shell_busy, &mut terminal_output)?
    {
        return Ok(action);
    }
    if let Some(action) =
        input_wait_timeout_recovery_action(inline_state, shell_busy, &mut terminal_output)?
    {
        return Ok(action);
    }
    let shell_handoff_pending = inline_state
        .control
        .shell_handoff()
        .pending_front()
        .is_some();
    if shell_busy || shell_handoff_pending {
        Ok(RawObserverAction::RawPassthrough)
    } else if inline_state
        .agent_run
        .active
        .as_ref()
        .is_some_and(|run| !run.completed)
    {
        Ok(RawObserverAction::DelayShellOutput)
    } else {
        Ok(RawObserverAction::Continue)
    }
}

fn committed_slice_handoff(state: &InlineState, request: &ShellHandoffRequest) -> bool {
    state.approval_slice.activation.is_some()
        && state.approvals.requests.iter().any(|approval| {
            approval.id == request.approval_id
                && approval.run_id == request.run_id
                && approval.status == ApprovalRequestStatus::Approved
        })
}

#[cfg(test)]
pub(crate) fn render_raw_inline_events<W: Write>(
    events: &[ShellEvent],
    output: &mut W,
    adapter: &AdapterInstance,
    shell_label: &str,
    inline_state: &mut InlineState,
) -> std::io::Result<RawObserverAction> {
    render_raw_inline_event_view(
        ShellEventView::new(0, events),
        output,
        adapter,
        shell_label,
        inline_state,
    )
}

fn shell_handoff_timeout_recovery_action<W: Write>(
    state: &mut InlineState,
    shell_busy: bool,
    output: &mut W,
) -> std::io::Result<Option<RawObserverAction>> {
    shell_handoff_timeout_recovery_action_with_timeout(
        state,
        shell_busy,
        output,
        configured_shell_handoff_timeout(),
    )
}

fn shell_handoff_timeout_recovery_action_with_timeout<W: Write>(
    state: &mut InlineState,
    shell_busy: bool,
    output: &mut W,
    timeout: Option<Duration>,
) -> std::io::Result<Option<RawObserverAction>> {
    let shell_handoff_pending = state.control.shell_handoff().pending_front().is_some();
    if !shell_busy && !shell_handoff_pending {
        if let Some(timeout) = state.pending_shell_handoff_timeout_notice.take() {
            render_shell_handoff_timeout_notice(state, output, timeout)?;
            return Ok(Some(RawObserverAction::RestorePrompt {
                ghost_text: None,
                ghost_route: Default::default(),
            }));
        }
        return Ok(None);
    }

    let Some(timeout) = timeout else {
        return Ok(None);
    };
    let marked_timeout = state
        .control
        .shell_handoff_mut()
        .mark_timeout_interrupt_if_elapsed(timeout);
    if !marked_timeout {
        return Ok(None);
    }
    state.pending_shell_handoff_timeout_notice = Some(timeout);
    Ok(Some(RawObserverAction::InterruptForeground))
}

fn render_shell_handoff_timeout_notice<W: Write>(
    state: &InlineState,
    output: &mut W,
    timeout: Duration,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let timeout_secs = timeout.as_secs().to_string();
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::ApprovalShellHandoffTimeoutTitle),
                body: vec![
                    i18n.format(
                        MessageId::ApprovalShellHandoffTimeoutExceededBody,
                        &[("seconds", &timeout_secs)],
                    ),
                    i18n.t(MessageId::ApprovalShellHandoffTimeoutInterruptBody)
                        .to_string(),
                ],
                footer: None,
            },
        )?;
    Ok(())
}

fn configured_shell_handoff_timeout() -> Option<Duration> {
    let secs = std::env::var("COSH_SHELL_HANDOFF_TIMEOUT_SECS")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    (secs > 0).then(|| Duration::from_secs(secs))
}

#[cfg(test)]
pub(crate) fn render_inline_guidance<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    shell_label: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let snapshot = ShellEventSnapshot::new(events);
    let previous_cursor = state.control.event_cursor();
    state.control.set_event_cursor(Default::default());
    let actions =
        RuntimeDispatcher::dispatch_inline_batch(&snapshot, adapter, shell_label, state, output)?;
    RuntimeDispatcher::apply_actions(actions, state);
    state.control.set_event_cursor(previous_cursor);
    Ok(())
}

pub(crate) fn pending_card_capture(state: &InlineState) -> Option<RawInputCapture> {
    // #1721 D13: an open draft card owns every keystroke until submit/cancel.
    if let Some(draft) = state.prompt_draft.as_ref() {
        return Some(draft.capture());
    }
    if let Some(capture) = crate::slash::pending_task_form_capture(state) {
        return Some(capture);
    }
    if let Some(capture) = crate::slash::pending_task_snapshot_capture(state) {
        return Some(capture);
    }
    if let Some(session_panel) = state.control.session().pending_panel() {
        return Some(RawInputCapture::Session {
            id: session_panel.id.clone(),
            option_count: session_panel.sessions.len(),
            selected: session_panel.selected_option,
            marked_for_clear: session_panel
                .sessions
                .iter()
                .map(|session| {
                    session_panel
                        .selected_for_clear
                        .contains(&session.session_id)
                })
                .collect(),
            confirming_clear: matches!(
                session_panel.phase,
                crate::slash::session::RuntimeSessionPanelPhase::ConfirmClear
            ),
        });
    }
    if let Some(mode_panel) = state.control.pending_mode_panel() {
        return Some(RawInputCapture::Mode {
            id: mode_panel.id.clone(),
            option_count: 3,
            selected: mode_panel.selected_option,
        });
    }
    if let Some(config_panel) = state.control.pending_config_panel() {
        return Some(RawInputCapture::Config {
            id: config_panel.id.clone(),
            option_count: 2,
            selected: config_panel.selected_option,
        });
    }
    if let Some(config_language_panel) = state.control.pending_config_language_panel() {
        return Some(RawInputCapture::ConfigLanguage {
            id: config_language_panel.id.clone(),
            option_count: 3,
            selected: config_language_panel.selected_option,
        });
    }

    if state.agent_run.active.is_none() {
        if let Some(consultation) = state.hooks.pending_consultation.as_ref() {
            return Some(RawInputCapture::Consultation {
                id: consultation.card_id.clone(),
            });
        }
    }

    // Hook-action disambiguation panel (#1629): when a hook id collides
    // between shell and agent layers, capture input for the question panel.
    if let Some(capture) = crate::slash::hooks::pending_hook_action_capture(state) {
        return Some(capture);
    }

    if let Some(capture) = pending_question_capture(state) {
        return Some(capture);
    }

    if let Some(capture) = crate::auth::runtime::pending_auth_capture(state) {
        return Some(capture);
    }

    if let Some(capture) = crate::runtime::evidence_requests::pending_evidence_capture(state) {
        return Some(capture);
    }

    state
        .approvals
        .requests
        .iter()
        .find(|request| request.status == ApprovalRequestStatus::Pending)
        .map(|request| RawInputCapture::Approval {
            id: request.id.clone(),
            action_set: crate::approval::panel::approval_action_set_for(
                request,
                &state.approvals.requests,
            ),
        })
}

pub(crate) fn shell_has_active_foreground_command(events: &[ShellEvent]) -> bool {
    let mut active = std::collections::HashSet::new();
    for event in events {
        let Some(command_id) = event.command_id.as_ref() else {
            continue;
        };

        match event.kind {
            ShellEventKind::CommandStarted => {
                active.insert(command_id.as_str());
            }
            ShellEventKind::CommandCompleted
            | ShellEventKind::CommandFailed
            | ShellEventKind::UserInputIntercepted => {
                active.remove(command_id.as_str());
            }
            _ => {}
        }
    }

    !active.is_empty()
}

#[cfg(test)]
mod hook_tests;

#[cfg(test)]
mod tests;
