//! Submit-time routing for trusted Readline mirrors containing path-bearing
//! natural language (#2913).

use std::io;

use crate::evidence::redact_sensitive_text;
use crate::input::{InterceptReason, PathPromptIntercept};

use super::super::mode::new_delay_input_mode;
use super::super::RawInputEvent;
use super::{send_shell_input_state, write_user_bytes_to_pty, InputRelayContext};

pub(super) fn route_missing_path_submission(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
    pending_shell_submits: usize,
) -> io::Result<bool> {
    // Zsh path candidates remain Rust-owned until Enter. If editing or a
    // control key already returned the line to ZLE, keep it Shell-owned.
    if relay.zsh_path_prompt_buffering.is_some() {
        return Ok(false);
    }
    if !relay.main_prompt_gate.is_path_prompt_ready() || pending_shell_submits > 0 {
        return Ok(false);
    }
    let Some(submit) = bytes.iter().position(|byte| matches!(byte, b'\n' | b'\r')) else {
        return Ok(false);
    };
    // Multiple submissions in one read are paste/read-ahead territory. The
    // first line may change shell state before the remainder is consumed, so
    // keep the whole batch Shell-owned instead of routing against stale state.
    if submit + 1 != bytes.len()
        || bytes[..submit]
            .iter()
            .any(|byte| matches!(byte, 0x00..=0x1f | 0x7f))
    {
        return Ok(false);
    }
    let Some(mirror) = relay.native_line_state.clean_visible_line() else {
        return Ok(false);
    };
    let mut input = mirror.to_vec();
    input.extend_from_slice(&bytes[..submit]);
    let Ok(input) = String::from_utf8(input) else {
        return Ok(false);
    };
    let Some(PathPromptIntercept {
        input,
        reason: InterceptReason::NaturalLanguage,
        cwd,
    }) = relay
        .input_classifier
        .classify_missing_path_submission(&input)
    else {
        return Ok(false);
    };

    if let Ok(mut mode) = relay.input_mode.lock() {
        *mode = new_delay_input_mode();
    }
    let _ = relay
        .input_events
        .send(RawInputEvent::SyntheticPromptRepaint);
    // Bytes typed in this read have not reached Readline yet. Write them
    // before Ctrl-U so Bash echoes the exact user submission, then accept
    // the cleared line to repaint PS1. The command itself never executes.
    let mut repaint = Vec::with_capacity(submit + 2);
    repaint.extend_from_slice(&bytes[..submit]);
    repaint.extend_from_slice(b"\x15\r");
    write_user_bytes_to_pty(
        relay.master,
        relay.input_generation,
        relay.line_submits,
        relay.input_events,
        relay.main_prompt_gate,
        &repaint,
    )?;
    relay.native_line_state.clear();
    let sensitive = redact_sensitive_text(&input).1;
    let _ = relay
        .input_events
        .send(RawInputEvent::NativePathPromptIntercept {
            input,
            cwd,
            sensitive,
        });
    send_shell_input_state(true, relay.input_events);
    Ok(true)
}

pub(super) fn route_candidate_missing_path_submission(
    input: &str,
    relay: &mut InputRelayContext<'_>,
    pending_shell_submits: usize,
    has_remainder: bool,
) -> bool {
    if !relay.main_prompt_gate.is_path_prompt_ready() || pending_shell_submits > 0 || has_remainder
    {
        return false;
    }
    if relay.zsh_path_prompt_buffering.is_some()
        && !relay
            .input_classifier
            .shell_path_command_names()
            .excludes_first_token(input)
    {
        return false;
    }
    let Some(PathPromptIntercept {
        input,
        reason: InterceptReason::NaturalLanguage,
        cwd,
    }) = relay
        .input_classifier
        .classify_missing_path_submission(input)
    else {
        return false;
    };

    let _ = relay.input_events.send(RawInputEvent::CandidateCommit(
        super::redact_extension_setting_value(input.as_bytes()),
    ));
    if let Ok(mut mode) = relay.input_mode.lock() {
        *mode = new_delay_input_mode();
    }
    let sensitive = redact_sensitive_text(&input).1;
    let event = if input.starts_with('/') {
        RawInputEvent::UserInterceptWithRouting {
            input,
            reason: InterceptReason::NaturalLanguage,
            cwd,
            sensitive,
        }
    } else {
        RawInputEvent::NativePathPromptIntercept {
            input,
            cwd,
            sensitive,
        }
    };
    let _ = relay.input_events.send(event);
    send_shell_input_state(true, relay.input_events);
    true
}
