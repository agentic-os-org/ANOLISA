//! Card capture driving for the wizard's finite auth task.
//!
//! Reuses the relay's byte semantics without starting a shell relay: the same
//! parser, the same release rules, and no carry across a released capture.

use super::card_capture::{events::releases_capture, CardInputState};
use super::{RawInputCapture, RawInputEvent};

#[derive(Default)]
pub(crate) struct AuthCardInput {
    state: CardInputState,
    carry: Vec<u8>,
    capture_id: Option<String>,
}

impl AuthCardInput {
    pub(crate) fn consume(
        &mut self,
        capture: &RawInputCapture,
        bytes: &[u8],
    ) -> Vec<RawInputEvent> {
        let id = capture_identity(capture);
        if self.capture_id.as_deref() != Some(id.as_str()) {
            // A new capture must never inherit a half-typed character.
            self.carry.clear();
            self.capture_id = Some(id);
        }
        let mut batch = std::mem::take(&mut self.carry);
        batch.extend_from_slice(bytes);
        self.state.apply_capture(capture);
        let (events, remainder) = self.state.consume_split(capture, &batch);
        if events.iter().any(releases_capture) {
            // Bytes typed after a submit belong to no live capture: they must
            // not advance the next field or reach the wizard.
            self.state.reset();
            self.carry.clear();
        } else {
            self.carry = remainder;
        }
        events
    }

    pub(crate) fn reset(&mut self) {
        self.state.reset();
        self.carry.clear();
        self.capture_id = None;
    }
}

fn capture_identity(capture: &RawInputCapture) -> String {
    match capture {
        RawInputCapture::Question { id, .. }
        | RawInputCapture::TextQuestion { id, .. }
        | RawInputCapture::TrialWaiting { id, .. } => id.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;

    fn question(id: &str) -> RawInputCapture {
        RawInputCapture::Question {
            id: id.to_string(),
            option_count: 2,
            selected: 0,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }
    }

    fn text(id: &str) -> RawInputCapture {
        RawInputCapture::TextQuestion {
            id: id.to_string(),
            initial_text: String::new(),
            secret: false,
        }
    }

    #[test]
    fn bytes_after_a_submit_never_reach_the_next_capture() {
        let mut input = AuthCardInput::default();
        let events = input.consume(&question("first"), b"\rsecond\r");
        assert!(events.iter().any(releases_capture));
        let next = input.consume(&question("second"), b"");
        assert!(next.is_empty(), "carried bytes leaked: {next:?}");
    }

    #[test]
    fn split_utf8_is_completed_across_batches() {
        let mut input = AuthCardInput::default();
        let capture = text("field");
        assert!(input.consume(&capture, &[0xe4, 0xbd]).is_empty());
        let events = input.consume(&capture, &[0xa0]);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RawInputEvent::CardInput(_, text) if text == "你")),
            "{events:?}"
        );
    }

    #[test]
    fn escape_steps_back_and_ctrl_c_aborts() {
        let mut input = AuthCardInput::default();
        let capture = question("menu");
        assert!(matches!(
            input.consume(&capture, b"\x1b").as_slice(),
            [RawInputEvent::QuestionCancel(_)]
        ));
        assert!(matches!(
            input.consume(&capture, &[0x03]).as_slice(),
            [RawInputEvent::QuestionAbort(_)]
        ));
    }

    #[test]
    fn reset_drops_a_partial_character() {
        let mut input = AuthCardInput::default();
        let capture = text("field");
        input.consume(&capture, &[0xe4]);
        input.reset();
        let events = input.consume(&capture, b"a");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RawInputEvent::CardInput(_, text) if text == "a")),
            "{events:?}"
        );
    }
}
}
