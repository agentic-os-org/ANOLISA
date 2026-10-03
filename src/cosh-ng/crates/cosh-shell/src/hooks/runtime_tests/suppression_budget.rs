use super::*;

#[test]
fn interruption_policy_combines_downgrade_inputs_without_escalating() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command_at("docker exec app free -m", INTERRUPTION_BUDGET_WINDOW_MS);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.ignored_cards.insert(suppression_key.clone());
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
    assert_ne!(decision.reason, "allowed");
}

#[test]
fn repeated_same_card_downgrades_to_hint() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block(0);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.rendered_cards.insert(
        suppression_key.clone(),
        HookSuppressionRecord {
            severity: FindingSeverity::Critical,
        },
    );

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &state.hooks,
            state.agent_run.active.is_some()
        ),
        RuntimeHookDisplay::Hint
    );
}

#[test]
fn repeated_recent_pressure_process_same_pid_downgrades_to_hint() {
    let mut state = InlineState::default();
    let pressure_block = block_with_command_at("free -m", 1_000);
    let pressure =
        aggregate_hook_findings(vec![finding("memory-pressure", FindingSeverity::Warning)])
            .remove(0);
    record_aggregated_hook_finding(&pressure_block, pressure, &mut state);

    let mut first_process_block = block_with_command_at("ps aux --sort=-%mem | head", 2_000);
    first_process_block.id = "cmd-2".to_string();
    let first_process =
        aggregate_hook_findings(vec![process_finding("java (PID 1234) uses 31.2% MEM")]).remove(0);
    record_aggregated_hook_finding(&first_process_block, first_process, &mut state);
    let first_hint = state.hooks.findings.last().unwrap().clone();
    assert_eq!(first_hint.display, RuntimeHookDisplay::Consultation);
    state.hooks.rendered_cards.insert(
        first_hint.suppression_key.clone(),
        HookSuppressionRecord {
            severity: first_hint.effective_severity,
        },
    );

    let mut second_process_block = block_with_command_at("ps aux --sort=-%mem | head", 3_000);
    second_process_block.id = "cmd-3".to_string();
    let second_process =
        aggregate_hook_findings(vec![process_finding("java (PID 1234) uses 32.0% MEM")]).remove(0);
    record_aggregated_hook_finding(&second_process_block, second_process, &mut state);

    let second_hint = state.hooks.findings.last().unwrap();
    assert_eq!(second_hint.display, RuntimeHookDisplay::Hint);
    assert_eq!(second_hint.display_reason, "same-card-already-rendered");
}

#[test]
fn ignored_same_hook_card_downgrades_to_hint() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block(0);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.ignored_cards.insert(suppression_key.clone());

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &state.hooks,
            state.agent_run.active.is_some()
        ),
        RuntimeHookDisplay::Hint
    );
}

#[test]
fn interruption_budget_downgrades_recent_topic_card_to_hint() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command_at("free -m", INTERRUPTION_BUDGET_WINDOW_MS + 10);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.interruption_budget.insert(
        topic_budget_key("memory"),
        InterruptionBudgetRecord {
            last_rendered_at_ms: block.ended_at_ms - 100,
            severity: FindingSeverity::Critical,
        },
    );

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &state.hooks,
            state.agent_run.active.is_some()
        ),
        RuntimeHookDisplay::Hint
    );
}

#[test]
fn interruption_budget_allows_severity_upgrade() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command_at("free -m", INTERRUPTION_BUDGET_WINDOW_MS + 10);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.interruption_budget.insert(
        topic_budget_key("memory"),
        InterruptionBudgetRecord {
            last_rendered_at_ms: block.ended_at_ms - 100,
            severity: FindingSeverity::Warning,
        },
    );

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &state.hooks,
            state.agent_run.active.is_some()
        ),
        RuntimeHookDisplay::Consultation
    );
}

#[test]
fn interruption_budget_expires_after_window() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command_at("free -m", INTERRUPTION_BUDGET_WINDOW_MS + 10);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.interruption_budget.insert(
        topic_budget_key("memory"),
        InterruptionBudgetRecord {
            last_rendered_at_ms: 1,
            severity: FindingSeverity::Critical,
        },
    );

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &state.hooks,
            state.agent_run.active.is_some()
        ),
        RuntimeHookDisplay::Consultation
    );
}

#[test]
fn interruption_budget_does_not_downgrade_failed_command_path() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let mut block = block(1);
    block.command = "free -m".to_string();
    block.ended_at_ms = INTERRUPTION_BUDGET_WINDOW_MS + 10;
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state.hooks.interruption_budget.insert(
        topic_budget_key("memory"),
        InterruptionBudgetRecord {
            last_rendered_at_ms: block.ended_at_ms - 100,
            severity: FindingSeverity::Critical,
        },
    );

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &state.hooks,
            state.agent_run.active.is_some()
        ),
        RuntimeHookDisplay::Consultation
    );
}
