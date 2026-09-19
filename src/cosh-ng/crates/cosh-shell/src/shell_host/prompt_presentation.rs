//! Relays the child shell's prompt bytes without adding outer-terminal lines.

use std::io::{self, Write};

use super::osc::OscParser;
use super::prompt_replay::{prompt_prefixed_replay_bytes, PromptReplayTracker};

/// Out-of-band prompt presentation for one interactive shell session.
///
/// Stays the single owner of prompt writes so every relay path applies the
/// same prompt-replay normalization. Enhanced `prompt_ready` boundaries drive
/// observation and input routing only; the child shell's bytes pass through
/// verbatim, so the prompt keeps its native appearance.
pub(super) struct PromptPresentation;

impl PromptPresentation {
    pub(super) fn new() -> Self {
        Self
    }

    /// Keeps the relay loop's boundary-observation step explicit even though
    /// prompt boundaries no longer change the presented bytes.
    pub(super) fn observe(&mut self, _parser: &mut OscParser) {}

    pub(super) fn write_range<W: Write>(
        &mut self,
        parser: &OscParser,
        start: usize,
        end: usize,
        output: &mut W,
    ) -> io::Result<()> {
        parser.write_display_range(start, end, output)
    }

    /// Writes bytes already transformed by prompt replay normalization.
    pub(super) fn write_transformed_range<W: Write>(
        &mut self,
        bytes: &[u8],
        output: &mut W,
    ) -> io::Result<()> {
        output.write_all(bytes)
    }

    /// Restores the current prompt when control returns to the Shell, or
    /// routing changes. Repeated draft/ghost redraws use
    /// `write_replayed_prompt` instead.
    pub(super) fn write_restored_prompt<W: Write>(
        &self,
        output: &mut W,
        prompt: &[u8],
    ) -> io::Result<()> {
        output.write_all(prompt)
    }

    /// Repaints the current prompt without adding another output line.
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
        let normalized = prompt_prefixed_replay_bytes(bytes, prompt);
        self.write_transformed_range(normalized.as_ref(), output)?;
        self.write_range(parser, prefix_end, display_end, output)
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
    fn write_range_relays_child_bytes_verbatim() {
        let parser = parser_with_display(b"\x1b[1mowner$ \x1b[0mecho ok\r\nowner$ ");
        let mut output = Vec::new();
        PromptPresentation::new()
            .write_range(&parser, 0, parser.display_position(), &mut output)
            .expect("write full range");
        assert_eq!(output, b"\x1b[1mowner$ \x1b[0mecho ok\r\nowner$ ");
    }

    #[test]
    fn transformed_chunks_relay_verbatim_without_decoration() {
        let mut presentation = PromptPresentation::new();
        let mut output = Vec::new();
        presentation
            .write_transformed_range(b"p", &mut output)
            .unwrap();
        presentation
            .write_transformed_range(b"$ ", &mut output)
            .unwrap();
        presentation
            .write_replayed_prompt(&mut output, b"p$ ")
            .unwrap();
        assert_eq!(output, b"p$ p$ ");
    }

    #[test]
    fn restored_prompt_writes_only_the_prompt() {
        let mut output = Vec::new();
        let presentation = PromptPresentation::new();
        presentation
            .write_restored_prompt(&mut output, b"")
            .unwrap();
        assert!(output.is_empty());

        // A prompt containing the former status glyph is user content and
        // must pass through unchanged.
        presentation
            .write_restored_prompt(&mut output, "◇ owner$ ".as_bytes())
            .unwrap();
        assert_eq!(output, "◇ owner$ ".as_bytes());
    }
}
