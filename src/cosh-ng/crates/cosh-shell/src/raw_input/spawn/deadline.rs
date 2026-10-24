//! Blocking receive deadlines for split escape-sequence handling.

use std::fs::File;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::input::InputClassifier;

use super::state::flush_pending_draft_escape;
use super::{
    current_raw_input_mode, flush_deferred_zsh_tab_typeahead, flush_pending_assistance_escape,
    flush_pending_delay_escape, flush_pending_prompt_ghost_escape,
    flush_pending_replaced_prompt_ghost_suffix, InputRead, RawInputEventSink, RawInputMode,
    RawInputRelayState,
};

pub(super) fn flush_pending_deadlines(
    master: &mut File,
    input_classifier: &InputClassifier,
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> std::io::Result<()> {
    let now = Instant::now();
    flush_deferred_zsh_tab_typeahead(
        false,
        now,
        master,
        input_classifier,
        input_events,
        input_mode,
        state,
    )?;
    flush_pending_assistance_escape(
        false,
        now,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
    )?;
    flush_pending_draft_escape(
        now,
        master,
        input_classifier,
        input_events,
        input_mode,
        state,
    )?;
    flush_pending_prompt_ghost_escape(
        false,
        now,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
    )?;
    flush_pending_delay_escape(
        false,
        now,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
    )?;
    let mode = current_raw_input_mode(input_mode);
    flush_pending_replaced_prompt_ghost_suffix(
        false,
        now,
        &mode,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
    )
}

/// Startup type-ahead can coalesce a shell submission with the following
/// path-prompt line into one read; the split parks the remainder in
/// `deferred_input`. Delivering it while the shell still runs the submitted
/// line gets routing refused (`pending_shell_submits > 0`, gate down) and the
/// line executes raw in the shell. Hold such a remainder until the shell is
/// back at its prompt, then resolve the completed submissions so the
/// path-prompt route re-evaluates cleanly (#7897).
///
/// The poll only has to re-check the gate, so it stays coarse: the hold
/// verdict itself is computed once per parked remainder (see
/// `deferred_is_hold_candidate`) and the wait cap dwarfs the poll interval.
const PATH_PROMPT_GATE_POLL: Duration = Duration::from_millis(25);
const PATH_PROMPT_GATE_WAIT_CAP: Duration = Duration::from_secs(2);

/// Whether this deferred remainder is a missing-path natural-language
/// submission that must wait for the gate: exactly one trailing submission,
/// no other control bytes, and `classify_missing_path_submission` matching
/// the current mirror. Shell commands and deferred slash controls keep
/// today's immediate delivery. The verdict is independent of the live gate
/// state, so it stays valid for as long as the remainder is parked and the
/// classify/redact regex work only runs once per remainder, not once per
/// poll (#7897 follow-up).
fn deferred_is_hold_candidate(
    input: &InputRead,
    state: &RawInputRelayState,
    input_classifier: &InputClassifier,
) -> bool {
    let InputRead::Bytes {
        bytes,
        pending_shell_submits,
        ..
    } = input
    else {
        return false;
    };
    if *pending_shell_submits == 0 {
        return false;
    }
    // Slash candidate batches keep today's shell-owned delivery
    // (`route_missing_path_submission` refuses their multi-submission reads).
    if state.deferred_split_slash_bearing {
        return false;
    }
    let Some(submit) = bytes.iter().position(|byte| matches!(byte, b'\n' | b'\r')) else {
        return false;
    };
    if submit + 1 != bytes.len()
        || bytes[..submit]
            .iter()
            .any(|byte| matches!(byte, 0x00..=0x1f | 0x7f))
    {
        return false;
    }
    let mut candidate = state
        .native_line_state
        .clean_visible_line()
        .unwrap_or_default()
        .to_vec();
    candidate.extend_from_slice(&bytes[..submit]);
    let Ok(candidate) = String::from_utf8(candidate) else {
        return false;
    };
    if input_classifier
        .classify_missing_path_submission(&candidate)
        .is_none()
    {
        return false;
    }
    // Startup type-ahead is read blind, before the first prompt. Remainders
    // carrying sensitive text never wait for a route; they keep today's
    // immediate delivery, which the pending count still refuses into the
    // shell-owned path.
    !crate::evidence::redact_sensitive_text(&candidate).1
}

/// Whether a parked deferred remainder must wait for the main prompt gate:
/// it is a hold candidate and the gate is currently down.
pub(super) fn deferred_waits_for_shell_prompt(
    input: &InputRead,
    state: &RawInputRelayState,
    input_classifier: &InputClassifier,
) -> bool {
    deferred_is_hold_candidate(input, state, input_classifier)
        && !state.main_prompt_gate.is_path_prompt_ready()
}

/// Delivers a deferred remainder, resolving submissions the gate already
/// completed: a held remainder delivered after the gate rose re-evaluates
/// routing with `pending_shell_submits` cleared, since those submissions
/// finished with the prompt that raised the gate.
pub(super) fn resolve_held_delivery(
    mut input: InputRead,
    state: &mut RawInputRelayState,
) -> InputRead {
    if state.deferred_prompt_hold_since.take().is_some() {
        state.deferred_prompt_poll = None;
        if let InputRead::Bytes {
            pending_shell_submits,
            ..
        } = &mut input
        {
            if state.main_prompt_gate.is_path_prompt_ready() {
                *pending_shell_submits = 0;
            }
        }
    }
    state.deferred_prompt_hold_candidate = None;
    input
}

pub(super) fn receive_input(
    receiver: &Receiver<InputRead>,
    state: &mut RawInputRelayState,
    input_classifier: &InputClassifier,
) -> Result<InputRead, RecvTimeoutError> {
    if let Some(input) = state.deferred_input.take() {
        // Decide once per parked remainder whether it is hold-worthy at all
        // (shape, classification, sensitivity, and a gate that can actually
        // rise again); the poll loop then only re-checks the gate. Eof/Error
        // never hold, and any cached verdict computed for an overwritten
        // remainder is dropped with it.
        let hold_candidate = match (&input, state.deferred_prompt_hold_candidate) {
            (InputRead::Bytes { .. }, Some(hold_candidate)) => hold_candidate,
            (InputRead::Bytes { .. }, None) => {
                let hold_candidate = deferred_is_hold_candidate(&input, state, input_classifier)
                    && state.main_prompt_gate.has_ever_been_ready();
                state.deferred_prompt_hold_candidate = Some(hold_candidate);
                hold_candidate
            }
            _ => {
                state.deferred_prompt_hold_candidate = None;
                state.deferred_prompt_hold_since = None;
                state.deferred_prompt_poll = None;
                false
            }
        };
        if hold_candidate && !state.main_prompt_gate.is_path_prompt_ready() {
            let now = Instant::now();
            let held_since = state.deferred_prompt_hold_since.unwrap_or(now);
            if now.duration_since(held_since) < PATH_PROMPT_GATE_WAIT_CAP {
                state.deferred_prompt_hold_since = Some(held_since);
                state.deferred_prompt_poll = Some(now + PATH_PROMPT_GATE_POLL);
                state.deferred_input = Some(input);
            } else {
                // The gate never rose; resume the previous immediate delivery.
                state.deferred_prompt_hold_since = None;
                state.deferred_prompt_poll = None;
                state.deferred_prompt_hold_candidate = None;
                return Ok(input);
            }
        } else {
            return Ok(resolve_held_delivery(input, state));
        }
    }
    let input = match next_pending_deadline(state) {
        Some(deadline) => {
            receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))?
        }
        None => receiver
            .recv()
            .map_err(|_| RecvTimeoutError::Disconnected)?,
    };
    // EOF must not strand a gate-held remainder: hand the parked remainder
    // out first and queue the shutdown signal behind it.
    if !matches!(input, InputRead::Bytes { .. }) {
        if let Some(parked) = state.deferred_input.take() {
            state.deferred_input = Some(input);
            return Ok(resolve_held_delivery(parked, state));
        }
    }
    Ok(input)
}

pub(super) fn next_pending_deadline(state: &RawInputRelayState) -> Option<Instant> {
    state
        .pending_prompt_ghost_escape
        .as_ref()
        .map(|pending| pending.deadline)
        .into_iter()
        .chain(
            state
                .pending_delay_escape
                .as_ref()
                .map(|pending| pending.deadline),
        )
        .chain(
            state
                .pending_replaced_prompt_ghost_suffix
                .as_ref()
                .map(|pending| pending.deadline),
        )
        .chain(
            state
                .pending_assistance_escape
                .as_ref()
                .map(|pending| pending.deadline),
        )
        .chain(state.pending_draft_escape_deadline)
        .chain(state.deferred_prompt_poll)
        .chain(
            state
                .zsh_path_prompt_buffering
                .as_ref()
                .and_then(|buffering| buffering.tab_typeahead_deadline()),
        )
        .min()
}
