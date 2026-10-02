use super::*;

#[test]
fn muted_topic_records_silent_finding_reason() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    state.hooks.muted_targets.insert("memory".to_string());

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert_eq!(state.hooks.findings.len(), 1);
    let hint = &state.hooks.findings[0];
    assert_eq!(hint.display, RuntimeHookDisplay::Silent);
    assert_eq!(hint.display_reason, "muted");
    assert_eq!(
        hint.hook_finding
            .as_ref()
            .map(|finding| finding.hook_id.as_str()),
        Some("memory-pressure")
    );
    assert_eq!(state.hooks.display_events.len(), 1);
    let event = &state.hooks.display_events[0];
    assert_eq!(event.action, RuntimeHookDisplayAction::Muted);
    assert_eq!(event.finding_id, "hook-cmd-1-memory-pressure");
    assert_eq!(event.display_reason, "muted");
}

#[test]
fn muted_hook_id_downgrades_consultation_to_silent() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("free -m");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .muted_targets
        .insert("memory-pressure".to_string());

    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &state.hooks,
        state.agent_run.active.is_some(),
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Silent);
    assert_eq!(decision.reason, "muted");
}

#[test]
fn noisy_feedback_downgrades_same_policy_key() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("free -m");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .feedback
        .insert(suppression_key.clone(), HookFeedback::Noisy);

    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &state.hooks,
        state.agent_run.active.is_some(),
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Hint);
    assert_eq!(decision.reason, "feedback-noisy");
}

#[test]
fn noisy_feedback_group_downgrades_same_topic_entity_intent() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("free -m");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .noisy_groups
        .insert(hook_feedback_group_key("memory", "system-memory", "free"));

    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &state.hooks,
        state.agent_run.active.is_some(),
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Hint);
    assert_eq!(decision.reason, "feedback-group-noisy");
}

#[test]
fn useful_feedback_key_bypasses_noisy_feedback_group() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("free -m");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .feedback
        .insert(suppression_key.clone(), HookFeedback::Useful);
    state
        .hooks
        .noisy_groups
        .insert(hook_feedback_group_key("memory", "system-memory", "free"));

    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &state.hooks,
        state.agent_run.active.is_some(),
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Consultation);
    assert_eq!(decision.reason, "allowed");
}

#[test]
fn useful_feedback_does_not_bypass_interruption_budget() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command_at("free -m", INTERRUPTION_BUDGET_WINDOW_MS + 10);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .feedback
        .insert(suppression_key.clone(), HookFeedback::Useful);
    state.hooks.interruption_budget.insert(
        topic_budget_key("memory"),
        InterruptionBudgetRecord {
            last_rendered_at_ms: block.ended_at_ms - 100,
            severity: FindingSeverity::Critical,
        },
    );

    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &state.hooks,
        state.agent_run.active.is_some(),
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Hint);
    assert_eq!(decision.reason, "interruption-budget");
}
