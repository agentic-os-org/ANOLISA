use std::borrow::Cow;
use std::time::{Duration, Instant};

use crate::raw_input::UserPtyInputGeneration;

/// PTY silence at a painted, idle prompt after which unmatched submissions
/// are written off as consumed by a foreground program. Readline echoes
/// queued typeahead within milliseconds of painting a prompt, so once the
/// prompt is up a fresh idle gap this long means no shell response is still
/// pending.
const IDLE_RECONCILE_WINDOW: Duration = Duration::from_millis(200);

/// Prompt replay dedup state tied to the user PTY input generation.
///
/// A synthesized prompt (panel restore or handoff) arms a pending replay
/// prefix so the shell's late echo of the same prompt is not painted twice.
/// Real user input must expire that state before its PTY response is parsed:
/// the relay bumps the shared generation before writing to the PTY, so the
/// echo of an empty Enter (bracketed-paste toggles + CRLF + fresh prompt) is
/// never mistaken for the replayed prompt.
///
/// Line submissions are matched one-to-one against shell prompt boundaries
/// (precmd), so neither typeahead entered while a foreground command runs
/// nor several Enters in one relay write can be acknowledged before the
/// shell actually consumed them. Two exceptions keep the ledger honest:
///
/// - A submission whose line the DEBUG trap intercepted still produces a
///   precmd, but its whole response *is* the prompt repaint the replay is
///   armed to strip, so it does not block arming.
/// - Submissions consumed by a foreground program (e.g. `read`, a REPL, a
///   password prompt) never produce a boundary; those are written off once
///   the shell has painted a prompt and idles at it, so one interactive
///   command cannot disable replay dedup for the whole session.
///
/// The ledger only decides whether dedup arms; input safety does not rest
/// on it. [`strip_replayed_prompt_prefix`] fails open: it removes bytes only
/// when a slice opens with the armed prompt verbatim, so a wrong arm costs
/// at most a duplicate prompt paint and can never swallow the response to a
/// user submission.
#[derive(Debug)]
pub(super) struct PromptReplayTracker {
    input_generation: UserPtyInputGeneration,
    /// Highest generation the relay reported as written to the PTY.
    last_written: u64,
    /// Line submissions written to the PTY whose shell response (a prompt
    /// boundary) has not been observed yet.
    outstanding_submits: usize,
    /// Submissions whose line was intercepted by the DEBUG trap: their only
    /// remaining response is the prompt repaint that replay dedup strips.
    pending_intercepts: usize,
    /// When the most recent relay write event was drained; gates idle
    /// reconciliation so a just-written submission is never written off.
    last_write_seen: Option<Instant>,
    idle_reconcile_window: Duration,
    pending_prompt: Option<Vec<u8>>,
    armed_at: u64,
}

impl PromptReplayTracker {
    pub(super) fn new(input_generation: UserPtyInputGeneration) -> Self {
        Self {
            input_generation,
            last_written: 0,
            outstanding_submits: 0,
            pending_intercepts: 0,
            last_write_seen: None,
            idle_reconcile_window: IDLE_RECONCILE_WINDOW,
            pending_prompt: None,
            armed_at: 0,
        }
    }

    /// Records a relay-reported PTY write. The event travels through the
    /// channel ahead of the PTY echo it triggers, so an armed replay from an
    /// older generation can also be expired here.
    pub(super) fn observe_user_write(&mut self, generation: u64, line_submits: usize) {
        self.last_written = generation;
        self.outstanding_submits = self.outstanding_submits.saturating_add(line_submits);
        self.last_write_seen = Some(Instant::now());
        if self.pending_prompt.is_some() && generation != self.armed_at {
            self.pending_prompt = None;
        }
    }

    /// Marks a shell prompt boundary (precmd/shell-ready): the shell
    /// consumed exactly one submitted line and finished responding to it.
    /// Boundaries without a matching submission (e.g. a Ctrl-C prompt
    /// repaint) are ignored by the saturating decrement.
    pub(super) fn observe_prompt_boundary(&mut self) {
        self.outstanding_submits = self.outstanding_submits.saturating_sub(1);
        self.pending_intercepts = self.pending_intercepts.saturating_sub(1);
    }

    /// Marks a DEBUG-trap intercept: the just-submitted line was killed and
    /// the shell will only answer it with a precmd plus a prompt repaint —
    /// exactly what an armed replay strips — so that submission must not
    /// block arming.
    pub(super) fn observe_intercept_cut(&mut self) {
        self.pending_intercepts = self.pending_intercepts.saturating_add(1);
    }

    /// Writes off submissions that will never produce a prompt boundary
    /// because a foreground program consumed them (or a submission byte was
    /// over-counted, e.g. Ctrl-O inside a pager).
    ///
    /// Requires causal evidence that readline is back in control before the
    /// silence window counts: no marker-tracked command running, every relay
    /// write drained, and prompt bytes painted after the shell's post-hook
    /// `prompt_ready` marker (`prompt_painted`). The precmd marker fires before
    /// the user's own PROMPT_COMMAND body and the PS1 paint, so
    /// boundary-plus-silence alone (e.g. a slow PROMPT_COMMAND) proves nothing
    /// about queued typeahead.
    /// Once the prompt is up, readline echoes queued submissions within
    /// milliseconds, so a genuinely pending one cannot survive the window;
    /// without this write-off one `read`/REPL interaction would disable
    /// replay dedup for the rest of the session. Even a premature write-off
    /// is bounded by the strip fail-open to a duplicate prompt paint.
    pub(super) fn reconcile_idle_at_prompt(
        &mut self,
        command_running: bool,
        prompt_painted: bool,
        last_pty_output: Option<Instant>,
    ) {
        if (self.outstanding_submits == 0 && self.pending_intercepts == 0)
            || command_running
            || !prompt_painted
        {
            return;
        }
        if self.input_generation.current() != self.last_written {
            return;
        }
        let now = Instant::now();
        let settled = |at: Option<Instant>| {
            at.is_none_or(|at| now.saturating_duration_since(at) >= self.idle_reconcile_window)
        };
        if settled(self.last_write_seen) && settled(last_pty_output) {
            self.outstanding_submits = 0;
            self.pending_intercepts = 0;
        }
    }

    pub(super) fn idle_reconcile_remaining(
        &self,
        command_running: bool,
        prompt_painted: bool,
        last_pty_output: Option<Instant>,
    ) -> Option<Duration> {
        if (self.outstanding_submits == 0 && self.pending_intercepts == 0)
            || command_running
            || !prompt_painted
            || self.input_generation.current() != self.last_written
        {
            return None;
        }
        let remaining = |at: Option<Instant>| {
            at.map_or(Duration::ZERO, |at| {
                self.idle_reconcile_window.saturating_sub(at.elapsed())
            })
        };
        Some(remaining(self.last_write_seen).max(remaining(last_pty_output)))
    }

    /// Arms replay dedup for a synthesized prompt (panel restore or handoff
    /// prompt echo).
    ///
    /// Refuses to arm while any user input still has an unparsed PTY response:
    /// a submitted line without its prompt boundary yet (typeahead during a
    /// foreground command, several Enters in one write) or a write the output
    /// loop has not drained. This keeps replay matching accurate; the strip
    /// fail-open independently guarantees that a wrong arm cannot swallow an
    /// accept-line response. Submissions the DEBUG trap intercepted are
    /// exempt: their only response is the repaint this arm exists to strip.
    ///
    /// Known limit: editor-execute functions and custom `.inputrc` submission
    /// bindings without CR/LF/Ctrl-O are invisible to the submission counter,
    /// so a boundary or intercept they produce can let this arm over a queued
    /// Enter. The strip fail-open bounds that mistake to a duplicate prompt
    /// paint: the Enter's response never opens with the prompt bytes, so it is
    /// passed through verbatim.
    pub(super) fn arm_for_replay(&mut self, prompt: &[u8]) {
        let current = self.input_generation.current();
        if current != self.last_written || self.outstanding_submits > self.pending_intercepts {
            return;
        }
        self.arm(prompt, current);
    }

    fn arm(&mut self, prompt: &[u8], generation: u64) {
        self.pending_prompt = Some(prompt.to_vec());
        self.armed_at = generation;
    }

    /// Strips the armed replay prefix from PTY display output, expiring the
    /// state first when user input was written after arming.
    pub(super) fn strip<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        self.expire_on_user_input();
        strip_replayed_prompt_prefix(bytes, &mut self.pending_prompt)
    }

    pub(super) fn pending_prompt_len(&self) -> usize {
        self.pending_prompt.as_ref().map_or(0, Vec::len)
    }

    fn expire_on_user_input(&mut self) {
        if self.pending_prompt.is_some() && self.input_generation.current() != self.armed_at {
            self.pending_prompt = None;
        }
    }

    #[cfg(test)]
    pub(super) fn is_armed(&self) -> bool {
        self.pending_prompt.is_some()
    }

    #[cfg(test)]
    pub(super) fn set_idle_reconcile_window(&mut self, window: Duration) {
        self.idle_reconcile_window = window;
    }
}

/// Strips the armed replayed prompt from the next PTY display slice.
///
/// Fail-open contract: bytes are removed only when the slice opens with the
/// replayed prompt verbatim (or its zsh partial-line-marker variant). Any
/// other non-empty slice disarms the replay and passes through untouched.
/// Readline answers an accepted line with CR/LF and/or a bracketed-paste
/// disable *before* the next prompt paint, so an accept-line response can
/// never open with the prompt bytes: a wrongly armed replay costs at most a
/// duplicate prompt paint and never swallows the response to user input.
pub(super) fn strip_replayed_prompt_prefix<'a>(
    bytes: &'a [u8],
    replayed_prompt_prefix: &mut Option<Vec<u8>>,
) -> &'a [u8] {
    let Some(raw_prompt) = replayed_prompt_prefix.as_deref() else {
        return bytes;
    };
    if bytes.is_empty() {
        return bytes;
    }

    let replay_prompt = prompt_replay_bytes(raw_prompt);
    let stripped = if bytes.starts_with(raw_prompt) {
        Some(&bytes[raw_prompt.len()..])
    } else if replay_prompt.len() != raw_prompt.len() && bytes.starts_with(replay_prompt) {
        Some(&bytes[replay_prompt.len()..])
    } else {
        None
    };

    *replayed_prompt_prefix = None;
    stripped.unwrap_or(bytes)
}

pub(super) fn prompt_replay_bytes(prompt: &[u8]) -> &[u8] {
    strip_zsh_partial_line_marker(prompt).unwrap_or(prompt)
}

pub(super) fn prompt_prefixed_replay_bytes<'a>(bytes: &'a [u8], prompt: &'a [u8]) -> Cow<'a, [u8]> {
    if prompt.is_empty() || !bytes.starts_with(prompt) {
        return Cow::Borrowed(bytes);
    }

    let replay = prompt_replay_bytes(prompt);
    if replay.len() == prompt.len() {
        return Cow::Borrowed(bytes);
    }

    let mut replayed = Vec::with_capacity(replay.len() + bytes.len().saturating_sub(prompt.len()));
    replayed.extend_from_slice(replay);
    replayed.extend_from_slice(&bytes[prompt.len()..]);
    Cow::Owned(replayed)
}

fn strip_zsh_partial_line_marker(prompt: &[u8]) -> Option<&[u8]> {
    let marker_end = prompt.iter().position(|byte| *byte == b'\n')?;
    if marker_end > 512 {
        return None;
    }
    if !visible_line_is_zsh_partial_marker(&prompt[..marker_end]) {
        return None;
    }
    let after_newline = marker_end + 1;
    if prompt[after_newline..].starts_with(b"\x1b[A") {
        Some(&prompt[marker_end..])
    } else {
        Some(&prompt[after_newline..])
    }
}

fn visible_line_is_zsh_partial_marker(line: &[u8]) -> bool {
    let mut visible = Vec::new();
    let mut idx = 0;
    while idx < line.len() {
        match line[idx] {
            b'\x1b' if line.get(idx + 1) == Some(&b'[') => {
                idx += 2;
                while idx < line.len() {
                    let byte = line[idx];
                    idx += 1;
                    if byte.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            b'\r' => idx += 1,
            b'\x08' => {
                visible.pop();
                idx += 1;
            }
            byte => {
                visible.push(byte);
                idx += 1;
            }
        }
    }

    visible
        .iter()
        .all(|byte| byte.is_ascii_whitespace() || *byte == b'%')
        && visible.contains(&b'%')
        && visible.iter().filter(|byte| **byte == b'%').count() == 1
}

#[cfg(test)]
mod tests {
    use super::{prompt_prefixed_replay_bytes, prompt_replay_bytes, strip_replayed_prompt_prefix};

    #[test]
    fn prompt_replay_strips_zsh_partial_line_marker() {
        let prompt = b"\x1b[0m\x1b[1m\x1b[7m%\x1b[27m\x1b[0m      \r\x1b[K\r\r\n\x1b[Aprompt> ";

        assert_eq!(prompt_replay_bytes(prompt), b"\n\x1b[Aprompt> ");
    }

    #[test]
    fn prompt_replay_strips_plain_zsh_percent_marker_line() {
        let prompt = b"%\r\nprompt> ";

        assert_eq!(prompt_replay_bytes(prompt), b"prompt> ");
    }

    #[test]
    fn prompt_replay_strips_styled_plain_percent_marker_line() {
        let prompt = b"\x1b[1m%\x1b[0m   \r\x1b[K\nprompt> ";

        assert_eq!(prompt_replay_bytes(prompt), b"prompt> ");
    }

    #[test]
    fn prompt_replay_keeps_literal_percent_prompt() {
        let prompt = b"usage 50% prompt> ";

        assert_eq!(prompt_replay_bytes(prompt), prompt);
    }

    #[test]
    fn prompt_replay_keeps_multiline_prompt_with_non_marker_percent() {
        let prompt = b"usage 50%\nprompt> ";

        assert_eq!(prompt_replay_bytes(prompt), prompt);
    }

    #[test]
    fn prompt_prefixed_replay_strips_marker_when_releasing_held_prompt() {
        let prompt = b"\x1b[1m%\x1b[0m   \r\x1b[K\nprompt> ";
        let display = b"\x1b[1m%\x1b[0m   \r\x1b[K\nprompt> echo after\r\n";

        assert_eq!(
            prompt_prefixed_replay_bytes(display, prompt).as_ref(),
            b"prompt> echo after\r\n"
        );
    }

    #[test]
    fn prompt_prefixed_replay_keeps_non_prompt_output() {
        let prompt = b"prompt> ";
        let display = b"%\r\nregular command output\r\n";

        assert_eq!(
            prompt_prefixed_replay_bytes(display, prompt).as_ref(),
            display
        );
    }

    #[test]
    fn replayed_prompt_prefix_is_suppressed_from_next_pty_echo() {
        let mut replayed = Some(b"prompt> ".to_vec());

        assert_eq!(
            strip_replayed_prompt_prefix(b"prompt> echo after\r\n", &mut replayed),
            b"echo after\r\n"
        );
        assert!(replayed.is_none());
    }

    #[test]
    fn replayed_prompt_prefix_fails_open_on_leading_newline() {
        let mut replayed = Some(b"prompt> ".to_vec());

        // A CR/LF before the prompt is an accept-line response, not the
        // replay echo: it must reach the terminal and disarm the replay.
        assert_eq!(
            strip_replayed_prompt_prefix(b"\r\nprompt> echo after\r\n", &mut replayed),
            b"\r\nprompt> echo after\r\n"
        );
        assert!(replayed.is_none());
    }

    #[test]
    fn replayed_prompt_prefix_fails_open_on_bracketed_paste_disable() {
        let mut replayed = Some(b"prompt> \x1b[?2004h".to_vec());

        // ESC[?2004l is only emitted when readline accepts a line; the
        // whole slice is a user submission response and passes through.
        assert_eq!(
            strip_replayed_prompt_prefix(
                b"\x1b[?2004l\r\nprompt> \x1b[?2004hecho after\r\n",
                &mut replayed
            ),
            b"\x1b[?2004l\r\nprompt> \x1b[?2004hecho after\r\n"
        );
        assert!(replayed.is_none());
    }

    #[test]
    fn replayed_prompt_prefix_fails_open_on_control_only_slice() {
        let mut replayed = Some(b"prompt> \x1b[?2004h".to_vec());

        // A control-only slice (paste toggle + CRLF) is the first half of a
        // split accept-line response: pass it through verbatim and disarm so
        // the follow-up prompt paint is never swallowed either.
        assert_eq!(
            strip_replayed_prompt_prefix(b"\x1b[?2004l\r\n", &mut replayed),
            b"\x1b[?2004l\r\n"
        );
        assert!(replayed.is_none());
        assert_eq!(
            strip_replayed_prompt_prefix(b"prompt> \x1b[?2004hecho after\r\n", &mut replayed),
            b"prompt> \x1b[?2004hecho after\r\n"
        );
    }

    #[test]
    fn replayed_prompt_prefix_strips_one_prompt_and_keeps_trailing_accept_line() {
        let mut replayed = Some(b"prompt> \x1b[?2004h".to_vec());

        // The slice opens with the replayed prompt, so that echo is deduped;
        // the trailing accept-line bytes belong to a user Enter and survive,
        // as does the fresh prompt in the next slice (one arm strips at most
        // one prompt paint).
        assert_eq!(
            strip_replayed_prompt_prefix(b"prompt> \x1b[?2004h\x1b[?2004l\r\n", &mut replayed),
            b"\x1b[?2004l\r\n"
        );
        assert!(replayed.is_none());
        assert_eq!(
            strip_replayed_prompt_prefix(b"prompt> \x1b[?2004hecho after\r\n", &mut replayed),
            b"prompt> \x1b[?2004hecho after\r\n"
        );
    }

    #[test]
    fn replayed_prompt_prefix_survives_empty_slice() {
        let mut replayed = Some(b"prompt> ".to_vec());

        assert_eq!(strip_replayed_prompt_prefix(b"", &mut replayed), b"");
        assert!(replayed.is_some());
        assert_eq!(
            strip_replayed_prompt_prefix(b"prompt> echo after\r\n", &mut replayed),
            b"echo after\r\n"
        );
        assert!(replayed.is_none());
    }
}
