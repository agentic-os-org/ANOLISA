//! Presents input-ownership status lines for one interactive shell session,
//! gated behind `shell.status_symbols` (default off).

use std::collections::VecDeque;
use std::io::{self, Write};

use crate::input::AssistanceControl;

use super::osc::OscParser;
use super::prompt_replay::{prompt_prefixed_replay_bytes, PromptReplayTracker};

const ASSISTED_SHELL_STATUS: &[u8] = "◇ \r\n".as_bytes();
const SHELL_ONLY_STATUS: &[u8] = "◌ \r\n".as_bytes();

/// Distinguishes a new prompt from a repaint of an already published line.
#[derive(Clone, Copy, Debug)]
pub(super) struct PromptDisplayStart {
    pub(super) position: usize,
    pub(super) publish_status: bool,
}

/// Out-of-band prompt presentation for one interactive shell session.
///
/// When `enabled` is off (the default), the child shell's bytes pass through
/// verbatim: prompt boundaries are drained and discarded, so the prompt keeps
/// its native appearance. When enabled, the status occupies its own
/// outer-terminal line at Enhanced `prompt_ready` boundaries; the child starts
/// its prompt at column zero, so Readline/ZLE retain their own wrapping and
/// cursor geometry.
pub(super) struct PromptPresentation {
    enabled: bool,
    assistance_control: Option<AssistanceControl>,
    pending_starts: VecDeque<PromptDisplayStart>,
}

impl PromptPresentation {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            assistance_control: None,
            pending_starts: VecDeque::new(),
        }
    }

    pub(super) fn with_assistance_control(mut self, control: AssistanceControl) -> Self {
        self.assistance_control = Some(control);
        self
    }

    fn status_line(&self) -> Option<&'static [u8]> {
        if !self.enabled {
            return None;
        }
        Some(
            if self
                .assistance_control
                .as_ref()
                .is_none_or(AssistanceControl::is_enabled)
            {
                ASSISTED_SHELL_STATUS
            } else {
                SHELL_ONLY_STATUS
            },
        )
    }

    pub(super) fn observe(&mut self, parser: &mut OscParser) {
        if self.enabled {
            self.pending_starts
                .extend(parser.drain_prompt_presentation_display_starts());
        } else {
            parser.drain_prompt_presentation_display_starts();
        }
    }

    pub(super) fn write_range<W: Write>(
        &mut self,
        parser: &OscParser,
        start: usize,
        end: usize,
        output: &mut W,
    ) -> io::Result<()> {
        self.discard_before(start);
        let mut cursor = start;
        while let Some(boundary) = self.pending_starts.front().copied() {
            if boundary.position >= end {
                break;
            }
            parser.write_display_range(cursor, boundary.position, output)?;
            if let Some(prefix) = self.status_line().filter(|_| boundary.publish_status) {
                output.write_all(prefix)?;
            }
            self.pending_starts.pop_front();
            cursor = boundary.position;
        }
        parser.write_display_range(cursor, end, output)
    }

    /// Writes bytes already transformed by prompt replay normalization while
    /// retaining the virtual boundary that belongs to their source range.
    pub(super) fn write_transformed_range<W: Write>(
        &mut self,
        start: usize,
        end: usize,
        bytes: &[u8],
        output: &mut W,
    ) -> io::Result<()> {
        self.discard_before(start);
        if bytes.len() == end.saturating_sub(start) {
            let mut cursor = start;
            while let Some(boundary) = self.pending_starts.front().copied() {
                if boundary.position >= end {
                    break;
                }
                output.write_all(&bytes[cursor - start..boundary.position - start])?;
                if let Some(prefix) = self.status_line().filter(|_| boundary.publish_status) {
                    output.write_all(prefix)?;
                }
                self.pending_starts.pop_front();
                cursor = boundary.position;
            }
            return output.write_all(&bytes[cursor - start..]);
        }

        // Zsh may prepend a partial-line marker that replay normalization
        // removes. Its prompt_ready boundary is still the start of the range.
        if self
            .pending_starts
            .front()
            .is_some_and(|boundary| boundary.position == start)
            && !bytes.is_empty()
        {
            if let Some(prefix) = self.status_line().filter(|_| {
                self.pending_starts
                    .front()
                    .is_some_and(|boundary| boundary.publish_status)
            }) {
                output.write_all(prefix)?;
            }
            self.pending_starts.pop_front();
        }
        self.discard_before(end);
        output.write_all(bytes)
    }

    /// Publishes ownership when control returns to Shell, or routing changes.
    /// Repeated draft/ghost redraws use `write_replayed_prompt` instead.
    pub(super) fn write_restored_prompt<W: Write>(
        &self,
        output: &mut W,
        prompt: &[u8],
    ) -> io::Result<()> {
        if !prompt.is_empty() {
            if let Some(prefix) = self.status_line() {
                output.write_all(prefix)?;
            }
        }
        output.write_all(prompt)
    }

    /// Repaints the current prompt without adding another status/output line.
    pub(super) fn write_replayed_prompt<W: Write>(
        &self,
        output: &mut W,
        prompt: &[u8],
    ) -> io::Result<()> {
        output.write_all(prompt)
    }

    pub(super) fn write_display_slice<W: Write>(
        &mut self,
        parser: &OscParser,
        output: &mut W,
        display_start: usize,
        display_end: usize,
        prompt_replay: &mut PromptReplayTracker,
    ) -> io::Result<()> {
        let prompt = parser.last_prompt_display();
        let prefix_len = display_end
            .saturating_sub(display_start)
            .min(prompt.len().max(prompt_replay.pending_prompt_len()).max(1));
        let prefix_end = display_start.saturating_add(prefix_len);
        let prefix = parser.read_display_range(display_start, prefix_end)?;
        let bytes = prompt_replay.strip(prefix.as_ref());
        let effective_start = prefix_end.saturating_sub(bytes.len());
        self.discard_before(effective_start);
        let normalized = prompt_prefixed_replay_bytes(bytes, prompt);
        self.write_transformed_range(effective_start, prefix_end, normalized.as_ref(), output)?;
        self.write_range(parser, prefix_end, display_end, output)
    }

    pub(super) fn discard_before(&mut self, position: usize) {
        while self
            .pending_starts
            .front()
            .is_some_and(|boundary| boundary.position < position)
        {
            self.pending_starts.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser_with_display(bytes: &[u8]) -> OscParser {
        let dir = std::env::temp_dir().join(format!(
            "cosh-shell-prompt-presentation-{}",
            std::process::id()
        ));
        let mut parser = OscParser::new(
            "prompt-presentation-test".to_string(),
            dir,
            "test-marker-token".to_string(),
        );
        parser.feed(bytes).expect("feed display bytes");
        parser
    }

    #[test]
    fn write_range_relays_child_bytes_verbatim_when_disabled() {
        let parser = parser_with_display(b"\x1b[1mowner$ \x1b[0mecho ok\r\nowner$ ");
        let mut output = Vec::new();
        PromptPresentation::new(false)
            .write_range(&parser, 0, parser.display_position(), &mut output)
            .expect("write full range");
        assert_eq!(output, b"\x1b[1mowner$ \x1b[0mecho ok\r\nowner$ ");
    }

    #[test]
    fn transformed_chunks_relay_verbatim_when_disabled() {
        let mut presentation = PromptPresentation::new(false);
        let mut output = Vec::new();
        presentation
            .write_transformed_range(0, 1, b"p", &mut output)
            .unwrap();
        presentation
            .write_transformed_range(1, 3, b"$ ", &mut output)
            .unwrap();
        presentation
            .write_replayed_prompt(&mut output, b"p$ ")
            .unwrap();
        assert_eq!(output, b"p$ p$ ");
    }

    #[test]
    fn restored_prompt_writes_only_the_prompt_when_disabled() {
        let mut output = Vec::new();
        let presentation = PromptPresentation::new(false);
        presentation
            .write_restored_prompt(&mut output, b"")
            .unwrap();
        assert!(output.is_empty());

        // A prompt containing the status glyph is user content and must pass
        // through unchanged.
        presentation
            .write_restored_prompt(&mut output, "◇ owner$ ".as_bytes())
            .unwrap();
        assert_eq!(output, "◇ owner$ ".as_bytes());
    }

    #[test]
    fn chunked_publication_and_repaint_add_only_one_status_line() {
        let mut presentation = PromptPresentation::new(true);
        presentation.pending_starts.extend([
            PromptDisplayStart {
                position: 0,
                publish_status: true,
            },
            PromptDisplayStart {
                position: 3,
                publish_status: false,
            },
        ]);
        let mut output = Vec::new();
        presentation
            .write_transformed_range(0, 1, b"p", &mut output)
            .unwrap();
        presentation
            .write_transformed_range(1, 3, b"$ ", &mut output)
            .unwrap();
        presentation
            .write_transformed_range(3, 6, b"p$ ", &mut output)
            .unwrap();
        presentation
            .write_replayed_prompt(&mut output, b"p$ ")
            .unwrap();
        assert_eq!(output, "◇ \r\np$ p$ p$ ".as_bytes());
    }

    #[test]
    fn empty_restoration_does_not_publish_status() {
        let mut output = Vec::new();
        PromptPresentation::new(true)
            .write_restored_prompt(&mut output, b"")
            .unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn restored_status_distinguishes_enabled_states_from_disabled() {
        let mut output = Vec::new();
        PromptPresentation::new(true)
            .write_restored_prompt(&mut output, b"alice$ ")
            .expect("assisted prompt");
        assert_eq!(output, "◇ \r\nalice$ ".as_bytes());

        output.clear();
        let state_file = std::env::temp_dir().join(format!(
            "cosh-shell-prompt-presentation-{}",
            std::process::id()
        ));
        let control = AssistanceControl::enabled(state_file);
        control.toggle().expect("disable assistance");
        PromptPresentation::new(true)
            .with_assistance_control(control)
            .write_restored_prompt(&mut output, b"alice$ ")
            .expect("Shell-only prompt");
        assert_eq!(output, "◌ \r\nalice$ ".as_bytes());

        output.clear();
        PromptPresentation::new(false)
            .write_restored_prompt(&mut output, b"alice$ ")
            .expect("status symbols disabled");
        assert_eq!(output, b"alice$ ");
    }
}
