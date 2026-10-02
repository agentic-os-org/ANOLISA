//! Input-read batches and the context needed to preserve submission ordering.

use std::io;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::input::InputClassifier;

use super::super::event_parser::{BRACKETED_PASTE_END, BRACKETED_PASTE_START};
use super::super::mode::RawInputMode;
use super::RawInputRelayState;

pub(super) enum InputRead {
    Bytes {
        bytes: Vec<u8>,
        received_at: Instant,
        observed_mode: RawInputMode,
        ownership_changed_during_read: bool,
        pending_shell_submits: u16,
    },
    Eof,
    Error(io::Error),
}

#[derive(Clone, Copy, Default)]
pub(super) struct RelayReadContext<'a> {
    pub(super) read_ahead: Option<&'a Receiver<InputRead>>,
    pub(super) expected_capture_generation: Option<u64>,
    pub(super) observed_mode: Option<&'a RawInputMode>,
    pub(super) pending_shell_submits: usize,
}

pub(super) fn should_split_passthrough_batch(
    bytes: &[u8],
    mode: &RawInputMode,
    input_classifier: &InputClassifier,
    state: &RawInputRelayState,
) -> bool {
    if input_classifier.shell_owns_input()
        || !matches!(mode, RawInputMode::Passthrough)
        || state.line_buffer.is_active()
        || state.native_line_state.paste_sequence_open()
        || bytes
            .windows(BRACKETED_PASTE_START.len())
            .any(|window| window == BRACKETED_PASTE_START || window == BRACKETED_PASTE_END)
    {
        return false;
    }
    bytes
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .is_some_and(|submit| submit + 1 < bytes.len())
}

pub(super) fn is_pending_shell_submission(
    line: &[u8],
    state: &RawInputRelayState,
    input_classifier: &InputClassifier,
) -> bool {
    if !state.native_line_state.is_empty() {
        return true;
    }
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    !line.is_empty()
        && matches!(
            input_classifier.classify(line),
            crate::input::InputDecision::SendToShell(_)
        )
}
