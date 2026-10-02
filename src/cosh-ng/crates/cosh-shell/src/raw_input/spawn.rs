use std::fs::File;
use std::io::{self, Read};
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::input::InputClassifier;

use super::capture_bridge::{consume_captured_input, CaptureConsumeResult};
use super::card_capture::CardInputState;
use super::event_parser::{CandidateLineBuffer, NativeLineState};
use super::event_sender::{RawInputEventSink, WakingRawInputEventSender};
use super::generation::{LineSubmitCounter, UserPtyInputGeneration};
use super::mode::{
    abandon_active_capture, complete_capture_chain_if_pending, complete_capture_replay,
    current_raw_input_mode, expire_capture_submission, PostCaptureOwner, RawInputMode,
};
use super::pty::write_all_pty;
use super::relay::{
    dismiss_prompt_ghost_input, relay_delayed_input, relay_passthrough_input,
    relay_passthrough_input_after_shell_submits, relay_prompt_ghost_input, send_held_input_events,
    send_raw_input_events, write_user_bytes_to_pty, ExplicitExitTracker, InputRelayContext,
};
use super::{MainPromptGate, PromptGhostRoute, RawInputEvent, ESC};

mod action;
mod assistance;
mod capture;
mod deadline;
mod finish;
mod prompt_ghost;
mod read_batch;
mod reader;
mod state;
#[cfg(test)]
mod tests;

pub(in crate::raw_input) use action::relay_input_bytes;
use action::{
    flush_pending_delay_escape, resolve_pending_delay_escape,
    stale_delay_escape_reached_interactive_owner, PendingDelayEscape,
};
pub(crate) use action::{spawn_raw_action_relay, spawn_raw_action_relay_with_wake};
use assistance::{flush_pending_assistance_escape, resolve_assistance_shortcut};
pub(super) use capture::relay_late_capture_input;
use capture::{
    capture_generation, capture_owns_input, capture_quarantine_generation, drain_abandoned_capture,
    relay_input_chunk, CaptureOwnedInput,
};
use deadline::{flush_pending_deadlines, next_pending_deadline, receive_input};
pub(super) use finish::finish_input_relay;
use prompt_ghost::{
    dismiss_replaced_prompt_ghost, PendingPromptGhostEscape, PendingReplacedPromptGhostSuffix,
};
use read_batch::{
    is_pending_shell_submission, should_split_passthrough_batch, InputRead, RelayReadContext,
};
use reader::read_input_chunks;
pub(in crate::raw_input) use state::input_relay_context;
pub(super) use state::RawInputRelayState;
use state::{flush_deferred_zsh_tab_typeahead, sync_pending_draft_escape};
pub(crate) use state::{RawInputShellRoute, ZshPathPromptBuffering};

const PROMPT_GHOST_ESCAPE_TIMEOUT: Duration = Duration::from_millis(50);
const DELAY_ESCAPE_TIMEOUT: Duration = Duration::from_millis(50);
// Retain a complete split Shift+Tab sequence while the relay handles ESC.
const INPUT_READ_AHEAD_CAPACITY: usize = 3;

pub(crate) fn spawn_raw_input_relay<R>(
    input: R,
    master: File,
    input_events: Sender<RawInputEvent>,
    input_classifier: InputClassifier,
    input_mode: Arc<Mutex<RawInputMode>>,
    input_generation: UserPtyInputGeneration,
    main_prompt_gate: MainPromptGate,
    slash_route_enabled: bool,
) -> JoinHandle<io::Result<()>>
where
    R: Read + Send + 'static,
{
    spawn_raw_input_relay_with_wake(
        input,
        master,
        input_events,
        input_classifier,
        input_mode,
        input_generation,
        RawInputShellRoute::new(main_prompt_gate, slash_route_enabled, None),
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_raw_input_relay_with_wake<R>(
    input: R,
    mut master: File,
    input_events: Sender<RawInputEvent>,
    input_classifier: InputClassifier,
    input_mode: Arc<Mutex<RawInputMode>>,
    input_generation: UserPtyInputGeneration,
    shell_route: RawInputShellRoute,
    input_fd: Option<RawFd>,
    wake: Option<UnixStream>,
) -> JoinHandle<io::Result<()>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let input_events = WakingRawInputEventSender::new(input_events, wake);
        let (read_tx, read_rx) = mpsc::sync_channel(INPUT_READ_AHEAD_CAPACITY);
        // The relay must wake without a later keystroke to resolve a bare ESC.
        let reader_input_mode = input_mode.clone();
        thread::spawn(move || read_input_chunks(input, read_tx, reader_input_mode, input_fd));

        let mut state = RawInputRelayState::with_shell_route(input_generation, shell_route);
        loop {
            sync_pending_draft_escape(&mut state);
            let input = match receive_input(&read_rx, &mut state) {
                Ok(input) => input,
                Err(RecvTimeoutError::Timeout) => {
                    flush_pending_deadlines(
                        &mut master,
                        &input_classifier,
                        &input_events,
                        &input_mode,
                        &mut state,
                    )?;
                    input_events.notify_relay();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => InputRead::Eof,
            };
            match input {
                InputRead::Bytes {
                    mut bytes,
                    received_at,
                    observed_mode,
                    ownership_changed_during_read,
                    pending_shell_submits,
                } => {
                    let mode = current_raw_input_mode(&input_mode);
                    if should_split_passthrough_batch(&bytes, &mode, &input_classifier, &state) {
                        let submit = bytes
                            .iter()
                            .position(|byte| matches!(byte, b'\n' | b'\r'))
                            .expect("split predicate requires a submission");
                        let remainder = bytes.split_off(submit + 1);
                        let shell_owned = is_pending_shell_submission(
                            &bytes[..submit],
                            &state,
                            &input_classifier,
                        );
                        state.deferred_input = Some(InputRead::Bytes {
                            bytes: remainder,
                            received_at,
                            observed_mode: mode.clone(),
                            ownership_changed_during_read: false,
                            pending_shell_submits: pending_shell_submits
                                .saturating_add(u16::from(shell_owned)),
                        });
                    }
                    let observed_generation = capture_generation(&observed_mode);
                    if stale_delay_escape_reached_interactive_owner(&bytes, &observed_mode, &mode) {
                        continue;
                    }
                    if ownership_changed_during_read {
                        if let Some(generation) = observed_generation {
                            relay_late_capture_input(
                                &bytes,
                                generation,
                                &mut master,
                                &input_events,
                                &input_classifier,
                                &input_mode,
                                &mut state,
                            )?;
                        }
                    } else if let Some(generation) =
                        capture_quarantine_generation(observed_generation, &mode)
                    {
                        relay_late_capture_input(
                            &bytes,
                            generation,
                            &mut master,
                            &input_events,
                            &input_classifier,
                            &input_mode,
                            &mut state,
                        )?;
                    } else if observed_generation.is_none() && capture_owns_input(&mode) {
                        relay_input_for_mode(
                            &bytes,
                            observed_mode,
                            &mut master,
                            &input_events,
                            &input_classifier,
                            &input_mode,
                            &mut state,
                            RelayReadContext::default(),
                        )?;
                    } else {
                        relay_input_bytes_with_read_ahead(
                            &bytes,
                            received_at,
                            &mut master,
                            &input_events,
                            &input_classifier,
                            &input_mode,
                            &mut state,
                            RelayReadContext {
                                read_ahead: Some(&read_rx),
                                expected_capture_generation: observed_generation,
                                observed_mode: Some(&mode),
                                pending_shell_submits: usize::from(pending_shell_submits),
                            },
                        )?;
                    }
                    input_events.notify_relay();
                }
                InputRead::Eof => {
                    finish_input_relay(
                        &mut master,
                        &input_events,
                        &input_classifier,
                        &input_mode,
                        &mut state,
                    )?;
                    input_events.notify_relay();
                    return Ok(());
                }
                InputRead::Error(error) => return Err(error),
            }
        }
    })
}

fn relay_input_bytes_with_read_ahead(
    bytes: &[u8],
    received_at: Instant,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
    read_context: RelayReadContext<'_>,
) -> io::Result<()> {
    flush_pending_assistance_escape(
        false,
        received_at,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
    )?;
    let prompt_ghost_timed_out = state
        .pending_prompt_ghost_escape
        .as_ref()
        .is_some_and(|pending| received_at > pending.deadline);
    if prompt_ghost_timed_out {
        flush_pending_prompt_ghost_escape(
            false,
            received_at,
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
        )?;
    }

    let mode = if prompt_ghost_timed_out {
        current_raw_input_mode(input_mode)
    } else {
        read_context
            .observed_mode
            .cloned()
            .unwrap_or_else(|| current_raw_input_mode(input_mode))
    };

    let Some(bytes) = resolve_assistance_shortcut(
        bytes,
        received_at,
        &mode,
        input_events,
        input_classifier,
        state,
    )?
    else {
        return Ok(());
    };
    let bytes = bytes.as_ref();
    flush_pending_replaced_prompt_ghost_suffix(
        false,
        received_at,
        &mode,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
    )?;

    let replaced_combined;
    let bytes = if let Some(pending) = state.pending_replaced_prompt_ghost_suffix.take() {
        if pending.expected_capture_generation != read_context.expected_capture_generation {
            if !pending.bytes.is_empty() {
                relay_input_for_mode(
                    &pending.bytes,
                    mode.clone(),
                    master,
                    input_events,
                    input_classifier,
                    input_mode,
                    state,
                    RelayReadContext {
                        read_ahead: read_context.read_ahead,
                        expected_capture_generation: pending.expected_capture_generation,
                        ..read_context
                    },
                )?;
            }
            return relay_input_bytes_with_read_ahead(
                bytes,
                received_at,
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                read_context,
            );
        }
        replaced_combined = [pending.bytes.as_slice(), bytes].concat();
        if b"[Z".starts_with(&replaced_combined) && replaced_combined.len() < 2 {
            state.pending_replaced_prompt_ghost_suffix = Some(PendingReplacedPromptGhostSuffix {
                bytes: replaced_combined,
                deadline: pending.deadline,
                expected_capture_generation: pending
                    .expected_capture_generation
                    .or(read_context.expected_capture_generation),
            });
            return Ok(());
        }
        if replaced_combined.starts_with(b"[Z") {
            &replaced_combined[2..]
        } else if pending.bytes.is_empty() {
            bytes
        } else {
            relay_input_for_mode(
                &pending.bytes,
                mode.clone(),
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                RelayReadContext {
                    read_ahead: read_context.read_ahead,
                    expected_capture_generation: pending.expected_capture_generation,
                    ..read_context
                },
            )?;
            return relay_input_bytes_with_read_ahead(
                bytes,
                received_at,
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                read_context,
            );
        }
    } else {
        bytes
    };

    let mut pending_deadline = None;
    let mut combined = Vec::new();
    let bytes = if let Some(pending) = state.pending_prompt_ghost_escape.take() {
        if !pending.matches_mode(&mode) {
            if capture_owns_input(&mode) {
                dismiss_replaced_prompt_ghost(input_events);
                state.pending_replaced_prompt_ghost_suffix =
                    Some(PendingReplacedPromptGhostSuffix {
                        bytes: Vec::new(),
                        deadline: pending.deadline,
                        expected_capture_generation: read_context.expected_capture_generation,
                    });
                return relay_input_bytes_with_read_ahead(
                    bytes,
                    received_at,
                    master,
                    input_events,
                    input_classifier,
                    input_mode,
                    state,
                    read_context,
                );
            }
            // Route the previous ghost first so new bytes cannot become its Shift+Tab suffix.
            relay_input_for_mode(
                &pending.bytes,
                mode,
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                RelayReadContext {
                    expected_capture_generation: None,
                    ..read_context
                },
            )?;
            return relay_input_bytes_with_read_ahead(
                bytes,
                received_at,
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                read_context,
            );
        }
        pending_deadline = Some(pending.deadline);
        combined.extend_from_slice(&pending.bytes);
        combined.extend_from_slice(bytes);
        &combined
    } else {
        bytes
    };

    let delay_combined = if let Some(pending) = state.pending_delay_escape.take() {
        let Some(combined) = resolve_pending_delay_escape(
            pending,
            bytes,
            received_at,
            &mode,
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
            read_context,
        )?
        else {
            return Ok(());
        };
        Some(combined)
    } else {
        None
    };
    let bytes = delay_combined.as_deref().unwrap_or(bytes);

    if let RawInputMode::PromptGhost { text, route } = &mode {
        if b"\x1b[Z".starts_with(bytes) && bytes.len() < 3 {
            state.pending_prompt_ghost_escape = Some(PendingPromptGhostEscape {
                bytes: bytes.to_vec(),
                text: text.clone(),
                route: route.clone(),
                deadline: pending_deadline.unwrap_or(received_at + PROMPT_GHOST_ESCAPE_TIMEOUT),
            });
            return Ok(());
        }
    }

    if let RawInputMode::Delay { generation } = mode {
        if bytes.len() != 1 || bytes[0] != ESC {
            return relay_input_for_mode(
                bytes,
                RawInputMode::Delay { generation },
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                read_context,
            );
        }
        state.pending_delay_escape = Some(PendingDelayEscape {
            bytes: bytes.to_vec(),
            deadline: received_at + DELAY_ESCAPE_TIMEOUT,
            generation,
        });
        return Ok(());
    }

    relay_input_for_mode(
        bytes,
        mode,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
        read_context,
    )
}

fn flush_pending_replaced_prompt_ghost_suffix(
    force: bool,
    now: Instant,
    mode: &RawInputMode,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    if !force
        && state
            .pending_replaced_prompt_ghost_suffix
            .as_ref()
            .is_none_or(|pending| now <= pending.deadline)
    {
        return Ok(());
    }
    let Some(pending) = state.pending_replaced_prompt_ghost_suffix.take() else {
        return Ok(());
    };
    if pending.bytes.is_empty() {
        return Ok(());
    }
    relay_input_for_mode(
        &pending.bytes,
        mode.clone(),
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
        RelayReadContext {
            read_ahead: None,
            expected_capture_generation: pending.expected_capture_generation,
            observed_mode: None,
            pending_shell_submits: 0,
        },
    )
}

fn relay_input_for_mode(
    bytes: &[u8],
    mode: RawInputMode,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
    read_context: RelayReadContext<'_>,
) -> io::Result<()> {
    if input_classifier.shell_owns_input() {
        // Native integration has no Cosh prompt/capture state to maintain.
        // Writing directly also avoids translating ordinary PTY writes into
        // synthetic UserInputIntercepted events consumed by Agent context.
        state.exit_tracker.observe_shell_bytes(bytes);
        return write_all_pty(master, bytes);
    }
    if !input_classifier.assistance_enabled() && matches!(mode, RawInputMode::Passthrough) {
        let mut relay =
            input_relay_context(master, input_classifier, input_events, input_mode, state);
        return relay_passthrough_input_after_shell_submits(
            bytes,
            read_context.pending_shell_submits,
            &mut relay,
        )
        .map(|_| ());
    }
    let RawInputRelayState {
        card_state,
        line_buffer,
        native_line_state,
        exit_tracker,
        capture_owned_input,
        deferred_input,
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
    relay_input_chunk(
        bytes,
        mode,
        card_state,
        capture_owned_input,
        deferred_input,
        read_context,
        &mut relay,
    )
}

fn flush_pending_prompt_ghost_escape(
    force: bool,
    now: Instant,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    let should_flush = state
        .pending_prompt_ghost_escape
        .as_ref()
        .is_some_and(|pending| force || now >= pending.deadline);
    if !should_flush {
        return Ok(());
    }
    let Some(pending) = state.pending_prompt_ghost_escape.take() else {
        return Ok(());
    };
    let mode = current_raw_input_mode(input_mode);
    if pending.matches_mode(&mode) {
        let mut relay =
            input_relay_context(master, input_classifier, input_events, input_mode, state);
        let _ = dismiss_prompt_ghost_input(&pending.bytes, &mut relay)?;
        return Ok(());
    }
    if capture_owns_input(&mode) {
        dismiss_replaced_prompt_ghost(input_events);
        return Ok(());
    }
    relay_input_for_mode(
        &pending.bytes,
        mode,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
        RelayReadContext::default(),
    )
}
