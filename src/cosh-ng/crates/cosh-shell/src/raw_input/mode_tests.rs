use super::*;

#[test]
fn terminal_approval_invalidation_rejects_submitted_tail_and_preserves_foreign_owner() {
    let capture = RawInputCapture::Approval {
        id: "dead-approval".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut mode = RawInputMode::Submitted {
        capture: capture.clone(),
        generation: 41,
    };
    update_locked_input_mode(
        &mut mode,
        &RawObserverAction::InvalidateApprovalCapture("foreign".to_string()),
        None,
    );
    assert!(matches!(
        mode,
        RawInputMode::Submitted { generation: 41, .. }
    ));
    update_locked_input_mode(
        &mut mode,
        &RawObserverAction::InvalidateApprovalCapture("dead-approval".to_string()),
        None,
    );
    assert!(matches!(
        mode,
        RawInputMode::Draining {
            generation: 41,
            invalidated: true,
            next_capture: None,
            ..
        }
    ));
    update_locked_input_mode(&mut mode, &RawObserverAction::Continue, Some(41));
    assert!(matches!(
        mode,
        RawInputMode::Draining {
            invalidated: true,
            ..
        }
    ));
    let shared = Arc::new(Mutex::new(mode));
    complete_capture_replay(&shared, 41).expect("drain owner");
    update_input_mode(&shared, &RawObserverAction::CaptureInput(capture), Some(41));
    assert!(matches!(
        current_raw_input_mode(&shared),
        RawInputMode::Terminal { .. }
    ));
    update_input_mode(&shared, &RawObserverAction::Continue, Some(41));
    assert_eq!(current_raw_input_mode(&shared), RawInputMode::Passthrough);
}

#[test]
fn repeated_delay_action_preserves_generation() {
    let state = Arc::new(Mutex::new(new_delay_input_mode()));
    let initial = current_raw_input_mode(&state);
    update_input_mode(&state, &RawObserverAction::DelayShellOutput, None);
    assert_eq!(current_raw_input_mode(&state), initial);
}

#[test]
fn input_ownership_ignores_same_owner_display_updates() {
    let candidates = vec![
        PromptGhostCandidate {
            text: "inspect memory".to_string(),
            suggestion_id: "health-1".to_string(),
        },
        PromptGhostCandidate {
            text: "continue deployment".to_string(),
            suggestion_id: "personal-1".to_string(),
        },
    ];
    let first = RawInputMode::PromptGhost {
        text: candidates[0].text.clone(),
        route: PromptGhostRoute::AgentSelection {
            candidates: candidates.clone(),
            active: 0,
        },
    };
    let cycled = RawInputMode::PromptGhost {
        text: candidates[1].text.clone(),
        route: PromptGhostRoute::AgentSelection {
            candidates: candidates.clone(),
            active: 1,
        },
    };
    assert_ne!(first, cycled);
    assert_eq!(first.input_ownership(), cycled.input_ownership());

    let native = RawInputMode::PromptGhost {
        text: candidates[0].text.clone(),
        route: PromptGhostRoute::NativeShell,
    };
    let intercept = RawInputMode::PromptGhost {
        text: candidates[0].text.clone(),
        route: PromptGhostRoute::AgentIntercept {
            suggestion_id: Some(candidates[0].suggestion_id.clone()),
        },
    };
    assert_ne!(first.input_ownership(), native.input_ownership());
    assert_ne!(first.input_ownership(), intercept.input_ownership());
    assert_ne!(native.input_ownership(), intercept.input_ownership());

    let capture = RawInputCapture::Consultation {
        id: "consult-1".to_string(),
    };
    let lease = RawInputMode::Capture {
        capture: capture.clone(),
        generation: 7,
        installed_at: std::time::Instant::now(),
    };
    let refreshed = RawInputMode::Capture {
        capture: capture.clone(),
        generation: 7,
        installed_at: std::time::Instant::now(),
    };
    assert_eq!(lease.input_ownership(), refreshed.input_ownership());
    let replaced = RawInputMode::Capture {
        capture,
        generation: 8,
        installed_at: std::time::Instant::now(),
    };
    assert_ne!(lease.input_ownership(), replaced.input_ownership());
    assert_ne!(
        first.input_ownership(),
        RawInputMode::Passthrough.input_ownership()
    );
}

#[test]
fn capture_lease_waits_for_ack_and_drains_before_replacement() {
    let state = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let capture = RawInputCapture::Consultation {
        id: "consult-1".to_string(),
    };
    let action = RawObserverAction::CaptureInput(capture.clone());

    update_input_mode(&state, &action, None);
    let RawInputMode::Capture { generation, .. } = current_raw_input_mode(&state) else {
        panic!("capture lease");
    };
    update_input_mode(&state, &action, None);
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Capture {
            generation: active,
            ..
        } if active == generation
    ));

    assert_eq!(submit_capture(&state, &capture), Some(generation));
    update_input_mode(&state, &action, Some(generation.saturating_add(1)));
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Submitted {
            generation: active,
            ..
        } if active == generation
    ));

    let next_capture = RawInputCapture::Consultation {
        id: "consult-2".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(next_capture.clone()),
        Some(generation),
    );
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Draining {
            previous_capture: ref previous,
            generation: active,
            next_capture: Some(ref next),
            ..
        } if previous == &capture && active == generation && next == &next_capture
    ));
    let replacement = RawObserverAction::CaptureInput(next_capture.clone());
    update_input_mode(&state, &replacement, Some(generation));
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Draining {
            generation: active,
            next_capture: Some(ref active_capture),
            ..
        } if active == generation && active_capture == &next_capture
    ));

    assert!(complete_capture_chain_if_pending(&state, generation));
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Capture {
            capture: active,
            generation: active_generation,
            ..
        } if active == next_capture && active_generation != generation
    ));
}

#[test]
fn active_capture_replacement_is_ready_without_an_extra_input() {
    let state = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let picker = RawInputCapture::Session {
        id: "session-panel".to_string(),
        option_count: 2,
        selected: 0,
        marked_for_clear: vec![false; 2],
        confirming_clear: false,
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(picker.clone()),
        None,
    );
    let RawInputMode::Capture {
        generation: picker_generation,
        ..
    } = current_raw_input_mode(&state)
    else {
        panic!("session picker capture");
    };
    let confirmation = RawInputCapture::Session {
        id: "session-panel".to_string(),
        option_count: 2,
        selected: 0,
        marked_for_clear: vec![false; 2],
        confirming_clear: true,
    };

    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(confirmation.clone()),
        None,
    );

    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Capture {
            capture,
            generation,
            ..
        } if capture == confirmation && generation != picker_generation
    ));
}

#[test]
fn expired_capture_rejects_old_and_accepts_new_target_after_drain() {
    let state = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let capture = RawInputCapture::Consultation {
        id: "consult-1".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(capture.clone()),
        None,
    );
    let generation = submit_capture(&state, &capture).expect("submitted capture");
    expire_capture_submission(&state, generation);
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(capture.clone()),
        Some(generation),
    );
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Draining {
            generation: active,
            next_capture: None,
            ..
        } if active == generation
    ));
    assert!(!complete_capture_chain_if_pending(&state, generation));
    let next_capture = RawInputCapture::Consultation {
        id: "consult-2".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(next_capture.clone()),
        Some(generation),
    );
    assert!(!complete_capture_chain_if_pending(&state, generation));
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(next_capture.clone()),
        None,
    );
    assert!(!complete_capture_chain_if_pending(&state, generation));
    let _ = complete_capture_replay(&state, generation);
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Capture { capture: active, .. } if active == next_capture
    ));
}

#[test]
fn terminal_rejects_old_target_and_allows_new_target() {
    let state = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let capture = RawInputCapture::Consultation {
        id: "consult-1".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(capture.clone()),
        None,
    );
    let generation = submit_capture(&state, &capture).expect("submitted capture");
    update_input_mode(&state, &RawObserverAction::Continue, Some(generation));
    assert!(!complete_capture_chain_if_pending(&state, generation));
    let _ = complete_capture_replay(&state, generation);

    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(capture.clone()),
        Some(generation),
    );
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Terminal {
            generation: active,
            ..
        } if active == generation
    ));

    let next_capture = RawInputCapture::Consultation {
        id: "consult-2".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(next_capture.clone()),
        Some(generation),
    );
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Terminal { .. }
    ));
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(next_capture.clone()),
        None,
    );
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Capture { capture: active, .. } if active == next_capture
    ));
}

#[test]
fn drain_completion_uses_live_pending_target() {
    let state = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let capture = RawInputCapture::Consultation {
        id: "consult-a".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(capture.clone()),
        None,
    );
    let generation = submit_capture(&state, &capture).expect("submitted capture");
    let target_b = RawInputCapture::Consultation {
        id: "consult-b".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(target_b),
        Some(generation),
    );
    update_input_mode(&state, &RawObserverAction::Continue, Some(generation));

    assert!(!complete_capture_chain_if_pending(&state, generation));
    let _ = complete_capture_replay(&state, generation);
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Terminal { .. }
    ));

    update_input_mode(&state, &RawObserverAction::Continue, Some(generation));
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(capture.clone()),
        None,
    );
    let generation = submit_capture(&state, &capture).expect("submitted capture");
    let target_b = RawInputCapture::Consultation {
        id: "consult-b".to_string(),
    };
    let target_c = RawInputCapture::Consultation {
        id: "consult-c".to_string(),
    };
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(target_b),
        Some(generation),
    );
    update_input_mode(
        &state,
        &RawObserverAction::CaptureInput(target_c.clone()),
        Some(generation),
    );

    assert!(complete_capture_chain_if_pending(&state, generation));
    assert!(matches!(
        current_raw_input_mode(&state),
        RawInputMode::Capture { capture: active, .. } if active == target_c
    ));
}
