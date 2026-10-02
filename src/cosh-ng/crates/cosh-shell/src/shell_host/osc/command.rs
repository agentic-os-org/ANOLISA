// Owner: shell_host. CurrentCommand state, the prompt display-window
// helpers, and the bounded visible-tail tracker, carved out of osc.rs
// (#2196 review: the file had crossed the 1000-line growth bar);
// `OscParser` keeps thin delegation over this data.
use std::collections::VecDeque;

use super::OscParser;
use crate::evidence::ControlSequenceScanner;
use crate::types::{CommandOrigin, ShellCommandAuditIdentity};

pub(super) fn is_shell_exit_command(command: &str) -> bool {
    let trimmed = command.trim();
    trimmed == "exit" || trimmed.starts_with("exit ") || trimmed == "logout"
}

/// A foreground command tracked between its preexec and precmd markers.
#[derive(Debug)]
pub(super) struct CurrentCommand {
    pub(super) id: String,
    pub(super) command: String,
    pub(super) cwd: String,
    pub(super) origin: CommandOrigin,
    pub(super) audit_identity: Option<ShellCommandAuditIdentity>,
    pub(super) started_at_ms: u64,
    pub(super) output_start: usize,
    pub(super) attempt_generation: Option<u64>,
    pub(super) shell_environment_generation: Option<u64>,
}

/// Upper bound (in chars) for a visible line to stay a prompt-tail
/// candidate. Real prompts fit comfortably; a longer line is log spill —
/// and returning only its trailing window would hand
/// `redact_sensitive_text` a suffix with the field-name prefix cut off,
/// bypassing redaction (#2196 review R8) — so over-long lines are
/// disqualified outright instead of truncated.
pub(crate) const VISIBLE_TAIL_MAX_CHARS: usize = 512;

/// Incrementally tracks the last non-blank VISIBLE line of the active
/// command's output (#2196 review R7): the previous design re-cleaned the
/// whole `display` window on every sentinel sample, which turned an
/// approved handoff that logs hundreds of MB before blocking in `read`
/// into an O(output) scan-and-copy inside the relay's main loop. The
/// tracker instead consumes each PTY chunk once as it is appended, carries
/// the escape-scanner state and any split UTF-8 char across chunk
/// boundaries, and disqualifies lines beyond [`VISIBLE_TAIL_MAX_CHARS`]
/// (fail-safe per R8: no tail rather than a redaction-bypassing suffix),
/// so sampling is O(1) and retained state has a hard bound.
#[derive(Debug, Default)]
pub(crate) struct VisibleTailTracker {
    scanner: ControlSequenceScanner,
    /// Last completed non-blank visible line (bounded by construction:
    /// only lines that never overflowed are stored).
    last_nonblank: Option<String>,
    /// Visible chars of the still-unfinished line (bounded).
    pending: VecDeque<char>,
    /// The unfinished line overflowed the candidate bound: its content is
    /// dropped, the line stays disqualified until the next newline, and
    /// completing it acts as a barrier that also clears `last_nonblank`
    /// (R9: never fall back to output older than the over-long line).
    pending_disqualified: bool,
    /// Bytes of a UTF-8 char split across chunk boundaries (max 3).
    utf8_carry: Vec<u8>,
}

impl VisibleTailTracker {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn feed(&mut self, data: &[u8]) {
        let mut bytes;
        let mut input = if self.utf8_carry.is_empty() {
            data
        } else {
            bytes = std::mem::take(&mut self.utf8_carry);
            bytes.extend_from_slice(data);
            &bytes[..]
        };
        loop {
            match std::str::from_utf8(input) {
                Ok(valid) => {
                    self.feed_chars(valid);
                    return;
                }
                Err(err) => {
                    let (valid, rest) = input.split_at(err.valid_up_to());
                    // SAFETY of interpretation: `valid_up_to` guarantees
                    // the prefix is valid UTF-8.
                    self.feed_chars(std::str::from_utf8(valid).unwrap_or(""));
                    match err.error_len() {
                        // Incomplete char at the end: carry it into the
                        // next chunk.
                        None => {
                            self.utf8_carry = rest.to_vec();
                            return;
                        }
                        // Invalid bytes mid-stream: substitute like the
                        // lossy decode the batch path used.
                        Some(skip) => {
                            if let Some(replaced) = self.scanner.push('\u{fffd}') {
                                self.push_visible(replaced);
                            }
                            input = &rest[skip..];
                        }
                    }
                }
            }
        }
    }

    fn feed_chars(&mut self, text: &str) {
        for ch in text.chars() {
            if let Some(visible) = self.scanner.push(ch) {
                self.push_visible(visible);
            }
        }
    }

    fn push_visible(&mut self, visible: char) {
        if visible == '\n' {
            if self.pending_disqualified {
                // R9: the finished over-long line is a BARRIER, not a
                // skip — dropping only the line itself would fall back to
                // an older `last_nonblank` and replay unrelated output as
                // the prompt. Everything before the barrier loses
                // candidacy; only lines after it can become the tail.
                self.last_nonblank = None;
            } else if !self.pending.iter().all(|c| c.is_whitespace()) {
                self.last_nonblank = Some(self.pending.iter().collect());
            }
            self.pending.clear();
            self.pending_disqualified = false;
            return;
        }
        if self.pending_disqualified {
            return;
        }
        if self.pending.len() == VISIBLE_TAIL_MAX_CHARS {
            // R8: dropping the front would hand redaction a suffix with
            // the sensitive field name cut off; the whole line loses tail
            // candidacy instead.
            self.pending.clear();
            self.pending_disqualified = true;
            return;
        }
        self.pending.push_back(visible);
    }

    /// Last non-blank visible line so far: the unfinished line when it has
    /// visible content, otherwise the last completed non-blank candidate
    /// line (#2179: with `echo "prompt"` + `read` the newline-terminated
    /// prompt leaves the final unfinished line empty). An over-long
    /// unfinished line yields NO tail at all (R8): the cursor sits on a
    /// non-candidate line, and surfacing an earlier line would present
    /// unrelated output as the prompt.
    pub(crate) fn tail(&self) -> String {
        if self.pending_disqualified {
            return String::new();
        }
        if !self.pending.iter().all(|c| c.is_whitespace()) {
            return self.pending.iter().collect();
        }
        self.last_nonblank.clone().unwrap_or_default()
    }
}

impl OscParser {
    /// #2025: origin of the command currently tracked between preexec and
    /// precmd, used by the interactive sentinel's trigger gate.
    pub(crate) fn active_command_origin(&self) -> Option<CommandOrigin> {
        self.current.as_ref().map(|current| current.origin)
    }

    /// True while a marker-tracked foreground command is running (between
    /// its preexec and precmd markers).
    pub(crate) fn has_active_foreground_command(&self) -> bool {
        self.current.is_some()
    }

    /// Bounded last non-blank visible line of the running foreground
    /// command's output (#2196 reviews: anchored at the preexec marker so
    /// earlier commands' output is never replayed as the prompt, stripped
    /// by the crate's one escape authority, and O(1) to sample).
    pub(crate) fn active_command_visible_tail(&self) -> String {
        if self.current.is_none() {
            return String::new();
        }
        self.visible_tail.tail()
    }

    pub(crate) fn last_prompt_display(&self) -> &[u8] {
        let Some(start) = self.last_prompt_display_start else {
            return &[];
        };
        if self.display.is_full() {
            return self
                .display
                .resident_slice()
                .get(start..)
                .unwrap_or_default();
        }
        &self.last_prompt_display
    }

    pub(super) fn start_prompt_display_capture(&mut self) {
        self.last_prompt_display_start = Some(self.display.position());
        self.last_prompt_display.clear();
        self.capture_prompt_display = true;
    }

    /// True after the shell's post-hook marker is followed by visible prompt
    /// bytes, excluding output produced by user prompt hooks.
    pub(crate) fn has_prompt_painted_since_ready(&self) -> bool {
        self.prompt_ready_display_start
            .is_some_and(|start| start < self.display.position())
    }
}
