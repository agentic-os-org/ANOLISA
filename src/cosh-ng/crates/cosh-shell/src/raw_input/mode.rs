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
    if let RawObserverAction::InvalidateApprovalCapture(id) = action {
        if let RawInputMode::Draining {
            previous_capture,
            next_capture,
            invalidated,
            ..
        } = mode
        {
            if matches!(next_capture, Some(RawInputCapture::Approval { id: active, .. }) if active == id)
            {
                *next_capture = None;
            }
            if matches!(previous_capture, RawInputCapture::Approval { id: active, .. } if active == id)
            {
                *invalidated = true;
            }
            return;
        }
        let capture = match &*mode {
            RawInputMode::Capture {
                capture,
                generation,
                ..
            }
            | RawInputMode::Submitted {
                capture,
                generation,
            } => Some((capture.clone(), *generation)),
            RawInputMode::Terminal {
                previous_capture,
                generation,
            } => Some((previous_capture.clone(), *generation)),
            _ => None,
        };
        if let Some((capture, generation)) = capture {
            if matches!(&capture, RawInputCapture::Approval { id: active, .. } if active == id) {
                *mode = RawInputMode::Draining {
                    previous_capture: capture,
                    generation,
                    next_capture: None,
                    invalidated: true,
                    post_owner: PostCaptureOwner::MainPrompt,
                };
            }
        }
        return;
    }
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
        | RawObserverAction::InvalidateApprovalCapture(_)
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
#[cfg(test)]
pub(crate) fn complete_capture_replay(
    input_mode: &Arc<Mutex<RawInputMode>>,
    generation: u64,
) -> Option<(RawInputMode, PostCaptureOwner, bool)> {
    let mut mode = input_mode.lock().ok()?;
    complete_locked_capture_replay(&mut mode, generation)
}

pub(crate) fn complete_locked_capture_replay(
    mode: &mut RawInputMode,
    generation: u64,
) -> Option<(RawInputMode, PostCaptureOwner, bool)> {
    let RawInputMode::Draining {
        previous_capture,
        generation: active,
        next_capture,
        post_owner,
        invalidated,
    } = &*mode
    else {
        return None;
    };
    if *active != generation {
        return None;
    }
    let previous_capture = previous_capture.clone();
    let post_owner = *post_owner;
    let invalidated = *invalidated;
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
    Some((mode.clone(), post_owner, invalidated))
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
#[path = "mode_tests.rs"]
mod tests;
