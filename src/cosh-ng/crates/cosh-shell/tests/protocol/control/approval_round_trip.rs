use super::*;

#[test]
fn control_protocol_allow_round_trip() {
    let adapter = make_adapter("mock_control_cli.sh");
    let request = make_request("test-allow");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });

    let tool_req = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolPermissionRequest { .. }));
    assert!(
        tool_req.is_some(),
        "expected ToolPermissionRequest, got: {events:?}"
    );

    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_name,
        tool_input,
        tool_use_id,
        ..
    } = tool_req.unwrap()
    {
        assert_eq!(request_id, "mock-req-001");
        assert_eq!(tool_name, "Bash");
        assert_eq!(tool_use_id, "toolu_mock001");

        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: Some(tool_input.clone()),
                decision: ApprovalDecision::Allow,
            })
            .expect("respond_approval should succeed");
    }

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(
            event,
            AgentEvent::AgentCompleted { .. } | AgentEvent::AgentFailed { .. }
        )
    });

    let completed = remaining
        .iter()
        .find(|e| matches!(e, AgentEvent::AgentCompleted { .. }));
    assert!(
        completed.is_some(),
        "expected AgentCompleted after Allow, got: {remaining:?}"
    );
}

#[test]
fn control_protocol_claude_allow_uses_tool_input() {
    let adapter = make_adapter("mock_control_cli.sh");
    let request = make_request("test-claude-updated-input");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    let tool_req = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. }));
    assert!(
        tool_req.is_some(),
        "expected ToolPermissionRequest, got: {events:?}"
    );

    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_input,
        ..
    } = tool_req.unwrap()
    {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some("intentionally-ignored-by-claude".to_string()),
                tool_input: Some(tool_input.clone()),
                decision: ApprovalDecision::Allow,
            })
            .expect("respond_approval should succeed");
    }

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected AgentCompleted when updatedInput is present, got: {remaining:?}"
    );
}

#[test]
fn control_protocol_deny_round_trip() {
    let adapter = make_adapter("mock_control_cli.sh");
    let request = make_request("test-deny");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });

    let tool_req = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolPermissionRequest { .. }));
    assert!(tool_req.is_some(), "expected ToolPermissionRequest");

    if let AgentEvent::ToolPermissionRequest { request_id, .. } = tool_req.unwrap() {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: None,
                tool_input: None,
                decision: ApprovalDecision::Deny {
                    message: "User denied".to_string(),
                },
            })
            .expect("respond_approval should succeed");
    }

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(
            event,
            AgentEvent::AgentCompleted { .. } | AgentEvent::AgentFailed { .. }
        )
    });

    let completed = remaining
        .iter()
        .find(|e| matches!(e, AgentEvent::AgentCompleted { .. }));
    assert!(
        completed.is_some(),
        "expected AgentCompleted after Deny, got: {remaining:?}"
    );
}

#[test]
fn control_protocol_cancel_while_waiting_for_approval_finishes() {
    let adapter = make_adapter("mock_control_cli.sh");
    let request = make_request("test-cancel-while-waiting");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. })),
        "expected ToolPermissionRequest before cancel, got: {events:?}"
    );

    handle.cancel();

    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCancelled { .. })
    });
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
        "expected AgentCancelled after pending approval cancel, got: {remaining:?}"
    );
}

#[test]
fn control_protocol_multi_tool_approval() {
    let adapter = make_adapter("mock_control_cli_multi.sh");
    let request = make_request("test-multi");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    // First tool request
    let events1 = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    let req1 = events1
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolPermissionRequest { .. }));
    assert!(req1.is_some(), "expected first ToolPermissionRequest");
    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_name,
        tool_input,
        tool_use_id,
        ..
    } = req1.unwrap()
    {
        assert_eq!(tool_name, "Read");
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: Some(tool_input.clone()),
                decision: ApprovalDecision::Allow,
            })
            .unwrap();
    }

    // Second tool request
    let events2 = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });
    let req2 = events2
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolPermissionRequest { .. }));
    assert!(req2.is_some(), "expected second ToolPermissionRequest");
    if let AgentEvent::ToolPermissionRequest {
        request_id,
        tool_name,
        tool_input,
        tool_use_id,
        ..
    } = req2.unwrap()
    {
        assert_eq!(tool_name, "Bash");
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: Some(tool_input.clone()),
                decision: ApprovalDecision::Allow,
            })
            .unwrap();
    }

    // Completion
    let remaining = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    assert!(
        remaining
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentCompleted { .. })),
        "expected AgentCompleted after multi-tool approval"
    );
}

#[test]
fn control_protocol_session_id_captured() {
    let adapter = make_adapter("mock_control_cli.sh");
    let session_state = Arc::clone(&adapter.session_id);
    let request = make_request("test-session");
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);

    let events = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::ToolPermissionRequest { .. })
    });

    if let Some(AgentEvent::ToolPermissionRequest {
        request_id,
        tool_input,
        tool_use_id,
        ..
    }) = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolPermissionRequest { .. }))
    {
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: Some(tool_use_id.clone()),
                tool_input: Some(tool_input.clone()),
                decision: ApprovalDecision::Allow,
            })
            .unwrap();
    }
    let _ = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    let captured = session_state.lock().unwrap().clone();
    assert_eq!(
        captured.as_deref(),
        Some("mock-session-001"),
        "session_id should be committed after completion"
    );
}
