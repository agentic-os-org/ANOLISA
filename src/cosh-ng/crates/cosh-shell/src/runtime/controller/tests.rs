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

    let first_action = render_raw_inline_events(&[], &mut Vec::new(), &adapter, "zsh", &mut state)
        .expect("emit first handoff");
    assert_eq!(first_action, RawObserverAction::EmitToPty(first));

    let second_action = render_raw_inline_events(&[], &mut Vec::new(), &adapter, "zsh", &mut state)
        .expect("hold second handoff until the first closes");

    assert_eq!(second_action, RawObserverAction::RawPassthrough);

    state
        .control
        .shell_handoff_mut()
        .pop_pending()
        .expect("close first handoff");
    let third_action = render_raw_inline_events(&[], &mut Vec::new(), &adapter, "zsh", &mut state)
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

    assert_eq!(
        action,
        Some(RawObserverAction::RestorePrompt {
            ghost_text: None,
            ghost_route: Default::default()
        })
    );
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

#[test]
fn deferred_handoff_timeout_notice_restores_native_prompt_without_input() {
    let mut state = InlineState {
        pending_shell_handoff_timeout_notice: Some(Duration::from_secs(2)),
        ..InlineState::default()
    };
    let mut output = Vec::new();
    let action = shell_handoff_timeout_recovery_action_with_timeout(
        &mut state,
        false,
        &mut output,
        Some(Duration::from_secs(2)),
    )
    .expect("idle timeout notice");
    assert_eq!(
        action,
        Some(RawObserverAction::RestorePrompt {
            ghost_text: None,
            ghost_route: Default::default(),
        })
    );
    assert!(String::from_utf8_lossy(&output).contains("handoff timeout (2s)."));
    assert!(state.pending_shell_handoff_timeout_notice.is_none());
    output.clear();
    assert_eq!(
        shell_handoff_timeout_recovery_action_with_timeout(
            &mut state,
            false,
            &mut output,
            Some(Duration::from_secs(2)),
        )
        .expect("settled timeout notice"),
        None
    );
    assert!(output.is_empty());
}

#[test]
fn terminal_capture_invalidation_precedes_unrelated_approved_handoff_emission() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    state
        .approvals
        .pending_capture_invalidations
        .push_back("dead-card".to_string());
    let request = ShellHandoffRequest::new(
        "echo approved",
        "$ echo approved",
        "approved_provider_shell_tool",
        "user",
        "req-approved",
        "run-approved",
        1,
    )
    .unwrap();
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(request.clone());
    let first =
        render_raw_inline_events(&[], &mut Vec::new(), &adapter, "bash", &mut state).unwrap();
    assert_eq!(
        first,
        RawObserverAction::InvalidateApprovalCapture("dead-card".to_string())
    );
    assert!(state.control.shell_handoff().pending_front().is_none());
    let second =
        render_raw_inline_events(&[], &mut Vec::new(), &adapter, "bash", &mut state).unwrap();
    assert_eq!(second, RawObserverAction::EmitToPty(request));
}
