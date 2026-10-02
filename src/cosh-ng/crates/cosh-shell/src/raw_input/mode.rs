use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod model;

pub(crate) use model::{InputOwnership, PostCaptureOwner, RawInputMode};
pub use model::{PromptGhostCandidate, PromptGhostRoute, RawInputCapture, RawObserverAction};

static CAPTURE_GENERATION: AtomicU64 = AtomicU64::new(1);
static DELAY_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(super) fn new_delay_input_mode() -> RawInputMode {
    RawInputMode::Delay {
        generation: DELAY_GENERATION.fetch_add(1, Ordering::Relaxed),
    }
}

pub(crate) fn update_input_mode(
    input_mode: &Arc<Mutex<RawInputMode>>,
    action: &RawObserverAction,
    acknowledged_generation: Option<u64>,
) {
    let Ok(mut mode) = input_mode.lock() else {
        return;
    };
    update_locked_input_mode(&mut mode, action, acknowledged_generation);
}

pub(crate) fn update_locked_input_mode(
    mode: &mut RawInputMode,
    action: &RawObserverAction,
    acknowledged_generation: Option<u64>,
) {
    if matches!(
        action,
        RawObserverAction::Continue | RawObserverAction::RawPassthrough
    ) && matches!(mode, RawInputMode::PromptGhost { .. })
    {
        return;
    }
    if matches!(action, RawObserverAction::DelayShellOutput)
        && matches!(mode, RawInputMode::Delay { .. })
    {
        return;
    }
    if let RawInputMode::Submitted {
        capture,
        generation,
    } = &*mode
    {
        if acknowledged_generation != Some(*generation) {
            return;
        }
        if matches!(action, RawObserverAction::CaptureInput(active) if active == capture) {
            return;
        }
        let next_capture = match action {
            RawObserverAction::CaptureInput(next) => Some(next.clone()),
            _ => None,
        };
        *mode = RawInputMode::Draining {
            previous_capture: capture.clone(),
            generation: *generation,
            next_capture,
            invalidated: false,
            post_owner: PostCaptureOwner::from_action(action),
        };
        return;
    }
    if let RawInputMode::Draining {
        previous_capture,
        next_capture,
        invalidated,
        generation,
        post_owner,
        ..
    } = &mut *mode
    {
        match action {
            RawObserverAction::CaptureInput(next)
                if next != previous_capture
                    && !(*invalidated && acknowledged_generation == Some(*generation)) =>
            {
                *next_capture = Some(next.clone());
            }
            RawObserverAction::CaptureInput(_) => {}
            _ => {
                *next_capture = None;
                // The latest acknowledged owner wins, mirroring the
                // next-capture replacement semantics above.
                *post_owner = PostCaptureOwner::from_action(action);
            }
        }
        return;
    }
    if let RawInputMode::Terminal {
        previous_capture,
        generation,
    } = &*mode
    {
        if matches!(action, RawObserverAction::CaptureInput(next) if next == previous_capture)
            || (matches!(action, RawObserverAction::CaptureInput(_))
                && acknowledged_generation == Some(*generation))
        {
            return;
        }
    }
    if let (RawInputMode::Capture { capture, .. }, RawObserverAction::CaptureInput(next_capture)) =
        (&mut *mode, action)
    {
        if capture.is_session_mark_refresh(next_capture) {
            // Mark redraws do not change key routing, so in-flight input still
            // belongs to this session card.
            *capture = next_capture.clone();
            return;
        }
    }
    if let RawInputMode::Capture {
        capture,
        generation,
        ..
    } = &*mode
    {
        if matches!(action, RawObserverAction::CaptureInput(active) if active == capture) {
            return;
        }
        if let RawObserverAction::CaptureInput(next_capture) = action {
            *mode = RawInputMode::Capture {
                capture: next_capture.clone(),
                generation: CAPTURE_GENERATION.fetch_add(1, Ordering::Relaxed),
                installed_at: std::time::Instant::now(),
            };
            return;
        }
        *mode = RawInputMode::Draining {
            previous_capture: capture.clone(),
            generation: *generation,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::from_action(action),
        };
        return;
    }
    *mode = match action {
        RawObserverAction::CaptureInput(capture) => RawInputMode::Capture {
            capture: capture.clone(),
            generation: CAPTURE_GENERATION.fetch_add(1, Ordering::Relaxed),
            installed_at: std::time::Instant::now(),
        },
        RawObserverAction::HoldShellOutput => RawInputMode::Hold,
        RawObserverAction::DelayShellOutput => new_delay_input_mode(),
        RawObserverAction::RawPassthrough => RawInputMode::RawPassthrough,
        RawObserverAction::RestorePrompt {
            ghost_text: Some(text),
            ghost_route,
        } => RawInputMode::PromptGhost {
            text: text.clone(),
            route: ghost_route.clone(),
        },
        RawObserverAction::Continue
        | RawObserverAction::EmitToPty(_)
        | RawObserverAction::EmitToPtyWithPromptRestore(_)
        | RawObserverAction::InterruptForeground
        | RawObserverAction::RestorePrompt {
            ghost_text: None, ..
        } => RawInputMode::Passthrough,
    };
}

pub(crate) fn current_raw_input_mode(input_mode: &Arc<Mutex<RawInputMode>>) -> RawInputMode {
    input_mode
        .lock()
        .map(|mode| mode.clone())
        .unwrap_or(RawInputMode::Passthrough)
}

pub(crate) fn submit_capture(
    input_mode: &Arc<Mutex<RawInputMode>>,
    expected_capture: &RawInputCapture,
) -> Option<u64> {
    let mut mode = input_mode.lock().ok()?;
    let RawInputMode::Capture {
        capture,
        generation,
        ..
    } = &*mode
    else {
        return None;
    };
    if capture != expected_capture {
        return None;
    }
    let generation = *generation;
    *mode = RawInputMode::Submitted {
        capture: capture.clone(),
        generation,
    };
    Some(generation)
}

pub(crate) fn complete_capture_chain_if_pending(
    input_mode: &Arc<Mutex<RawInputMode>>,
    generation: u64,
) -> bool {
    let Ok(mut mode) = input_mode.lock() else {
        return false;
    };
    let RawInputMode::Draining {
        generation: active,
        next_capture,
        invalidated,
        ..
    } = &*mode
    else {
        return false;
    };
    if *active != generation || *invalidated {
        return false;
    }
    let Some(next_capture) = next_capture.clone() else {
        return false;
    };
    *mode = RawInputMode::Capture {
        capture: next_capture,
        generation: CAPTURE_GENERATION.fetch_add(1, Ordering::Relaxed),
        installed_at: std::time::Instant::now(),
    };
    true
}

/// Completes a drained chain by installing the observer-acknowledged
/// post-capture owner (main prompt, delay, raw passthrough, hold) or the
/// pending next capture. Returns the installed mode so the caller can
/// route the quarantined replay against the exact owner snapshot.
pub(crate) fn complete_capture_replay(
    input_mode: &Arc<Mutex<RawInputMode>>,
    generation: u64,
) -> Option<(RawInputMode, PostCaptureOwner)> {
    let Ok(mut mode) = input_mode.lock() else {
        return None;
    };
    let RawInputMode::Draining {
        previous_capture,
        generation: active,
        next_capture,
        post_owner,
        ..
    } = &*mode
    else {
        return None;
    };
    if *active != generation {
        return None;
    }
    let previous_capture = previous_capture.clone();
    let post_owner = *post_owner;
    *mode = if let Some(capture) = next_capture.clone() {
        RawInputMode::Capture {
            capture,
            generation: CAPTURE_GENERATION.fetch_add(1, Ordering::Relaxed),
            installed_at: std::time::Instant::now(),
        }
    } else {
        match post_owner {
            PostCaptureOwner::Delay => new_delay_input_mode(),
            PostCaptureOwner::RawPassthrough => RawInputMode::RawPassthrough,
            PostCaptureOwner::Hold => RawInputMode::Hold,
            // A ghost owner cannot be reconstructed here; the host replays
            // its pending prompt restore, so land on the terminal state and
            // let the replay verdict reject instead of driving the ghost.
            PostCaptureOwner::MainPrompt | PostCaptureOwner::PromptGhost => {
                RawInputMode::Terminal {
                    previous_capture,
                    generation,
                }
            }
        }
    };
    Some((mode.clone(), post_owner))
}

pub(crate) fn expire_capture_submission(input_mode: &Arc<Mutex<RawInputMode>>, generation: u64) {
    if let Ok(mut mode) = input_mode.lock() {
        if matches!(&*mode, RawInputMode::Submitted { generation: active, .. } if *active == generation)
        {
            let RawInputMode::Submitted { capture, .. } = &*mode else {
                return;
            };
            *mode = RawInputMode::Draining {
                previous_capture: capture.clone(),
                generation,
                next_capture: None,
                invalidated: true,
                post_owner: PostCaptureOwner::MainPrompt,
            };
        } else if let RawInputMode::Draining {
            generation: active,
            next_capture,
            invalidated,
            ..
        } = &mut *mode
        {
            if *active == generation {
                *next_capture = None;
                *invalidated = true;
            }
        }
    }
}

pub(crate) fn abandon_active_capture(input_mode: &Arc<Mutex<RawInputMode>>) {
    if let Ok(mut mode) = input_mode.lock() {
        if let RawInputMode::Capture {
            capture,
            generation,
            ..
        } = &*mode
        {
            *mode = RawInputMode::Draining {
                previous_capture: capture.clone(),
                generation: *generation,
                next_capture: None,
                invalidated: true,
                post_owner: PostCaptureOwner::MainPrompt,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
