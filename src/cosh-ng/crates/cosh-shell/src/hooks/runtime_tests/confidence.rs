use super::*;

#[test]
fn aggregate_metadata_is_explicit_after_refresh() {
    let mut aggregated = aggregate_hook_findings(vec![
        process_finding("java (PID 1234) uses 31.2% MEM"),
        finding("memory-pressure", FindingSeverity::Warning),
    ])
    .remove(0);
    let block = block_with_command("top -b -n1 -o %MEM | head -30");

    refresh_aggregate_metadata(&block, &mut aggregated);

    assert_eq!(
        aggregated.recommended_skill.as_deref(),
        Some("memory-analysis")
    );
    assert_eq!(aggregated.topic, "memory");
    assert_eq!(aggregated.entity_key, "system-memory");
    assert_eq!(aggregated.effective_severity, FindingSeverity::Warning);
    assert_eq!(aggregated.confidence, "high");
    assert_eq!(
        aggregated.suppression_key,
        "memory:system-memory:memory-pressure:top:user_interactive"
    );
}

#[test]
fn lookup_intent_records_low_confidence() {
    let aggregate =
        aggregate_hook_findings(vec![process_finding("java (PID 1234) uses 51.2% MEM")]).remove(0);
    let block = block_with_command("LANG=C ps -p 1234 -o pid,%mem,rss,comm");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert_eq!(state.hooks.findings.len(), 1);
    assert_eq!(state.hooks.findings[0].confidence, "low");
    assert_eq!(state.hooks.findings[0].display, RuntimeHookDisplay::Hint);
}

#[test]
fn container_wrapper_memory_pressure_records_low_confidence_hint() {
    let aggregate =
        aggregate_hook_findings(vec![finding("memory-pressure", FindingSeverity::Critical)])
            .remove(0);
    let block = block_with_command("docker exec app free -m");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert_eq!(state.hooks.findings.len(), 1);
    let hint = &state.hooks.findings[0];
    assert_eq!(hint.confidence, "low");
    assert_eq!(hint.display, RuntimeHookDisplay::Hint);
    assert_eq!(hint.display_reason, "non-diagnostic-success-command");
}

#[test]
fn diagnostic_composite_memory_finding_records_high_confidence() {
    let aggregate = aggregate_hook_findings(vec![
        finding("memory-pressure", FindingSeverity::Critical),
        process_finding("java (PID 1234) uses 51.2% MEM"),
    ])
    .remove(0);
    let block = block_with_command("top -b -n1 -o %MEM | head -30");
    let mut state = InlineState::default();

    record_aggregated_hook_finding(&block, aggregate, &mut state);

    assert_eq!(state.hooks.findings.len(), 1);
    assert_eq!(state.hooks.findings[0].confidence, "high");
    assert_eq!(
        state.hooks.findings[0].display,
        RuntimeHookDisplay::Consultation
    );
}

#[test]
fn low_confidence_success_consultation_downgrades_to_hint() {
    let mut finding = finding("memory-pressure", FindingSeverity::Critical);
    finding
        .description
        .push_str(" Confidence is lower because output lacks avail Mem.");
    let aggregated = aggregate_hook_findings(vec![finding]);
    let block = block_with_command("top -b -n1 | head -20");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &HookRuntimeState::default(),
        false,
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Hint);
    assert_eq!(decision.reason, "low-confidence");
}

#[test]
fn low_confidence_policy_does_not_downgrade_failed_command_path() {
    let mut finding = finding("memory-pressure", FindingSeverity::Critical);
    finding
        .description
        .push_str(" Confidence is lower because output lacks avail Mem.");
    let aggregated = aggregate_hook_findings(vec![finding]);
    let block = block(1);
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &HookRuntimeState::default(),
        false,
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Consultation);
    assert_eq!(decision.reason, "allowed");
}
