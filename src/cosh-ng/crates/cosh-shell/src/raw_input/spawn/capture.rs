use super::super::pty::NonblockingReplayWrite;
use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

mod native_interrupt;
mod replay;

#[cfg(test)]
use native_interrupt::NativeCtrlC;
use replay::{complete_and_replay_capture, flush_pending_capture_replay};
#[cfg(test)]
use replay::{
    consume_pending_replay_control, is_replay_control, replay_hint_blocks_writing,
    replay_or_reject_after_drain,
};
pub(super) use replay::{replay_mode_accepts_interrupt, wait_for_pending_capture_replay};

pub(super) const REPLAY_IDLE: u8 = 0;
pub(super) const REPLAY_PENDING: u8 = 1;
pub(super) const REPLAY_CTRL_C: u8 = 2;
pub(super) const REPLAY_EOF: u8 = 3;

const CAPTURE_QUARANTINE_MAX_BYTES: usize = 64 * 1024;

/// Submit-window deadline before an unacknowledged capture chain expires.
/// Production keeps the pre-existing 5s bound: the observer ack normally
/// lands within milliseconds, and past the bound the chain is invalidated
/// so the buffered bytes surface as a visible rejection instead of a
/// silent drop. Tests shrink it so expiry paths stay testable without
/// real waits, while leaving headroom for ack helpers on loaded CI hosts.
fn capture_submit_drain_deadline() -> Duration {
    if cfg!(test) {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(5)
    }
}

/// Bounded buffer for bytes that arrive while a submitted capture chain is
/// still draining. The bytes are retained (not just counted) so a cleanly
/// finished chain can replay them into the main-prompt owner instead of
/// silently dropping type-ahead (#1913); unsafe terminal states reject them
/// with user-visible feedback instead.
#[derive(Default)]
pub(super) struct CaptureOwnedInput {
    bytes: Vec<u8>,
    overflowed: bool,
    pending_replay: Option<PendingCaptureReplay>,
    pub(super) replay_control: Arc<AtomicU8>,
}

struct PendingCaptureReplay {
    bytes: Vec<u8>,
    offset: usize,
    generation: u64,
    owner: RawInputMode,
    native_line_state: NativeLineState,
    exit_tracker_after_replay: ExplicitExitTracker,
}

impl CaptureOwnedInput {
    pub(super) fn has_pending_replay(&self) -> bool {
        self.pending_replay.is_some()
    }

    fn observe(&mut self, bytes: &[u8]) -> bool {
        if self.overflowed {
            return false;
        }
        if self.bytes.len().saturating_add(bytes.len()) > CAPTURE_QUARANTINE_MAX_BYTES {
            // Past the cap nothing can be replayed faithfully anymore:
            // discard the whole batch and report the overflow edge once.
            self.bytes.clear();
            self.overflowed = true;
            return true;
        }
        self.bytes.extend_from_slice(bytes);
        false
    }

    fn take_bytes(&mut self) -> Vec<u8> {
        self.overflowed = false;
        std::mem::take(&mut self.bytes)
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.overflowed = false;
        self.pending_replay = None;
        self.replay_control.store(REPLAY_IDLE, Ordering::Release);
    }
}

pub(super) fn capture_owns_input(mode: &RawInputMode) -> bool {
    matches!(
        mode,
        RawInputMode::Capture { .. }
            | RawInputMode::Submitted { .. }
            | RawInputMode::Draining { .. }
            | RawInputMode::Terminal { .. }
    )
}

pub(super) fn capture_generation(mode: &RawInputMode) -> Option<u64> {
    match mode {
        RawInputMode::Capture { generation, .. }
        | RawInputMode::Submitted { generation, .. }
        | RawInputMode::Draining { generation, .. } => Some(*generation),
        _ => None,
    }
}

pub(super) fn capture_quarantine_generation(
    observed_generation: Option<u64>,
    mode: &RawInputMode,
) -> Option<u64> {
    match observed_generation {
        Some(observed)
            if matches!(
                mode,
                RawInputMode::Capture { generation, .. } if *generation == observed
            ) =>
        {
            None
        }
        Some(observed) => Some(observed),
        None => None,
    }
}

pub(super) fn relay_input_chunk(
    bytes: &[u8],
    mut mode: RawInputMode,
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    deferred_input: &mut Option<InputRead>,
    read_context: RelayReadContext<'_>,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    let RelayReadContext {
        read_ahead,
        expected_capture_generation,
        pending_shell_submits,
        ..
    } = read_context;
    loop {
        match mode {
            RawInputMode::Capture {
                capture,
                generation,
                ..
            } => {
                if let Some(expected_generation) = expected_capture_generation {
                    if expected_generation != generation {
                        relay_late_capture_bytes(
                            bytes,
                            expected_generation,
                            card_state,
                            capture_owned_input,
                            relay,
                        )?;
                        return Ok(());
                    }
                }
                let result = consume_captured_input(
                    card_state,
                    &capture,
                    generation,
                    bytes,
                    relay.input_events,
                    relay.input_mode,
                );
                if result.retry {
                    if let Some(expected_generation) = expected_capture_generation {
                        relay_late_capture_bytes(
                            bytes,
                            expected_generation,
                            card_state,
                            capture_owned_input,
                            relay,
                        )?;
                        return Ok(());
                    }
                    mode = current_raw_input_mode(relay.input_mode);
                    continue;
                }
                if result.generation.is_some() {
                    relay.line_buffer.clear();
                    relay.native_line_state.clear();
                    drain_capture_submission(
                        result,
                        card_state,
                        capture_owned_input,
                        deferred_input,
                        read_ahead,
                        relay,
                    )?;
                }
                return Ok(());
            }
            RawInputMode::Submitted { .. } => {
                thread::sleep(Duration::from_millis(1));
                mode = current_raw_input_mode(relay.input_mode);
            }
            RawInputMode::Draining { .. } => {
                card_state.reset();
                drain_abandoned_capture(card_state, capture_owned_input, relay)?;
                mode = current_raw_input_mode(relay.input_mode);
            }
            RawInputMode::Hold => {
                card_state.reset();
                send_held_input_events(bytes, relay.input_events);
                return Ok(());
            }
            RawInputMode::Delay { .. } => {
                card_state.reset();
                relay_delayed_input(bytes, relay)?;
                return Ok(());
            }
            RawInputMode::Passthrough | RawInputMode::Terminal { .. } => {
                card_state.reset();
                relay_passthrough_input_after_shell_submits(bytes, pending_shell_submits, relay)?;
                return Ok(());
            }
            RawInputMode::PromptGhost {
                text: ghost_text,
                route,
            } => {
                card_state.reset();
                relay_prompt_ghost_input(bytes, &ghost_text, &route, relay)?;
                return Ok(());
            }
            RawInputMode::RawPassthrough => {
                card_state.reset();
                relay.line_buffer.clear();
                send_raw_input_events(bytes, relay.input_events);
                relay.native_line_state.observe_shell_bytes(bytes);
                relay.exit_tracker.observe_shell_bytes(bytes);
                write_user_bytes_to_pty(
                    relay.master,
                    relay.input_generation,
                    relay.line_submits,
                    relay.input_events,
                    relay.main_prompt_gate,
                    bytes,
                )?;
                return Ok(());
            }
        }
    }
}

pub(super) fn drain_capture_submission(
    result: CaptureConsumeResult,
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    deferred_input: &mut Option<InputRead>,
    read_ahead: Option<&Receiver<InputRead>>,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    let Some(generation) = result.generation else {
        return Ok(());
    };
    let mut overflow = capture_owned_input.observe(&result.remainder);
    if overflow {
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureOverflow { generation });
        expire_capture_submission(relay.input_mode, generation);
    }

    let deadline = Instant::now() + capture_submit_drain_deadline();
    loop {
        match current_raw_input_mode(relay.input_mode) {
            RawInputMode::Draining {
                generation: active, ..
            } if active == generation => {
                if !overflow {
                    overflow = drain_capture_read_ahead(
                        generation,
                        capture_owned_input,
                        deferred_input,
                        read_ahead,
                        relay,
                    );
                }
                break;
            }
            RawInputMode::Submitted {
                generation: active, ..
            } if active == generation && Instant::now() < deadline => {
                let wait = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5));
                let Some(receiver) = read_ahead else {
                    thread::sleep(wait);
                    continue;
                };
                match receiver.recv_timeout(wait) {
                    Ok(InputRead::Bytes { bytes, .. }) => {
                        if !overflow && capture_owned_input.observe(&bytes) {
                            overflow = true;
                            let _ = relay
                                .input_events
                                .send(RawInputEvent::CaptureOverflow { generation });
                            expire_capture_submission(relay.input_mode, generation);
                        }
                    }
                    Ok(InputRead::CaptureInvalidated) => {}
                    Ok(input @ (InputRead::Eof | InputRead::Error(_))) => {
                        *deferred_input = Some(input);
                        expire_capture_submission(relay.input_mode, generation);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        *deferred_input = Some(InputRead::Eof);
                        expire_capture_submission(relay.input_mode, generation);
                    }
                }
            }
            RawInputMode::Submitted {
                generation: active, ..
            } if active == generation => {
                let _ = relay
                    .input_events
                    .send(RawInputEvent::CaptureExpired { generation });
                expire_capture_submission(relay.input_mode, generation);
            }
            _ => {
                // Ownership cutover mid-wait (e.g. the observer replaced the
                // draining mode with a prompt ghost): the quarantined bytes
                // can no longer reach any safe owner, so surface them as a
                // rejection instead of returning with a silent buffer.
                reject_quarantined_input(capture_owned_input, generation, relay);
                return Ok(());
            }
        }
    }
    if !overflow && complete_capture_chain_if_pending(relay.input_mode, generation) {
        // T2: a follow-up card armed; the no-leak invariant outranks
        // delivery, so the quarantined bytes are rejected visibly (#1913 D5).
        reject_quarantined_input(capture_owned_input, generation, relay);
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureDrained { generation });
        return Ok(());
    }

    if overflow {
        reject_quarantined_input(capture_owned_input, generation, relay);
    }
    let result = complete_and_replay_capture(card_state, capture_owned_input, generation, relay);
    if !capture_owned_input.has_pending_replay() {
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureDrained { generation });
    }
    result
}

#[cfg(test)]
thread_local! {
    static BEFORE_REPLAY_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn before_replay_commit() {
    BEFORE_REPLAY_COMMIT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

/// Discard the quarantined bytes with user-visible feedback; an empty
/// buffer stays silent (nothing was lost).
fn reject_quarantined_input(
    capture_owned_input: &mut CaptureOwnedInput,
    generation: u64,
    relay: &mut InputRelayContext<'_>,
) {
    let bytes = capture_owned_input.take_bytes();
    if !bytes.is_empty() {
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureInputRejected {
                generation,
                byte_len: bytes.len(),
            });
    }
}

pub(in super::super) fn relay_late_capture_input(
    bytes: &[u8],
    generation: u64,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    let RawInputRelayState {
        card_state,
        line_buffer,
        native_line_state,
        exit_tracker,
        capture_owned_input,
        input_generation,
        line_submits,
        main_prompt_gate,
        slash_route_enabled,
        zsh_path_prompt_buffering,
        ..
    } = state;
    let mut relay = InputRelayContext {
        master,
        input_classifier,
        input_events,
        input_mode,
        input_generation,
        line_submits,
        line_buffer,
        native_line_state,
        exit_tracker,
        main_prompt_gate,
        slash_route_enabled: *slash_route_enabled,
        zsh_path_prompt_buffering: zsh_path_prompt_buffering.as_mut(),
    };
    relay_late_capture_bytes(
        bytes,
        generation,
        card_state,
        capture_owned_input,
        &mut relay,
    )
}

fn relay_late_capture_bytes(
    bytes: &[u8],
    generation: u64,
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    let active_generation = match current_raw_input_mode(relay.input_mode) {
        RawInputMode::Capture {
            generation: active, ..
        }
        | RawInputMode::Submitted {
            generation: active, ..
        }
        | RawInputMode::Draining {
            generation: active, ..
        } => Some(active),
        _ => None,
    };
    if active_generation != Some(generation) {
        // The chain these bytes were typed against is gone; delivering them
        // to whatever owns input now would be a leak, so reject visibly
        // instead of the pre-#1913 silent discard. Orphaned leftovers and
        // the current batch merge into one rejection so a chain never
        // stacks duplicate notices.
        let mut rejected_len = bytes.len();
        if active_generation.is_none() {
            // No live chain owns the quarantine buffer anymore: leftover
            // bytes join the rejection instead of being wiped silently.
            rejected_len += capture_owned_input.take_bytes().len();
        }
        // With a live chain of a different generation the buffer holds that
        // chain's type-ahead; it keeps its own terminal verdict untouched.
        if rejected_len > 0 {
            let _ = relay
                .input_events
                .send(RawInputEvent::CaptureInputRejected {
                    generation,
                    byte_len: rejected_len,
                });
        }
        return Ok(());
    }

    let overflow = capture_owned_input.observe(bytes);
    if overflow {
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureOverflow { generation });
        match current_raw_input_mode(relay.input_mode) {
            RawInputMode::Capture {
                generation: active, ..
            } if active == generation => abandon_active_capture(relay.input_mode),
            RawInputMode::Submitted {
                generation: active, ..
            }
            | RawInputMode::Draining {
                generation: active, ..
            } if active == generation => expire_capture_submission(relay.input_mode, active),
            _ => {}
        }
    }

    match current_raw_input_mode(relay.input_mode) {
        RawInputMode::Capture { .. } | RawInputMode::Submitted { .. } if !overflow => Ok(()),
        RawInputMode::Draining { .. } => {
            drain_abandoned_capture(card_state, capture_owned_input, relay)
        }
        _ => {
            reject_quarantined_input(capture_owned_input, generation, relay);
            Ok(())
        }
    }
}

fn drain_capture_read_ahead(
    generation: u64,
    capture_owned_input: &mut CaptureOwnedInput,
    deferred_input: &mut Option<InputRead>,
    read_ahead: Option<&Receiver<InputRead>>,
    relay: &mut InputRelayContext<'_>,
) -> bool {
    let Some(receiver) = read_ahead else {
        return false;
    };
    loop {
        match receiver.try_recv() {
            Ok(InputRead::Bytes { bytes, .. }) => {
                if capture_owned_input.observe(&bytes) {
                    let _ = relay
                        .input_events
                        .send(RawInputEvent::CaptureOverflow { generation });
                    expire_capture_submission(relay.input_mode, generation);
                    return true;
                }
            }
            Ok(InputRead::CaptureInvalidated) => {}
            Ok(input @ (InputRead::Eof | InputRead::Error(_))) => {
                *deferred_input = Some(input);
                return false;
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return false,
        }
    }
}

pub(super) fn drain_abandoned_capture(
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    if capture_owned_input.has_pending_replay() {
        return flush_pending_capture_replay(capture_owned_input, relay);
    }
    let RawInputMode::Draining { generation, .. } = current_raw_input_mode(relay.input_mode) else {
        return Ok(());
    };
    let result = complete_and_replay_capture(card_state, capture_owned_input, generation, relay);
    if !capture_owned_input.has_pending_replay() {
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureDrained { generation });
    }
    result
}

#[cfg(test)]
#[path = "capture_matrix_tests.rs"]
mod matrix_tests;

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
