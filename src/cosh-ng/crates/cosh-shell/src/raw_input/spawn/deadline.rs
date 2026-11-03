//! Blocking receive deadlines for split escape-sequence handling.

use std::fs::File;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    )?;
    if matches!(
        current_raw_input_mode(input_mode),
        RawInputMode::Draining { .. }
    ) || state.capture_owned_input.has_pending_replay()
    {
        let mut card = std::mem::take(&mut state.card_state);
        let mut quarantine = std::mem::take(&mut state.capture_owned_input);
        let mut relay =
            super::input_relay_context(master, input_classifier, input_events, input_mode, state);
        let result = super::drain_abandoned_capture(&mut card, &mut quarantine, &mut relay);
        state.card_state = card;
        state.capture_owned_input = quarantine;
        result?;
    }
    Ok(())
}

pub(super) fn receive_input(
    receiver: &Receiver<InputRead>,
    state: &mut RawInputRelayState,
) -> Result<InputRead, RecvTimeoutError> {
    if !matches!(
        state.deferred_input,
        Some(InputRead::Eof | InputRead::Error(_))
    ) {
        if let Some(input) = state.deferred_input.take() {
            return Ok(input);
        }
    }
    if let Some(input) = state.pending_replay_input.pop_front() {
        if let InputRead::Bytes { bytes, .. } = &input {
            state.pending_replay_input_bytes -= bytes.len();
        }
        return Ok(input);
    }
    if let Some(input) = state.deferred_input.take() {
        return Ok(input);
    }
    match next_pending_deadline(state) {
        Some(deadline) => receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
    }
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
        .chain(
            state
                .zsh_path_prompt_buffering
                .as_ref()
                .and_then(|buffering| buffering.tab_typeahead_deadline()),
        )
        .min()
}
