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

#[test]
fn persistent_display_physical_failures_require_an_emitted_token() {
    crate::shell_host::physical_failures_require_an_emitted_token_and_never_mask_other_errors();
}
#[test]
fn persistent_display_queued_overlay_failure_preserves_hold_and_eof() {
    crate::shell_host::queued_overlay_failure_stays_hold_and_preserves_eof();
}
#[test]
fn persistent_display_zero_write_is_not_a_storage_failure() {
    crate::shell_host::zero_write_is_physical_failure_without_masking_storage_write_zero();
    assert_failed_observer_audits_each_absolute_event_once();
    assert_gate_wrapping_preserves_only_physical_write_source();
}

fn assert_failed_observer_audits_each_absolute_event_once() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut state = InlineState {
        audit: Some(crate::journal::audit::ShellAuditRecorder::test_with_root(
            root.path(),
        )),
        ..InlineState::default()
    };
    assert!(
        !state.audit.as_ref().unwrap().is_degraded(),
        "actual private audit segment must be writable"
    );
    let reset = state.approval_slice.capture_representation_reset.clone();
    reset.bind_display_handoff("already-emitted");
    reset
        .defer_physical_error(
            crate::shell_host::PhysicalDisplayWriter(&mut AuditFailedOutput)
                .flush()
                .unwrap_err(),
        )
        .unwrap();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let events = [
        ShellEvent::command_started("s", "first", "echo first", "/tmp", 1),
        ShellEvent::command_started("s", "second", "echo second", "/tmp", 2),
    ];
    for (base, window) in [(100, &events[..1]), (100, &events[..]), (101, &events[1..])] {
        for _ in 0..2 {
            let action = render_raw_inline_event_view(
                crate::shell_host::ShellEventView::new(base, window),
                &mut Vec::new(),
                &adapter,
                "bash",
                &mut state,
            )
            .unwrap();
            assert_eq!(action, RawObserverAction::HoldShellOutput);
            assert_eq!(
                state.control.event_cursor().position(),
                0,
                "audit cannot acknowledge unfinished dispatch"
            );
        }
    }
    let mut text = String::new();
    for date in std::fs::read_dir(root.path().join("v1/segments")).unwrap() {
        for file in std::fs::read_dir(date.unwrap().path()).unwrap() {
            text.push_str(&std::fs::read_to_string(file.unwrap().path()).unwrap());
        }
    }
    let starts = text
        .lines()
        .filter(|line| {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            record["event_type"] == "shell.command.started"
        })
        .count();
    assert_eq!(
        starts, 2,
        "each native command start must have one audit record across repeat/compacted views"
    );
}

struct AuditFailedOutput;
impl Write for AuditFailedOutput {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from_raw_os_error(5))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(5))
    }
}

fn assert_gate_wrapping_preserves_only_physical_write_source() {
    use crate::approval::surface_session::SurfaceError;
    use crate::runtime::approval_gate::GateError;
    let physical = crate::shell_host::PhysicalDisplayWriter(&mut AuditFailedOutput)
        .flush()
        .unwrap_err();
    let wrapped = std::io::Error::other(SurfaceError::Gate(GateError::Write {
        accepted: 0,
        source: physical,
    }));
    assert!(crate::shell_host::physical_error(&wrapped));
    let winsize = std::io::Error::other(SurfaceError::Gate(GateError::Winsize(
        std::io::Error::from_raw_os_error(5),
    )));
    assert!(
        !crate::shell_host::physical_error(&winsize),
        "ioctl EIO is not a tagged display write"
    );
    let geometry = std::io::Error::other(SurfaceError::GeometryInvalidated);
    assert!(!crate::shell_host::physical_error(&geometry));
    let storage = std::io::Error::other(GateError::Write {
        accepted: 0,
        source: std::io::Error::from_raw_os_error(5),
    });
    assert!(
        !crate::shell_host::physical_error(&storage),
        "an untagged writer with the same OS code is strict"
    );
}
