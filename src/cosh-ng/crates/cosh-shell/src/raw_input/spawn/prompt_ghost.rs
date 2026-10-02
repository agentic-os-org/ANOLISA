//! Pending prompt-ghost state shared by raw-input relay paths.

use std::time::Instant;

use super::super::event_sender::RawInputEventSink;
use super::super::mode::RawInputMode;
use super::super::{PromptGhostRoute, RawInputEvent};

pub(super) struct PendingPromptGhostEscape {
    pub(super) bytes: Vec<u8>,
    pub(super) text: String,
    pub(super) route: PromptGhostRoute,
    pub(super) deadline: Instant,
}

pub(super) struct PendingReplacedPromptGhostSuffix {
    pub(super) bytes: Vec<u8>,
    pub(super) deadline: Instant,
    pub(super) expected_capture_generation: Option<u64>,
}

impl PendingPromptGhostEscape {
    pub(super) fn matches_mode(&self, mode: &RawInputMode) -> bool {
        matches!(
            mode,
            RawInputMode::PromptGhost { text, route }
                if text == &self.text && route == &self.route
        )
    }
}

pub(super) fn dismiss_replaced_prompt_ghost(input_events: &dyn RawInputEventSink) {
    let _ = input_events.send(RawInputEvent::PromptGhostClear);
    let _ = input_events.send(RawInputEvent::PromptGhostDismissed);
}
