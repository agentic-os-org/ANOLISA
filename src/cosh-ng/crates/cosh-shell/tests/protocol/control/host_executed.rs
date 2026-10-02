use super::*;

#[test]
fn cosh_core_host_executed_shell_result_uses_control_response() {
    let adapter = make_cosh_core_adapter("mock_cosh_core_host_executed_cli.sh");
    let session_state = Arc::clone(&adapter.session);
    let request = make_request("cosh-core-test-host-executed");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    let tool_req = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. }))
        .expect("expected cosh-core ToolPermissionRequest");

    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_use_id,
        ..
    } = tool_req
    {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: None,
                decision: ApprovalDecision::HostExecutedShell {
                    result: Box::new(HostExecutedShellResult {
                        llm_content:
                            "ShellCommandCompleted evidence\ncommand: df -h\nstatus: completed"
                                .to_string(),
                        return_display: None,
                        metadata: HostExecutedShellMetadata {
                            command: "df -h".to_string(),
                            status: "completed".to_string(),
                            exit_code: 0,
                            signal: None,
                            cwd: "/tmp".to_string(),
                            end_cwd: "/tmp".to_string(),
                            duration_ms: 12,
                            output_ref: Some("terminal-output://test/cmd-1".to_string()),
                            redaction_status: "bounded".to_string(),
                            approval_id: Some("approval-1".to_string()),
                            tool_use_id: Some(tool_use_id.clone()),
                            input_wait: None,
                        },
                    }),
                },
            })
            .expect("cosh-core host-executed approval response should succeed");
    }

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected cosh-core host-executed completion, got: {remaining:?}"
    );
    assert!(
        wait_for_cosh_core_session(
            &session_state,
            "mock-cosh-core-host-executed",
            Duration::from_secs(1)
        ),
        "session_id should be committed after cosh-core host-executed completion"
    );
    let capabilities = handle.control_capabilities();
    assert!(capabilities.provider_initialize_seen);
    assert!(capabilities.can_handle_can_use_tool);
    assert!(capabilities.can_handle_host_executed_shell_tool_result);
}

#[test]
fn cosh_core_multi_host_executed_shell_results_stay_in_same_control_turn() {
    let adapter = make_cosh_core_adapter("mock_cosh_core_host_executed_multi_cli.sh");
    let session_state = Arc::clone(&adapter.session);
    let request = make_request("cosh-core-test-host-executed-multi");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let first = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    respond_host_executed(&handle, &first, "df -h", "completed");

    let second = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    assert!(
        second.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text, .. } if text.contains("First host executed result received.")
        )),
        "expected provider to continue same turn after first host result, got: {second:?}"
    );
    respond_host_executed(&handle, &second, "du -sh .", "completed");

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    assert!(
        remaining.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text, .. } if text.contains("Second host executed result received.")
        )),
        "expected provider to continue after second host result, got: {remaining:?}"
    );
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected cosh-core multi host-executed completion, got: {remaining:?}"
    );
    assert!(
        wait_for_cosh_core_session(
            &session_state,
            "mock-cosh-core-host-executed-multi",
            Duration::from_secs(1)
        ),
        "session_id should be committed after multi host-executed completion"
    );
}

#[test]
fn cosh_core_analysis_continuation_denies_reentrant_shell_request() {
    let adapter = make_cosh_core_adapter("mock_cosh_core_analysis_continuation_shell_request.sh");
    let session_state = Arc::clone(&adapter.session);
    let mut request = make_request("cosh-core-analysis-continuation-deny");
    request.user_input =
        Some("ShellCommandCompleted evidence\ncommand: df -h\nstatus: completed".to_string());
    request
        .context_hints
        .push("analysis-only continuation after foreground shell handoff".to_string());
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(
            event,
            AgentEvent::AgentCompleted { .. } | AgentEvent::AgentFailed { .. }
        )
    });
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text, .. }
                if text.contains("Cosh-core analysis continuation shell request was denied.")
        )),
        "expected cosh-core provider to receive deny response, got: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected cosh-core analysis continuation completion, got: {events:?}"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AgentEvent::ToolPermissionRequest { .. })),
        "analysis-only shell request should be denied inside adapter, got: {events:?}"
    );
    assert!(
        wait_for_cosh_core_session(
            &session_state,
            "mock-cosh-core-analysis-continuation",
            Duration::from_secs(1)
        ),
        "session_id should be committed after cosh-core analysis continuation completion"
    );
}

fn respond_host_executed(
    handle: &cosh_shell::adapter::AgentRunHandle,
    events: &[AgentEvent],
    command: &str,
    status: &str,
) {
    let tool_req = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. }))
        .expect("expected ToolPermissionRequest");

    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_use_id,
        ..
    } = tool_req
    {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: None,
                decision: ApprovalDecision::HostExecutedShell {
                    result: Box::new(HostExecutedShellResult {
                        llm_content: format!(
                            "ShellCommandCompleted evidence\ncommand: {command}\nstatus: {status}"
                        ),
                        return_display: None,
                        metadata: HostExecutedShellMetadata {
                            command: command.to_string(),
                            status: status.to_string(),
                            exit_code: 0,
                            signal: None,
                            cwd: "/tmp".to_string(),
                            end_cwd: "/tmp".to_string(),
                            duration_ms: 12,
                            output_ref: Some("terminal-output://test/cmd-1".to_string()),
                            redaction_status: "bounded".to_string(),
                            approval_id: Some("approval-1".to_string()),
                            tool_use_id: Some(tool_use_id.clone()),
                            input_wait: None,
                        },
                    }),
                },
            })
            .expect("host-executed approval response should succeed");
    }
}

#[test]
fn qwen_control_protocol_records_initialize_capabilities() {
    let adapter = make_qwen_adapter("mock_qwen_control_capabilities.sh");
    let request = make_request("qwen-test-capabilities");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected qwen control completion, got: {events:?}"
    );
    let capabilities = handle.control_capabilities();
    assert!(capabilities.provider_initialize_seen);
    assert!(capabilities.can_handle_can_use_tool);
    assert!(capabilities.can_handle_host_executed_shell_tool_result);
}

#[test]
fn respond_approval_fails_without_sender() {
    let fake = FakeAgentAdapter;
    let request = make_request("test-fake");
    let handle = AdapterInstance::Fake(fake).start_cancellable(request, CoshApprovalMode::Auto);

    let result = handle.respond_approval(ApprovalResponse {
        request_id: "test".to_string(),
        tool_use_id: Some("toolu_test".to_string()),
        tool_input: None,
        decision: ApprovalDecision::Allow,
    });
    assert!(
        result.is_err(),
        "respond_approval should fail for FakeAdapter"
    );

    let fake = FakeAgentAdapter;
    assert!(!fake.capabilities().control_protocol);
}
