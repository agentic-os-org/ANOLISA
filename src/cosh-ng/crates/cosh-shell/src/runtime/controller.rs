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
    let snapshot = ShellEventSnapshot::with_base(view.base(), view.events());
    if let Some(audit) = inline_state.audit.as_mut() {
        let batch = snapshot.batch_since(inline_state.control.event_cursor());
        audit.observe_shell_event_batch(batch.events);
    }
    let mut terminal_output = CrLfWriter::new(output);
    if inline_state.questions.active_panel_id.is_some() {
        redraw_active_question_if_width_changed(
            inline_state,
            &mut terminal_output,
            RatatuiInlineRenderer::for_terminal().with_language(inline_state.language),
        )?;
    }
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &snapshot,
        adapter,
        shell_label,
        inline_state,
        &mut terminal_output,
    )?;
    RuntimeDispatcher::apply_actions(actions, inline_state);
    if let Some(request) = inline_state
        .control
        .shell_handoff_mut()
        .emit_next_approved(snapshot.cursor().position())
    {
        if inline_state.trigger_pty_prompt {
            inline_state.trigger_pty_prompt = false;
            inline_state.pending_input_ghost = None;
            inline_state.pending_input_ghost_route = Default::default();
            inline_state.pending_input_ghost_binding = None;
            return Ok(RawObserverAction::EmitToPtyWithPromptRestore(request));
        }
        return Ok(RawObserverAction::EmitToPty(request));
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

#[cfg(test)]
fn render_raw_inline_events<W: Write>(
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
mod tests {
    use super::*;
    use crate::agent::run::ActiveAgentRun;
    use std::time::Instant;

    #[test]
    fn active_foreground_command_keeps_raw_passthrough_even_when_agent_running() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        state.agent_run.active = Some(test_active_run());
        let events = vec![ShellEvent::command_started(
            "session-1",
            "cmd-1",
            "sudo df -h",
            "/tmp",
            10,
        )];
        let mut output = Vec::new();

        let action = render_raw_inline_events(&events, &mut output, &adapter, "zsh", &mut state)
            .expect("render raw inline events");

        assert_eq!(action, RawObserverAction::RawPassthrough);
    }

    #[test]
    fn completed_agent_run_restores_prompt_through_the_pty_owner() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        state.agent_run.needs_prompt_after_run = true;
        let mut output = Vec::new();

        let action = render_raw_inline_events(&[], &mut output, &adapter, "bash", &mut state)
            .expect("restore prompt after agent run");

        assert_eq!(
            action,
            RawObserverAction::RestorePrompt {
                ghost_text: None,
                ghost_route: Default::default(),
            }
        );
        assert!(
            output.is_empty(),
            "runtime must not write a bare PS1 outside PromptPresentation"
        );
        assert!(!state.agent_run.needs_prompt_after_run);
    }

    #[test]
    fn pending_shell_handoff_keeps_raw_passthrough_before_preexec() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        state.agent_run.active = Some(test_active_run());
        let request = ShellHandoffRequest::new(
            "echo approved",
            "$ echo approved",
            "approved_provider_shell_tool",
            "user",
            "req-approved",
            "run-approved",
            1,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request.clone());
        let mut first_output = Vec::new();

        let first_action =
            render_raw_inline_events(&[], &mut first_output, &adapter, "zsh", &mut state)
                .expect("emit handoff");

        assert_eq!(first_action, RawObserverAction::EmitToPty(request));

        let mut second_output = Vec::new();
        let second_action =
            render_raw_inline_events(&[], &mut second_output, &adapter, "zsh", &mut state)
                .expect("keep handoff foreground protected");

        assert_eq!(second_action, RawObserverAction::RawPassthrough);
    }

    #[test]
    fn pending_shell_handoff_serializes_approved_requests() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        state.agent_run.active = Some(test_active_run());
        let first = ShellHandoffRequest::new(
            "echo first",
            "$ echo first",
            "approved_provider_shell_tool",
            "user",
            "req-first",
            "run-approved",
            1,
        )
        .expect("first handoff request");
        let second = ShellHandoffRequest::new(
            "echo second",
            "$ echo second",
            "approved_provider_shell_tool",
            "user",
            "req-second",
            "run-approved",
            1,
        )
        .expect("second handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(first.clone());
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(second.clone());

        let first_action =
            render_raw_inline_events(&[], &mut Vec::new(), &adapter, "zsh", &mut state)
                .expect("emit first handoff");
        assert_eq!(first_action, RawObserverAction::EmitToPty(first));

        let second_action =
            render_raw_inline_events(&[], &mut Vec::new(), &adapter, "zsh", &mut state)
                .expect("hold second handoff until the first closes");

        assert_eq!(second_action, RawObserverAction::RawPassthrough);

        state
            .control
            .shell_handoff_mut()
            .pop_pending()
            .expect("close first handoff");
        let third_action =
            render_raw_inline_events(&[], &mut Vec::new(), &adapter, "zsh", &mut state)
                .expect("emit second handoff after the first closes");

        assert_eq!(third_action, RawObserverAction::EmitToPty(second));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn pending_shell_handoff_restores_prompt_with_first_emit() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        state.trigger_pty_prompt = true;
        let request = ShellHandoffRequest::new(
            "echo approved",
            "$ echo approved",
            "approved_provider_shell_tool",
            "user",
            "req-approved",
            "run-approved",
            1,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request.clone());
        let mut output = Vec::new();

        let action = render_raw_inline_events(&[], &mut output, &adapter, "zsh", &mut state)
            .expect("emit handoff with prompt restore");

        assert_eq!(
            action,
            RawObserverAction::EmitToPtyWithPromptRestore(request)
        );
    }

    #[test]
    fn pending_shell_handoff_timeout_interrupts_before_preexec_without_notice() {
        let mut state = InlineState::default();
        let request = ShellHandoffRequest::new(
            "sleep 10",
            "$ sleep 10",
            "approved_provider_shell_tool",
            "user",
            "req-timeout-before-preexec",
            "run-timeout-before-preexec",
            1,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request);
        state
            .control
            .shell_handoff_mut()
            .emit_next_approved(0)
            .expect("emit handoff");
        state
            .control
            .shell_handoff_mut()
            .backdate_pending_emit_for_test(Duration::from_secs(2));
        let mut output = Vec::new();

        let action = shell_handoff_timeout_recovery_action_with_timeout(
            &mut state,
            false,
            &mut output,
            Some(Duration::from_secs(1)),
        )
        .expect("timeout action");

        assert_eq!(action, Some(RawObserverAction::InterruptForeground));
        assert!(output.is_empty(), "{}", String::from_utf8_lossy(&output));
    }

    #[test]
    fn shell_handoff_timeout_notice_is_deferred_until_foreground_is_idle() {
        let mut state = InlineState::default();
        let request = ShellHandoffRequest::new(
            "sleep 10",
            "$ sleep 10",
            "approved_provider_shell_tool",
            "user",
            "req-timeout",
            "run-timeout",
            1,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request);
        state
            .control
            .shell_handoff_mut()
            .emit_next_approved(0)
            .expect("emit handoff");
        state
            .control
            .shell_handoff_mut()
            .backdate_pending_emit_for_test(Duration::from_secs(2));
        let mut busy_output = Vec::new();

        let action = shell_handoff_timeout_recovery_action_with_timeout(
            &mut state,
            true,
            &mut busy_output,
            Some(Duration::from_secs(1)),
        )
        .expect("timeout action");

        assert_eq!(action, Some(RawObserverAction::InterruptForeground));
        assert!(
            busy_output.is_empty(),
            "{}",
            String::from_utf8_lossy(&busy_output)
        );

        state
            .control
            .shell_handoff_mut()
            .pop_pending()
            .expect("handoff finished");
        let mut idle_output = Vec::new();
        let action = shell_handoff_timeout_recovery_action_with_timeout(
            &mut state,
            false,
            &mut idle_output,
            Some(Duration::from_secs(1)),
        )
        .expect("timeout notice");
        let idle_text = String::from_utf8_lossy(&idle_output);

        assert_eq!(action, None);
        assert!(
            idle_text.contains("Command exceeded configured shell handoff timeout (1s)."),
            "{idle_text}"
        );
        assert!(
            idle_text.contains("Sent interrupt to foreground PTY; waiting for shell evidence."),
            "{idle_text}"
        );
    }

    fn test_active_run() -> ActiveAgentRun {
        let request = test_agent_request("active");
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

    fn test_agent_request(id: &str) -> AgentRequest {
        AgentRequest {
            id: id.to_string(),
            session_id: "session-1".to_string(),
            command_block: CommandBlock {
                id: "agent-cmd-1".to_string(),
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
}
