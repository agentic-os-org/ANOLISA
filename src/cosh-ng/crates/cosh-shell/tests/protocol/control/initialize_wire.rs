use super::*;

#[test]
fn cosh_core_initialize_carries_protocol_version_and_capabilities() {
    // #2156: the persistent cosh-core transport must send the versioned
    // initialize handshake with the control capability pair, on the wire —
    // the mock core rejects the session when either is missing.
    let adapter = make_cosh_core_adapter("mock_cosh_core_init_caps_cli.sh");
    let request = make_request("init-caps-wire");
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
            AgentEvent::TextDelta { text, .. } if text.contains("init handshake accepted")
        )) && events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "the versioned initialize with capabilities must be accepted, got: {events:?}"
    );
}
