//! Enhanced-session assistance shortcut handling.

use std::borrow::Cow;
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::input::InputClassifier;

use super::super::event_sender::RawInputEventSink;
use super::super::mode::{current_raw_input_mode, RawInputMode};
use super::super::RawInputEvent;
use super::{relay_input_for_mode, RawInputRelayState, RelayReadContext};

const ASSISTANCE_ESCAPE_TIMEOUT: Duration = Duration::from_millis(50);

pub(super) struct PendingAssistanceEscape {
    pub(super) bytes: Vec<u8>,
    pub(super) deadline: Instant,
}

pub(super) fn resolve_assistance_shortcut<'a>(
    bytes: &'a [u8],
    received_at: Instant,
    mode: &RawInputMode,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    state: &mut RawInputRelayState,
) -> io::Result<Option<Cow<'a, [u8]>>> {
    let combined = state
        .pending_assistance_escape
        .take()
        .map(|pending| [pending.bytes.as_slice(), bytes].concat());
    let bytes = match combined {
        Some(bytes) => Cow::Owned(bytes),
        None => Cow::Borrowed(bytes),
    };

    if !assistance_toggle_available(mode, input_classifier, state) {
        return Ok(Some(bytes));
    }
    if b"\x1b[Z".starts_with(bytes.as_ref()) && bytes.len() < 3 {
        state.pending_assistance_escape = Some(PendingAssistanceEscape {
            bytes: bytes.into_owned(),
            deadline: received_at + ASSISTANCE_ESCAPE_TIMEOUT,
        });
        return Ok(None);
    }
    let Some(remainder) = bytes.strip_prefix(b"\x1b[Z") else {
        observe_line_submission(input_classifier, bytes.as_ref());
        return Ok(Some(bytes));
    };
    let Some(control) = input_classifier.assistance_control() else {
        return Ok(Some(bytes));
    };
    if control.toggle().is_err() {
        return Ok(Some(bytes));
    }
    let _ = input_events.send(RawInputEvent::AssistanceToggled);
    if remainder.is_empty() {
        Ok(None)
    } else {
        observe_line_submission(input_classifier, remainder);
        Ok(Some(Cow::Owned(remainder.to_vec())))
    }
}

fn observe_line_submission(input_classifier: &InputClassifier, bytes: &[u8]) {
    if bytes.iter().any(|byte| matches!(byte, b'\r' | b'\n')) {
        if let Some(control) = input_classifier.assistance_control() {
            control.set_at_prompt(false);
        }
    }
}

fn assistance_toggle_available(
    mode: &RawInputMode,
    input_classifier: &InputClassifier,
    state: &RawInputRelayState,
) -> bool {
    input_classifier
        .assistance_control()
        .is_some_and(|control| control.is_at_prompt() && matches!(mode, RawInputMode::Passthrough))
        && state.native_line_state.is_empty()
        && !state.line_buffer.is_active()
}

pub(super) fn flush_pending_assistance_escape(
    force: bool,
    now: Instant,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    let should_flush = state
        .pending_assistance_escape
        .as_ref()
        .is_some_and(|pending| force || now >= pending.deadline);
    if !should_flush {
        return Ok(());
    }
    let Some(pending) = state.pending_assistance_escape.take() else {
        return Ok(());
    };
    relay_input_for_mode(
        &pending.bytes,
        current_raw_input_mode(input_mode),
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
        RelayReadContext::default(),
    )
}
