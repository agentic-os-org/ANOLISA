use super::*;

#[test]
fn active_agent_run_defers_success_consultation_until_run_finishes() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("free -m");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let decision = decide_session_interruption_policy_with_context(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &HookRuntimeState::default(),
        true,
        false,
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Consultation);
    assert_eq!(decision.reason, "active-agent-run-deferred");
}

#[test]
fn active_agent_run_does_not_downgrade_failed_command_consultation() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let mut block = block_with_command("free -m");
    block.exit_code = 1;
    block.status = CommandStatus::Failed;
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let decision = decide_session_interruption_policy_with_context(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &HookRuntimeState::default(),
        true,
        false,
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Consultation);
    assert_eq!(decision.reason, "allowed");
}

#[test]
fn active_agent_run_queues_deferred_consultation_and_renders_after_completion() {
    let active_block = block_with_command("?? slow active run");
    let active_request = AgentRequest {
        id: "active-agent-request".to_string(),
        session_id: active_block.session_id.clone(),
        command_block: active_block.clone(),
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("?? slow active run".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();
    start_agent_run(
        &active_request,
        AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("start active fake run");
    assert!(state.agent_run.active.is_some());

    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    render_recorded_hook_findings(&[block], &mut state, &mut output)
        .expect("queue deferred consultation");

    assert!(state.hooks.pending_consultation.is_none());
    assert_eq!(state.hooks.pending_consultation_queue.len(), 1);
    assert_eq!(
        state.hooks.pending_consultation_queue[0].state,
        PendingConsultationState::Deferred
    );
    assert_eq!(
        state.hooks.pending_consultation_queue[0].display_reason,
        "active-agent-run-deferred"
    );
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Deferred
            && event.display_reason == "active-agent-run-deferred"
    }));

    state.agent_run.active = None;
    mark_front_consultation_idle(&mut state);
    render_next_queued_consultation(
        &mut state.hooks,
        state.agent_run.active.is_some(),
        state.language,
        &mut output,
    )
    .expect("render deferred consultation");

    assert!(state.hooks.pending_consultation.is_some());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    let consultation = state.hooks.pending_consultation.as_ref().unwrap();
    assert_eq!(consultation.state, PendingConsultationState::Displayed);
    assert_eq!(consultation.display_reason, "allowed");
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("[Analyze] [Ignore]"), "{rendered}");
}
