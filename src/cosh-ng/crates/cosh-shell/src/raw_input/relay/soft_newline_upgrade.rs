//! Prompt-line soft-newline handling on the passthrough path (#1932 F6).
//!
//! With modifyOtherKeys negotiated the terminal emits whitelisted
//! soft-newline sequences on any line. On a bash-owned main-prompt line the
//! keypress itself is an explicit multi-line intent: when the observed line
//! mirror is trusted, the line upgrades into the prompt-draft card
//! (readline's copy is cleared with Ctrl-U). Otherwise the sequence is
//! stripped so bash never echoes the CSI tail as literal garbage.

use std::io;

use super::super::soft_newline::{
    first_soft_newline_position, soft_newline_sequence_len, strip_soft_newline_sequences,
};
use super::super::RawInputEvent;
use super::{send_shell_input_state, write_user_bytes_to_pty, InputRelayContext};

pub(super) enum PromptLineSoftNewline {
    /// No gated sequence in this chunk: relay the bytes untouched.
    Passthrough,
    /// The line upgraded into the draft card; the chunk is fully consumed.
    Upgraded,
    /// Fallback: sequences removed, relay the remaining bytes.
    Stripped(Vec<u8>),
}

pub(super) fn handle_prompt_line_soft_newline(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
) -> io::Result<PromptLineSoftNewline> {
    // Gate down: a running command or continuation owns the tty (heredoc,
    // vim) and may understand the sequence itself; bytes pass untouched.
    if !relay.main_prompt_gate.is_at_prompt() {
        return Ok(PromptLineSoftNewline::Passthrough);
    }
    let Some(position) = first_soft_newline_position(bytes) else {
        return Ok(PromptLineSoftNewline::Passthrough);
    };
    let sequence_len = soft_newline_sequence_len(&bytes[position..]).unwrap_or(0);
    let rest = &bytes[position + sequence_len..];
    // The chunk prefix joins the draft verbatim, so it must be plain text
    // itself: any control byte (Tab, backspace, ESC) means readline edited
    // the line where the mirror cannot follow.
    let prefix_is_plain = bytes[..position]
        .iter()
        .all(|byte| !matches!(byte, 0x00..=0x1f | 0x7f));
    if rest.is_empty() && prefix_is_plain {
        if let Some(mirror) = relay.native_line_state.clean_visible_line() {
            // Upgrade: the draft carries the observed line plus any bytes
            // of this chunk typed before the shortcut; the trailing newline
            // is the Shift+Enter itself (cursor lands on line two). An
            // empty line opens an empty card, same as `??` + Enter.
            let mut draft = mirror.to_vec();
            draft.extend_from_slice(&bytes[..position]);
            let text = if draft.is_empty() {
                String::new()
            } else {
                let mut text = String::from_utf8_lossy(&draft).into_owned();
                text.push('\n');
                text
            };
            // Clear readline's copy and accept the now-empty line: the
            // Enter makes bash paint a fresh PS1 (precmd), exactly like the
            // ??-Enter intercept path, so the post-turn RestorePrompt has a
            // prompt to release; with a bare Ctrl-U bash never repaints and
            // the prompt stays missing after the agent reply (#1932).
            // Arm the blank-echo drop before the PTY write: the event then
            // provably precedes the accept echo in channel order, and the
            // output loop re-drains events after each PTY read, so the
            // prompt boundary can never outrun the arm (review race).
            let _ = relay
                .input_events
                .send(RawInputEvent::SyntheticPromptRepaint);
            write_user_bytes_to_pty(
                relay.master,
                relay.input_generation,
                relay.line_submits,
                relay.input_events,
                relay.main_prompt_gate,
                b"\x15\r",
            )?;
            relay.native_line_state.clear();
            let _ = relay
                .input_events
                .send(RawInputEvent::PromptDraftOpen { text });
            send_shell_input_state(true, relay.input_events);
            return Ok(PromptLineSoftNewline::Upgraded);
        }
    }
    // Fail-closed fallback (dirty mirror or trailing bytes): strip the
    // sequences, the one-time discoverability tip still educates.
    match strip_soft_newline_sequences(bytes) {
        Some(stripped) => Ok(PromptLineSoftNewline::Stripped(stripped)),
        None => Ok(PromptLineSoftNewline::Passthrough),
    }
}
