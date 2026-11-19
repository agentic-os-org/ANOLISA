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

/// How long a scripted toggle may wait for the shell to repaint its prompt.
/// Long enough for a loaded runner's precmd, short enough that a shell which
/// never reaches the boundary fails the calling test instead of wedging it.
const ASSISTANCE_TOGGLE_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Blocks until [`assistance_toggle_available`] holds.
///
/// Scripted input cannot know when the shell finished repainting after a
/// command: the prompt-ready marker is written by the shell's own precmd, so
/// a fixed sleep either wastes time or races it and gets the toggle refused.
pub(super) fn wait_for_assistance_toggle(
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &RawInputRelayState,
) -> io::Result<()> {
    wait_for_assistance_toggle_within(
        input_classifier,
        input_mode,
        state,
        ASSISTANCE_TOGGLE_WAIT_TIMEOUT,
    )
}

fn wait_for_assistance_toggle_within(
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &RawInputRelayState,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let mode = current_raw_input_mode(input_mode);
        if assistance_toggle_available(&mode, input_classifier, state) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "assistance toggle boundary not reached within {timeout:?}: \
                     at_prompt={:?} passthrough={} native_empty={} line_active={}",
                    input_classifier
                        .assistance_control()
                        .map(|control| control.is_at_prompt()),
                    matches!(mode, RawInputMode::Passthrough),
                    state.native_line_state.is_empty(),
                    state.line_buffer.is_active(),
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use crate::input::AssistanceControl;

    use super::*;

    #[test]
    fn assistance_toggle_wait_times_out_with_the_boundary_state() {
        let state_file = std::env::temp_dir().join(format!(
            "cosh-shell-assistance-wait-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let control = AssistanceControl::enabled(state_file);
        control.set_at_prompt(false);
        let input_classifier = InputClassifier::default().with_assistance_control(control);
        let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
        let state = RawInputRelayState::default();

        let (result_tx, result_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = result_tx.send(wait_for_assistance_toggle_within(
                &input_classifier,
                &input_mode,
                &state,
                Duration::ZERO,
            ));
        });
        let error = result_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the wait must return once its deadline has passed")
            .expect_err("an unreached boundary must not succeed");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let message = error.to_string();
        for expected in [
            "at_prompt=Some(false)",
            "passthrough=true",
            "native_empty=true",
            "line_active=false",
        ] {
            assert!(
                message.contains(expected),
                "timeout error must report {expected}: {message}"
            );
        }
        waiter.join().expect("waiter thread");
    }
}
