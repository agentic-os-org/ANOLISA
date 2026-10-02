//! Candidate-line routing after a complete prompt-owned input is available.

use std::io;

use crate::input::{InputDecision, InterceptReason};

use super::super::event_parser::{
    candidate_line_status, redact_extension_setting_value, CandidateLineStatus,
    BRACKETED_PASTE_END, BRACKETED_PASTE_START,
};
use super::super::mode::new_delay_input_mode;
use super::super::RawInputEvent;
use super::{
    flush_candidate_line_to_shell, relay_passthrough_input_with_policy, send_shell_input_state,
    submit_line_bytes_to_shell, InputRelayContext, ShellBatchOwnership,
};

pub(super) fn relay_candidate_line(
    relay: &mut InputRelayContext<'_>,
    emit_activity: bool,
    pending_shell_submits: usize,
) -> io::Result<bool> {
    // A bracketed paste is still streaming: defer every routing decision
    // (submit, flush, card upgrade) until the closer arrives so embedded
    // newlines can never execute early (#1721).
    if relay.line_buffer.in_paste() {
        return Ok(true);
    }
    if relay.line_buffer.saw_paste()
        && !relay.line_buffer.soft_newline_enabled
        && !relay.line_buffer.force_agent_intercept
    {
        if let Some(closed_at) = relay.line_buffer.paste_closed_at() {
            let closed_at = closed_at.min(relay.line_buffer.bytes.len());
            let payload = &relay.line_buffer.bytes[..closed_at];
            let after_paste = &relay.line_buffer.bytes[closed_at..];
            let trimmed = payload
                .strip_suffix(b"\r\n")
                .or_else(|| payload.strip_suffix(b"\n"))
                .or_else(|| payload.strip_suffix(b"\r"))
                .unwrap_or(payload);
            let pasted_slash = (!trimmed.contains(&b'\n')
                && !trimmed.contains(&b'\r')
                && std::str::from_utf8(trimmed).ok().is_some_and(|line| {
                    matches!(
                        relay.input_classifier.classify(line),
                        InputDecision::Intercept {
                            reason: InterceptReason::Slash,
                            ..
                        }
                    )
                }))
            .then(|| String::from_utf8_lossy(trimmed).into_owned());
            let submission = after_paste
                .iter()
                .position(|byte| matches!(byte, b'\n' | b'\r'));
            if pasted_slash.is_some() && submission.is_none() {
                return Ok(true);
            }

            let mut bytes = relay.line_buffer.take();
            let after_paste = bytes.split_off(closed_at);
            if let (Some(input), Some(0)) = (pasted_slash, submission) {
                let remainder = after_paste.get(1..).unwrap_or_default().to_vec();
                let _ = relay.input_events.send(RawInputEvent::CandidateCommit(
                    redact_extension_setting_value(input.as_bytes()),
                ));
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = new_delay_input_mode();
                }
                let _ = relay
                    .input_events
                    .send(RawInputEvent::UserIntercept(input, InterceptReason::Slash));
                send_shell_input_state(true, relay.input_events);
                if !remainder.is_empty() {
                    relay_passthrough_input_with_policy(
                        &remainder,
                        relay,
                        emit_activity,
                        pending_shell_submits,
                    )?;
                }
                return Ok(true);
            }

            let mut wrapped = Vec::with_capacity(bytes.len() + 12);
            wrapped.extend_from_slice(BRACKETED_PASTE_START);
            wrapped.extend_from_slice(&bytes);
            wrapped.extend_from_slice(BRACKETED_PASTE_END);
            return submit_line_bytes_to_shell(
                relay,
                wrapped,
                after_paste,
                emit_activity,
                ShellBatchOwnership::Opaque,
            );
        }
    }
    match candidate_line_status(
        &relay.line_buffer.bytes,
        relay.line_buffer.soft_newline_enabled,
    ) {
        CandidateLineStatus::Pending => Ok(true),
        CandidateLineStatus::Unsafe if relay.line_buffer.force_agent_intercept => {
            relay.line_buffer.clear();
            let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
            let _ = relay.input_events.send(RawInputEvent::PromptGhostDismissed);
            send_shell_input_state(true, relay.input_events);
            Ok(true)
        }
        CandidateLineStatus::Unsafe => {
            flush_candidate_line_to_shell(relay, emit_activity, pending_shell_submits)
        }
        CandidateLineStatus::Complete { line, line_len } => {
            let force_agent_intercept = relay.line_buffer.force_agent_intercept;
            let suggestion_id = relay.line_buffer.forced_agent_suggestion_id.clone();
            let saw_paste = relay.line_buffer.saw_paste();
            let mut bytes = relay.line_buffer.take();
            let remainder = bytes.split_off(line_len);
            if force_agent_intercept {
                let _ = relay.input_events.send(RawInputEvent::CandidateCommit(
                    redact_extension_setting_value(line.as_bytes()),
                ));
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = new_delay_input_mode();
                }
                let _ = relay
                    .input_events
                    .send(RawInputEvent::PromptGhostIntercept {
                        input: line,
                        suggestion_id,
                    });
                send_shell_input_state(true, relay.input_events);
                if !remainder.is_empty() {
                    relay_passthrough_input_with_policy(
                        &remainder,
                        relay,
                        emit_activity,
                        pending_shell_submits,
                    )?;
                }
                return Ok(true);
            }
            if line.contains('\n') {
                if line.trim().is_empty() {
                    let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
                    send_shell_input_state(true, relay.input_events);
                } else {
                    let _ = relay.input_events.send(RawInputEvent::CandidateCommit(
                        redact_extension_setting_value(line.as_bytes()),
                    ));
                    if let Ok(mut mode) = relay.input_mode.lock() {
                        *mode = new_delay_input_mode();
                    }
                    let _ = relay.input_events.send(RawInputEvent::UserIntercept(
                        line,
                        InterceptReason::AgentMarker,
                    ));
                    send_shell_input_state(true, relay.input_events);
                }
                if !remainder.is_empty() {
                    relay_passthrough_input_with_policy(
                        &remainder,
                        relay,
                        emit_activity,
                        pending_shell_submits,
                    )?;
                }
                return Ok(true);
            }
            if line.trim() == "??" {
                // A lone `??` submit opens an empty prompt draft (#1932):
                // the terminal-agnostic entry into multi-line composition,
                // mirroring the soft-newline upgrade in redraw_candidate_line.
                let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
                let _ = relay.input_events.send(RawInputEvent::PromptDraftOpen {
                    text: String::new(),
                });
                send_shell_input_state(true, relay.input_events);
                if !remainder.is_empty() {
                    relay_passthrough_input_with_policy(
                        &remainder,
                        relay,
                        emit_activity,
                        pending_shell_submits,
                    )?;
                }
                return Ok(true);
            }
            match relay.input_classifier.classify(&line) {
                InputDecision::Intercept { input, reason } => {
                    if reason == InterceptReason::Slash
                        && relay.slash_route_enabled
                        && relay.main_prompt_gate.is_at_prompt()
                        && relay.input_classifier.is_exact_slash_control_command(
                            line.split_whitespace().next().unwrap_or_default(),
                        )
                    {
                        // Submit the exact slash line through bash so
                        // Readline records it in native history (#1718).
                        // Once accepted by Bash it remains Shell-owned; the
                        // bounded marker only observes its command boundary.
                        return submit_line_bytes_to_shell(
                            relay,
                            bytes,
                            remainder,
                            emit_activity,
                            ShellBatchOwnership::ReadlineSafe,
                        );
                    }
                    let _ = relay.input_events.send(RawInputEvent::CandidateCommit(
                        redact_extension_setting_value(line.as_bytes()),
                    ));
                    if let Ok(mut mode) = relay.input_mode.lock() {
                        *mode = new_delay_input_mode();
                    }
                    let event = if pending_shell_submits > 0 {
                        RawInputEvent::UserInterceptAtPrompt {
                            input,
                            reason,
                            pending_submits: pending_shell_submits,
                        }
                    } else {
                        RawInputEvent::UserIntercept(input, reason)
                    };
                    let _ = relay.input_events.send(event);
                    send_shell_input_state(true, relay.input_events);
                    if !remainder.is_empty() {
                        relay_passthrough_input_with_policy(
                            &remainder,
                            relay,
                            emit_activity,
                            pending_shell_submits,
                        )?;
                    }
                    Ok(true)
                }
                InputDecision::SendToShell(_) => {
                    if !saw_paste
                        && super::path_prompt_submit::route_candidate_missing_path_submission(
                            &line,
                            relay,
                            pending_shell_submits,
                            !remainder.is_empty(),
                        )
                    {
                        return Ok(true);
                    }
                    let mut bytes = bytes;
                    let mut remainder = remainder;
                    if saw_paste {
                        // Replay the whole pasted region as one bracketed
                        // paste: bash inserts the bytes (incl. embedded
                        // newlines) and waits for explicit Enter (#1721).
                        bytes.extend_from_slice(&remainder);
                        remainder = Vec::new();
                        let mut wrapped = Vec::with_capacity(bytes.len() + 12);
                        wrapped.extend_from_slice(b"\x1b[200~");
                        wrapped.extend_from_slice(&bytes);
                        wrapped.extend_from_slice(b"\x1b[201~");
                        bytes = wrapped;
                    }
                    submit_line_bytes_to_shell(
                        relay,
                        bytes,
                        remainder,
                        emit_activity,
                        ShellBatchOwnership::Opaque,
                    )
                }
                InputDecision::Consume => {
                    let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
                    send_shell_input_state(true, relay.input_events);
                    if !remainder.is_empty() {
                        relay_passthrough_input_with_policy(
                            &remainder,
                            relay,
                            emit_activity,
                            pending_shell_submits,
                        )?;
                    }
                    Ok(false)
                }
            }
        }
    }
}
