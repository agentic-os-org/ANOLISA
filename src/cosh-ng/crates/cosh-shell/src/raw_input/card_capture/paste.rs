//! Bracketed-paste mode for interactive card captures (#1721).
//!
//! While a paste is in progress (`200~` seen, `201~` not yet), payload
//! bytes are data, not keystrokes: the [`CardInputState::pasting`] guard
//! at the top of `consume_split` routes control and escape bytes to the
//! payload path, extending what `draft_pasted_newline`/`draft_pasted_tab`
//! already did for CRLF and Tab.

use super::{CardInputState, RawInputCapture};

/// True when `bytes` is the bracketed-paste closer `ESC [ 2 0 1 ~`, or a
/// prefix of it that may still complete across split reads.
pub(super) fn is_paste_closer_prefix(bytes: &[u8]) -> bool {
    const CLOSER: &[u8] = b"\x1b[201~";
    bytes.starts_with(CLOSER) || CLOSER.starts_with(bytes)
}

impl CardInputState {
    /// True while the active capture is receiving a bracketed-paste
    /// payload. Draft and free-text captures track the paste; choice-only
    /// cards buffer no text and keep treating every byte as a key.
    pub(super) fn pasting(&self, capture: &RawInputCapture) -> bool {
        self.pasting
            && matches!(
                capture,
                RawInputCapture::PromptDraft { .. }
                    | RawInputCapture::TextQuestion { .. }
                    | RawInputCapture::Question {
                        allow_free_text: true,
                        ..
                    }
            )
    }
}
