use super::*;

#[test]
fn consultation_queue_records_state_ttl_and_output_ref() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let mut output = Vec::new();
    let hint = state.hooks.findings[0].clone();

    state.hooks.pending_consultation = Some(consultation_from_hint(&hint).unwrap());
    render_or_queue_consultation(
        &hint,
        &mut state.hooks,
        state.agent_run.active.is_some(),
        &mut output,
    )
    .expect("queue consultation");

    let queued = state
        .hooks
        .pending_consultation_queue
        .front()
        .expect("queued consultation");
    assert_eq!(queued.state, PendingConsultationState::Queued);
    assert_eq!(queued.output_ref.as_deref(), Some("/tmp/out"));
    assert_eq!(queued.created_at_ms, block.ended_at_ms);
    assert_eq!(
        queued.expires_at_ms,
        block.ended_at_ms + PENDING_CONSULTATION_TTL_MS
    );
}

#[test]
fn consultation_card_queues_then_records_shown_event() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let hint = state.hooks.findings[0].clone();
    let mut output = Vec::new();

    render_or_queue_consultation(
        &hint,
        &mut state.hooks,
        state.agent_run.active.is_some(),
        &mut output,
    )
    .expect("queue card");

    assert!(state.hooks.pending_consultation.is_none());
    assert_eq!(state.hooks.pending_consultation_queue.len(), 1);

    render_next_queued_consultation(
        &mut state.hooks,
        state.agent_run.active.is_some(),
        state.language,
        &mut output,
    )
    .expect("keep queued card");
    assert!(state.hooks.pending_consultation.is_none());
    assert_eq!(state.hooks.pending_consultation_queue.len(), 1);

    mark_front_consultation_idle(&mut state);
    render_next_queued_consultation(
        &mut state.hooks,
        state.agent_run.active.is_some(),
        state.language,
        &mut output,
    )
    .expect("render queued card");

    assert!(state.hooks.pending_consultation.is_some());
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("[Analyze] [Ignore]"), "{rendered}");
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Shown
            && event.finding_id == "hook-cmd-1-memory-pressure"
            && event.display == RuntimeHookDisplay::Consultation
    }));
}

#[test]
fn queued_consultation_rechecks_ignore_before_rendering() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let hint = state.hooks.findings[0].clone();
    state
        .hooks
        .pending_consultation_queue
        .push_back(consultation_from_hint(&hint).unwrap());
    mark_front_consultation_idle(&mut state);
    state
        .hooks
        .ignored_cards
        .insert(hint.suppression_key.clone());
    let mut output = Vec::new();

    render_next_queued_consultation(
        &mut state.hooks,
        state.agent_run.active.is_some(),
        state.language,
        &mut output,
    )
    .expect("recheck queued consultation");

    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Ignored
            && event.finding_id == "hook-cmd-1-memory-pressure"
    }));
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
}

#[test]
fn queued_consultation_rechecks_user_continued_input_before_rendering() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let hint = state.hooks.findings[0].clone();
    state
        .hooks
        .pending_consultation_queue
        .push_back(consultation_from_hint(&hint).unwrap());
    mark_front_consultation_idle(&mut state);
    state
        .hooks
        .blocks_followed_by_user_input
        .insert(block.id.clone());
    let mut output = Vec::new();

    render_next_queued_consultation(
        &mut state.hooks,
        state.agent_run.active.is_some(),
        state.language,
        &mut output,
    )
    .expect("recheck queued consultation");

    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Deferred
            && event.finding_id == "hook-cmd-1-memory-pressure"
            && event.display_reason == "user-continued-input"
    }));
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
}

#[test]
fn queued_consultation_expiry_records_expired_event() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let hint = state.hooks.findings[0].clone();
    let mut consultation = consultation_from_hint(&hint).unwrap();
    let now_ms = consultation.expires_at_ms + 1;

    assert_eq!(
        queued_consultation_decision(
            &mut consultation,
            &mut state.hooks,
            state.agent_run.active.is_some(),
            now_ms
        ),
        QueuedConsultationDecision::Drop
    );

    assert_eq!(consultation.state, PendingConsultationState::Expired);
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Expired
            && event.finding_id == "hook-cmd-1-memory-pressure"
    }));
}

#[test]
fn queued_consultation_noisy_feedback_records_deferred_event() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let hint = state.hooks.findings[0].clone();
    let mut consultation = consultation_from_hint(&hint).unwrap();
    mark_consultation_idle(&mut consultation);
    state
        .hooks
        .feedback
        .insert(hint.suppression_key.clone(), HookFeedback::Noisy);

    assert_eq!(
        queued_consultation_decision(
            &mut consultation,
            &mut state.hooks,
            state.agent_run.active.is_some(),
            block.ended_at_ms
        ),
        QueuedConsultationDecision::Drop
    );

    assert_eq!(consultation.state, PendingConsultationState::Deferred);
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Deferred
            && event.finding_id == "hook-cmd-1-memory-pressure"
    }));
}
