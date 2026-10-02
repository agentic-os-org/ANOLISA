use super::*;

#[test]
fn qwen_recommend_uses_stream_prompt_with_closed_stdin() {
    let adapter = make_qwen_adapter("mock_qwen_stream_cli.sh");
    let session_state = Arc::clone(&adapter.session_id);
    let request = make_request("qwen-test-stream");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Recommend);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(
            event,
            AgentEvent::AgentCompleted { .. } | AgentEvent::AgentFailed { .. }
        )
    });

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text, .. } if text.contains("qwen stream completed")
        ) || matches!(
            event,
            AgentEvent::AgentCompleted { summary, .. } if summary.contains("analysis completed")
        )),
        "expected qwen stream completion, got: {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::AgentFailed { error, .. } if error.contains("stdin was not closed")
        )),
        "qwen stream should not inherit raw shell stdin, got: {events:?}"
    );
    assert!(
        wait_for_session_id(&session_state, "mock-qwen-stream", Duration::from_secs(1)),
        "session_id should be committed after qwen stream completion"
    );
    assert!(
        handle
            .respond_approval(ApprovalResponse {
                request_id: "unused".to_string(),
                tool_use_id: None,
                tool_input: None,
                decision: ApprovalDecision::Deny {
                    message: "unused".to_string(),
                },
            })
            .is_err(),
        "qwen stream mode should not expose a control approval channel"
    );
}

#[test]
fn qwen_auto_uses_control_approval_channel() {
    let adapter = make_qwen_adapter("mock_qwen_control_cli.sh");
    let session_state = Arc::clone(&adapter.session_id);
    let request = make_request("qwen-test-auto-stream");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    let tool_req = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. }));
    assert!(tool_req.is_some(), "expected qwen ToolPermissionRequest");
    assert_eq!(
        session_state.lock().unwrap().as_deref(),
        None,
        "session id must not be committed while provider is stopped at a permission request"
    );
    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_use_id,
        ..
    } = tool_req.unwrap()
    {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: None,
                decision: ApprovalDecision::Allow,
            })
            .expect("qwen approval response should succeed");
    }

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected qwen control completion, got: {remaining:?}"
    );
    assert!(
        wait_for_session_id(&session_state, "mock-qwen-control", Duration::from_secs(1)),
        "session_id should be committed after qwen control completion"
    );
}

#[test]
fn cosh_core_auto_uses_control_approval_channel() {
    let adapter = make_cosh_core_adapter("mock_qwen_control_cli.sh");
    let session_state = Arc::clone(&adapter.session);
    let request = make_request("cosh-core-test-auto-stream");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    let tool_req = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. }));
    assert!(
        tool_req.is_some(),
        "expected cosh-core ToolPermissionRequest"
    );

    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_use_id,
        ..
    } = tool_req.unwrap()
    {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: None,
                decision: ApprovalDecision::Allow,
            })
            .expect("cosh-core approval response should succeed");
    }

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected cosh-core control completion, got: {remaining:?}"
    );
    assert!(
        wait_for_cosh_core_session(&session_state, "mock-qwen-control", Duration::from_secs(1)),
        "session_id should be committed after cosh-core control completion"
    );
}
