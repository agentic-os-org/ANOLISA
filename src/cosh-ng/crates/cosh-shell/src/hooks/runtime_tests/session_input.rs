use super::*;

#[test]
fn continued_user_input_silences_success_consultation() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("free -m");
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .blocks_followed_by_user_input
        .insert(block.id.clone());

    let decision = decide_session_interruption_policy(
        &block,
        &aggregated[0],
        RuntimeHookDisplay::Consultation,
        &suppression_key,
        &state.hooks,
        state.agent_run.active.is_some(),
    );

    assert_eq!(decision.display, RuntimeHookDisplay::Silent);
    assert_eq!(decision.reason, "user-continued-input");
}

#[test]
fn continued_user_input_does_not_downgrade_failed_command_consultation() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let mut block = block_with_command("free -m");
    block.exit_code = 1;
    block.status = CommandStatus::Failed;
    let suppression_key = suppression_key(&block, &aggregated[0]);
    let mut state = InlineState::default();
    state
        .hooks
        .blocks_followed_by_user_input
        .insert(block.id.clone());

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
fn records_blocks_followed_by_any_intercepted_input() {
    let block = block_with_command("free -m");
    let mut card_action = ShellEvent::user_input_intercepted("session", "card-action");
    card_action.component = Some("card".to_string());
    let card_only_events = vec![
        ShellEvent::command_started("session", "cmd-1", "free -m", "/tmp", 10),
        ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "cmd-1",
            0,
            20,
            "/tmp/out",
        ),
        card_action,
    ];
    let mut state = InlineState::default();

    record_blocks_followed_by_user_input(
        &card_only_events,
        std::slice::from_ref(&block),
        &mut state,
    );

    assert!(state.hooks.blocks_followed_by_user_input.contains("cmd-1"));

    state.hooks.blocks_followed_by_user_input.clear();

    let events = vec![
        ShellEvent::command_started("session", "cmd-1", "free -m", "/tmp", 10),
        ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "cmd-1",
            0,
            20,
            "/tmp/out",
        ),
        ShellEvent::user_input_intercepted("session", "what happened"),
    ];

    record_blocks_followed_by_user_input(&events, &[block], &mut state);

    assert!(state.hooks.blocks_followed_by_user_input.contains("cmd-1"));
}

#[test]
fn records_blocks_followed_by_next_command_start() {
    let block = block_with_command("free -m");
    let events = vec![
        ShellEvent::command_started("session", "cmd-1", "free -m", "/tmp", 10),
        ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "cmd-1",
            0,
            20,
            "/tmp/out",
        ),
        ShellEvent::command_started("session", "cmd-2", "echo next", "/tmp", 21),
    ];
    let mut state = InlineState::default();

    record_blocks_followed_by_user_input(&events, &[block], &mut state);

    assert!(state.hooks.blocks_followed_by_user_input.contains("cmd-1"));
}

#[test]
fn manual_mode_records_silently() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);

    assert_eq!(
        display_for_aggregate(&block(0), &aggregated[0], true),
        RuntimeHookDisplay::Silent
    );
}
