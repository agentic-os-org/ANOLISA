use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};

use crate::input::{InputClassifier, InterceptReason};

use super::event_parser::{
    candidate_inline_hint, native_candidate_allows_soft_newline,
    native_candidate_should_return_to_shell, redact_extension_setting_value,
    starts_native_intercept_candidate, CandidateLineBuffer, NativeLineState, BRACKETED_PASTE_END,
    BRACKETED_PASTE_START,
};
use super::event_sender::RawInputEventSink;
use super::generation::{LineSubmitCounter, UserPtyInputGeneration};
use super::mode::new_delay_input_mode;
use super::path_prompt_candidate::zsh_path_candidate_should_hold;
use super::soft_newline::{
    contains_soft_newline_sequence, draft_text_from_bytes, render_soft_newline_markers,
};
use super::spawn::ZshPathPromptBuffering;
use super::{write_all_pty, MainPromptGate, PromptGhostRoute, RawInputEvent, RawInputMode, CTRL_C};

pub(super) struct InputRelayContext<'a> {
    pub(super) master: &'a mut File,
    pub(super) input_classifier: &'a InputClassifier,
    pub(super) input_events: &'a dyn RawInputEventSink,
    pub(super) input_mode: &'a Arc<Mutex<RawInputMode>>,
    pub(super) input_generation: &'a UserPtyInputGeneration,
    pub(super) line_submits: &'a mut LineSubmitCounter,
    pub(super) line_buffer: &'a mut CandidateLineBuffer,
    pub(super) native_line_state: &'a mut NativeLineState,
    pub(super) exit_tracker: &'a mut ExplicitExitTracker,
    pub(super) main_prompt_gate: &'a MainPromptGate,
    /// Routes exact slash-control submissions through bash so they enter
    /// native history (issue #1718); admission additionally requires the
    /// main prompt gate, so any submission (slash or shell) lowers the gate
    /// until the next prompt_ready marker and later bytes fall back to the
    /// Rust intercept path instead of leaking into a foreground process.
    pub(super) slash_route_enabled: bool,
    /// Zsh-only capability that keeps slash candidates out of ZLE until Rust
    /// can route or submit the complete line.
    pub(super) zsh_path_prompt_buffering: Option<&'a mut ZshPathPromptBuffering>,
}

/// Writes real user bytes to the PTY, bumping the shared input generation
/// first so replayed-prompt state armed for an older generation expires
/// before the resulting PTY output can be parsed. The event also reports how
/// many line submissions the write carried, so the output loop can match
/// them against shell prompt boundaries.
pub(super) fn write_user_bytes_to_pty(
    master: &mut File,
    input_generation: &UserPtyInputGeneration,
    line_submits: &mut LineSubmitCounter,
    input_events: &dyn RawInputEventSink,
    main_prompt_gate: &MainPromptGate,
    bytes: &[u8],
) -> io::Result<()> {
    let line_submits = line_submits.count(bytes);
    if line_submits > 0 {
        // A submitted line leaves the primary prompt until the marker emits
        // the next prompt_ready (#1721 D16).
        main_prompt_gate.set_at_prompt(false);
    }
    let generation = input_generation.bump();
    let _ = input_events.send(RawInputEvent::PtyUserWrite {
        generation,
        line_submits,
    });
    write_all_pty(master, bytes)
}

pub(super) fn send_raw_input_events(bytes: &[u8], input_events: &dyn RawInputEventSink) {
    if bytes.contains(&CTRL_C) {
        let _ = input_events.send(RawInputEvent::CtrlC);
    }
}

pub(super) fn send_shell_input_state(empty: bool, input_events: &dyn RawInputEventSink) {
    let _ = input_events.send(RawInputEvent::ShellInputActivity { empty });
}

fn observe_native_line(
    state: &mut NativeLineState,
    bytes: &[u8],
    input_events: &dyn RawInputEventSink,
) {
    state.observe_shell_bytes(bytes);
    if state.take_multiline_paste_observed() {
        let _ = input_events.send(RawInputEvent::MultilinePasteObserved);
    }
}

pub(super) fn relay_passthrough_input(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
) -> io::Result<bool> {
    relay_passthrough_input_with_activity(bytes, relay, true)
}

pub(super) fn relay_passthrough_input_after_shell_submits(
    bytes: &[u8],
    pending_shell_submits: usize,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<bool> {
    relay_passthrough_input_with_policy(bytes, relay, true, pending_shell_submits)
}

fn relay_passthrough_input_with_activity(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
    emit_activity: bool,
) -> io::Result<bool> {
    relay_passthrough_input_with_policy(bytes, relay, emit_activity, 0)
}

fn relay_passthrough_input_with_policy(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
    emit_activity: bool,
    pending_shell_submits: usize,
) -> io::Result<bool> {
    if relay
        .zsh_path_prompt_buffering
        .as_deref()
        .is_some_and(ZshPathPromptBuffering::has_deferred_tab_typeahead)
    {
        if let Some(buffering) = relay.zsh_path_prompt_buffering.as_deref_mut() {
            buffering.defer_tab_typeahead(bytes);
        }
        return Ok(true);
    }
    if relay.line_buffer.force_agent_intercept && relay.line_buffer.is_active() {
        relay.line_buffer.soft_newline_enabled = true;
        relay.line_buffer.push(bytes);
        if !relay.line_buffer.force_agent_intercept {
            let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
            let _ = relay.input_events.send(RawInputEvent::PromptGhostDismissed);
            if !relay.line_buffer.is_active() {
                send_shell_input_state(true, relay.input_events);
                return Ok(true);
            }
            redraw_candidate_line(relay.input_events, relay.line_buffer);
            return candidate::relay_candidate_line(relay, emit_activity, pending_shell_submits);
        }
        if !relay.line_buffer.is_active() {
            relay.line_buffer.clear();
            let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
            let _ = relay.input_events.send(RawInputEvent::PromptGhostDismissed);
            send_shell_input_state(true, relay.input_events);
            return Ok(true);
        }
        redraw_candidate_line(relay.input_events, relay.line_buffer);
        return candidate::relay_candidate_line(relay, emit_activity, pending_shell_submits);
    }
    relay_native_passthrough(bytes, relay, emit_activity, pending_shell_submits)
}

pub(super) fn replay_deferred_zsh_tab_typeahead(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    // A separate relay turn gives ZLE time to finish the Tab widget. Cancel
    // its potentially rewritten buffer before replaying preserved input as
    // the next logical line.
    relay_native_passthrough(&[CTRL_C], relay, true, 0)?;
    relay_passthrough_input_with_policy(bytes, relay, true, 0)?;
    Ok(())
}

pub(super) fn relay_prompt_ghost_input(
    bytes: &[u8],
    ghost_text: &str,
    route: &PromptGhostRoute,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<bool> {
    if bytes.starts_with(b"\x1b[Z") {
        if let PromptGhostRoute::AgentSelection {
            candidates, active, ..
        } = route
        {
            if candidates.len() > 1 {
                let next = (active + 1) % candidates.len();
                let candidate = &candidates[next];
                let next_route = PromptGhostRoute::AgentSelection {
                    candidates: candidates.clone(),
                    active: next,
                };
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = RawInputMode::PromptGhost {
                        text: candidate.text.clone(),
                        route: next_route.clone(),
                    };
                }
                let _ = relay.input_events.send(RawInputEvent::PromptGhostCycle {
                    text: candidate.text.clone(),
                });
                let remainder = &bytes[3..];
                if !remainder.is_empty() {
                    return relay_prompt_ghost_input(
                        remainder,
                        &candidate.text,
                        &next_route,
                        relay,
                    );
                }
                return Ok(true);
            }
        }
        return match bytes.len() {
            3 => Ok(true),
            _ => relay_prompt_ghost_input(&bytes[3..], ghost_text, route, relay),
        };
    }
    if matches!(bytes.first(), Some(b'\r' | b'\n')) {
        if let PromptGhostRoute::AgentSelection {
            candidates, active, ..
        } = route
        {
            if let Some(candidate) = candidates.get(*active) {
                let _ = relay.input_events.send(RawInputEvent::PromptGhostClear);
                let _ = relay.input_events.send(RawInputEvent::CandidateCommit(
                    candidate.text.as_bytes().to_vec(),
                ));
                let _ = relay
                    .input_events
                    .send(RawInputEvent::PromptGhostIntercept {
                        input: candidate.text.clone(),
                        suggestion_id: Some(candidate.suggestion_id.clone()),
                    });
                send_shell_input_state(true, relay.input_events);
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = new_delay_input_mode();
                }
                return Ok(true);
            }
        }
    }
    if bytes.starts_with(b"\t") && !relay.line_buffer.is_active() {
        let _ = relay.input_events.send(RawInputEvent::PromptGhostClear);
        let remainder = &bytes[1..];
        match route {
            PromptGhostRoute::NativeShell => {
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = RawInputMode::RawPassthrough;
                }
                observe_native_line(
                    relay.native_line_state,
                    ghost_text.as_bytes(),
                    relay.input_events,
                );
                relay
                    .exit_tracker
                    .observe_shell_bytes(ghost_text.as_bytes());
                write_user_bytes_to_pty(
                    relay.master,
                    relay.input_generation,
                    relay.line_submits,
                    relay.input_events,
                    relay.main_prompt_gate,
                    ghost_text.as_bytes(),
                )?;
                if !remainder.is_empty() {
                    send_raw_input_events(remainder, relay.input_events);
                    observe_native_line(relay.native_line_state, remainder, relay.input_events);
                    relay.exit_tracker.observe_shell_bytes(remainder);
                    write_user_bytes_to_pty(
                        relay.master,
                        relay.input_generation,
                        relay.line_submits,
                        relay.input_events,
                        relay.main_prompt_gate,
                        remainder,
                    )?;
                }
            }
            PromptGhostRoute::AgentIntercept { suggestion_id } => {
                let _ = relay.input_events.send(RawInputEvent::PromptGhostAccepted {
                    suggestion_id: suggestion_id.clone(),
                });
                relay.line_buffer.soft_newline_enabled = true;
                relay.line_buffer.push(ghost_text.as_bytes());
                relay.line_buffer.force_agent_intercept = true;
                relay.line_buffer.forced_agent_suggestion_id = suggestion_id.clone();
                redraw_candidate_line(relay.input_events, relay.line_buffer);
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = RawInputMode::Passthrough;
                }
                if !remainder.is_empty() {
                    relay_passthrough_input(remainder, relay)?;
                }
            }
            PromptGhostRoute::AgentSelection {
                candidates, active, ..
            } => {
                let suggestion_id = candidates
                    .get(*active)
                    .map(|candidate| candidate.suggestion_id.clone());
                let _ = relay.input_events.send(RawInputEvent::PromptGhostAccepted {
                    suggestion_id: suggestion_id.clone(),
                });
                relay.line_buffer.soft_newline_enabled = true;
                relay.line_buffer.push(ghost_text.as_bytes());
                relay.line_buffer.force_agent_intercept = true;
                relay.line_buffer.forced_agent_suggestion_id = suggestion_id;
                redraw_candidate_line(relay.input_events, relay.line_buffer);
                if let Ok(mut mode) = relay.input_mode.lock() {
                    *mode = RawInputMode::Passthrough;
                }
                if !remainder.is_empty() {
                    relay_passthrough_input(remainder, relay)?;
                }
            }
        }
        return Ok(true);
    }
    dismiss_prompt_ghost_input(bytes, relay)
}

pub(super) fn dismiss_prompt_ghost_input(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
) -> io::Result<bool> {
    if let Ok(mut mode) = relay.input_mode.lock() {
        *mode = RawInputMode::Passthrough;
    }
    let _ = relay.input_events.send(RawInputEvent::PromptGhostClear);
    let _ = relay.input_events.send(RawInputEvent::PromptGhostDismissed);
    relay_passthrough_input(bytes, relay)
}

pub(super) fn send_held_input_events(bytes: &[u8], input_events: &dyn RawInputEventSink) {
    send_raw_input_events(bytes, input_events);
    if held_input_requests_cancel(bytes) {
        let _ = input_events.send(RawInputEvent::CtrlC);
    }
}

pub(super) fn relay_delayed_input(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    if bytes.contains(&CTRL_C) {
        let _ = relay.input_events.send(RawInputEvent::CtrlC);
        relay.line_buffer.clear();
        relay.native_line_state.clear();
        send_shell_input_state(true, relay.input_events);
        return Ok(());
    }
    if relay_passthrough_input_with_activity(bytes, relay, false)? {
        return Ok(());
    }
    Ok(())
}

fn relay_native_passthrough(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
    emit_activity: bool,
    pending_shell_submits: usize,
) -> io::Result<bool> {
    let starts_paste = bytes.starts_with(BRACKETED_PASTE_START)
        || (bytes.len() >= 2
            && bytes.len() < BRACKETED_PASTE_START.len()
            && BRACKETED_PASTE_START.starts_with(bytes));
    let complete_paste = bytes.starts_with(BRACKETED_PASTE_START);
    if relay.line_buffer.is_active()
        || complete_paste
        || starts_native_intercept_candidate(
            bytes,
            relay.native_line_state,
            relay.zsh_path_prompt_buffering.is_some(),
        )
    {
        if let Some(handled) = tab_handoff::relay_coalesced_zsh_tab(
            bytes,
            relay,
            emit_activity,
            pending_shell_submits,
            complete_paste,
        )? {
            return Ok(handled);
        }
        // Route flags must consider the whole draft so far: a bracketed
        // paste opener may arrive as its own chunk (or split mid-delimiter,
        // #1721) before the payload decides CJK vs slash (#1721 D13).
        let combined: Vec<u8> = [
            relay.line_buffer.bytes.as_slice(),
            relay.line_buffer.pending_partial_bytes(),
            bytes,
        ]
        .concat();
        relay.line_buffer.soft_newline_enabled = native_candidate_allows_soft_newline(&combined);
        relay.line_buffer.push(bytes);
        let zsh_path_candidate = relay.zsh_path_prompt_buffering.is_some();
        let hold_zsh_path_candidate = zsh_path_candidate
            && !relay.line_buffer.saw_paste()
            && zsh_path_candidate_should_hold(&relay.line_buffer.bytes)
            && !relay.line_buffer.bytes.contains(&b'\t');
        let return_resolved_zsh_space_prefix = zsh_path_candidate
            && relay.line_buffer.bytes.first() == Some(&b' ')
            && !hold_zsh_path_candidate;
        if !relay.line_buffer.in_paste()
            && !hold_zsh_path_candidate
            && (return_resolved_zsh_space_prefix
                || native_candidate_should_return_to_shell(
                    relay.input_classifier,
                    relay.line_buffer,
                ))
        {
            return flush_candidate_line_to_shell(relay, emit_activity, pending_shell_submits);
        }
        // Control bytes such as Tab must reach readline without first changing
        // the outer terminal cursor, whose display width differs from byte count.
        redraw_candidate_line(relay.input_events, relay.line_buffer);
        return candidate::relay_candidate_line(relay, emit_activity, pending_shell_submits);
    }
    if !starts_paste {
        if path_prompt_submit::route_missing_path_submission(bytes, relay, pending_shell_submits)? {
            return Ok(true);
        }
        if let Some(submit) = bytes.iter().position(|byte| matches!(byte, b'\n' | b'\r')) {
            let line_end = submit + 1;
            if line_end < bytes.len() {
                let (line, remainder) = bytes.split_at(line_end);
                let submitted_nonempty = !relay.native_line_state.is_empty()
                    || line[..submit]
                        .iter()
                        .any(|byte| !matches!(byte, b'\n' | b'\r'));
                relay_native_passthrough(line, relay, emit_activity, pending_shell_submits)?;
                return relay_passthrough_input_with_policy(
                    remainder,
                    relay,
                    emit_activity,
                    pending_shell_submits.saturating_add(usize::from(submitted_nonempty)),
                );
            }
        }
    }
    // Non-slash input: send directly to PTY. Shell marker's preexec/
    // command_not_found hooks handle NL/CJK intercept on the shell side.
    // Same soft-newline handling as the escape path (#1932 F6).
    let handled = handle_prompt_line_soft_newline(bytes, relay)?;
    if matches!(handled, PromptLineSoftNewline::Upgraded) {
        return Ok(true);
    }
    observe_passthrough_soft_newline(bytes, relay.input_events);
    let bytes = match &handled {
        PromptLineSoftNewline::Stripped(stripped) => stripped.as_slice(),
        _ => bytes,
    };
    let private_history = history_private_submission(
        relay
            .input_classifier
            .bash_readline_history_privacy_enabled(),
        relay.native_line_state,
        relay.line_submits,
        relay.main_prompt_gate.is_at_prompt(),
        bytes,
    );
    let guard_submission = bash_submission_needs_guard(
        relay.input_classifier.bash_slash_submission_guard_enabled(),
        relay.main_prompt_gate.is_at_prompt(),
        relay.input_classifier,
        relay.native_line_state,
        relay.line_submits,
        bytes,
    ) || bash_submission_has_leading_whitespace(
        relay.input_classifier.bash_slash_submission_guard_enabled(),
        relay.main_prompt_gate.is_at_prompt(),
        relay.native_line_state,
        relay.line_submits,
        bytes,
    );
    send_raw_input_events(bytes, relay.input_events);
    observe_native_line(relay.native_line_state, bytes, relay.input_events);
    if emit_activity && !bytes.is_empty() {
        send_shell_input_state(relay.native_line_state.is_empty(), relay.input_events);
    }
    relay.exit_tracker.observe_shell_bytes(bytes);
    let pty_bytes = private_history
        .as_ref()
        .map(|submission| submission.bytes.as_slice())
        .unwrap_or(bytes);
    let guarded_bytes = guarded_bash_submission(
        guard_submission || private_history.is_some(),
        private_history.is_some(),
        private_history
            .as_ref()
            .is_some_and(|submission| submission.recoverable),
        relay.input_classifier,
        relay.line_submits,
        pty_bytes,
    );
    write_user_bytes_to_pty(
        relay.master,
        relay.input_generation,
        relay.line_submits,
        relay.input_events,
        relay.main_prompt_gate,
        guarded_bytes.as_deref().unwrap_or(pty_bytes),
    )?;
    Ok(false)
}

fn flush_candidate_line_to_shell(
    relay: &mut InputRelayContext<'_>,
    emit_activity: bool,
    pending_shell_submits: usize,
) -> io::Result<bool> {
    let saw_paste = relay.line_buffer.saw_paste();
    let paste_closed_at = relay.line_buffer.paste_closed_at();
    if !saw_paste {
        let candidate = {
            let bytes = relay.line_buffer.bytes.as_slice();
            bytes
                .iter()
                .position(|byte| matches!(byte, b'\n' | b'\r'))
                .and_then(|submit| {
                    let submit_end = if bytes.get(submit) == Some(&b'\r')
                        && bytes.get(submit + 1) == Some(&b'\n')
                    {
                        submit + 2
                    } else {
                        submit + 1
                    };
                    (submit_end == bytes.len())
                        .then(|| std::str::from_utf8(&bytes[..submit]).ok())
                        .flatten()
                        .map(str::to_string)
                })
        };
        if candidate.as_deref().is_some_and(|input| {
            path_prompt_submit::route_candidate_missing_path_submission(
                input,
                relay,
                pending_shell_submits,
                false,
            )
        }) {
            relay.line_buffer.clear();
            return Ok(true);
        }
    }
    let mut bytes = relay.line_buffer.take();
    if saw_paste {
        // Replay only paste payload; bytes after the closer are Readline input (#1721).
        let closed_at = paste_closed_at.unwrap_or(bytes.len()).min(bytes.len());
        let after_paste = bytes.split_off(closed_at);
        let mut wrapped = Vec::with_capacity(bytes.len() + 12);
        wrapped.extend_from_slice(BRACKETED_PASTE_START);
        wrapped.extend_from_slice(&bytes);
        wrapped.extend_from_slice(BRACKETED_PASTE_END);
        wrapped.extend_from_slice(&after_paste);
        bytes = wrapped;
    }
    submit_line_bytes_to_shell(
        relay,
        bytes,
        Vec::new(),
        emit_activity,
        ShellBatchOwnership::ReadlineSafe,
    )
}

#[derive(Clone, Copy)]
pub(super) enum ShellBatchOwnership {
    Opaque,
    ReadlineSafe,
}

/// Clears the cosh-echoed candidate line and writes the whole read batch to
/// the PTY. Shared by SendToShell submissions, unsafe candidate flushes, and
/// shell-routed slash submissions (issue #1718).
fn submit_line_bytes_to_shell(
    relay: &mut InputRelayContext<'_>,
    mut bytes: Vec<u8>,
    remainder: Vec<u8>,
    emit_activity: bool,
    ownership: ShellBatchOwnership,
) -> io::Result<bool> {
    // A remainder was read with the candidate submission. Once any line in a
    // read is Shell-owned, the complete batch stays Shell-owned even if Bash
    // paints another prompt before this function returns.
    bytes.extend_from_slice(&remainder);
    let _ = relay.input_events.send(RawInputEvent::CandidateClearLine);
    let readline_safe = matches!(ownership, ShellBatchOwnership::ReadlineSafe);
    let guard_submission = readline_safe
        && (bash_submission_needs_guard(
            relay.input_classifier.bash_slash_submission_guard_enabled(),
            relay.main_prompt_gate.is_at_prompt(),
            relay.input_classifier,
            relay.native_line_state,
            relay.line_submits,
            &bytes,
        ) || bash_submission_has_leading_whitespace(
            relay.input_classifier.bash_slash_submission_guard_enabled(),
            relay.main_prompt_gate.is_at_prompt(),
            relay.native_line_state,
            relay.line_submits,
            &bytes,
        ));
    send_raw_input_events(&bytes, relay.input_events);
    observe_native_line(relay.native_line_state, &bytes, relay.input_events);
    if emit_activity && !bytes.is_empty() {
        send_shell_input_state(relay.native_line_state.is_empty(), relay.input_events);
    }
    relay.exit_tracker.observe_shell_bytes(&bytes);
    let guarded_bytes = matches!(ownership, ShellBatchOwnership::ReadlineSafe)
        .then(|| {
            guarded_bash_submission(
                guard_submission,
                false,
                false,
                relay.input_classifier,
                relay.line_submits,
                &bytes,
            )
        })
        .flatten();
    write_user_bytes_to_pty(
        relay.master,
        relay.input_generation,
        relay.line_submits,
        relay.input_events,
        relay.main_prompt_gate,
        guarded_bytes.as_deref().unwrap_or(&bytes),
    )?;
    Ok(false)
}

fn redraw_candidate_line(
    input_events: &dyn RawInputEventSink,
    line_buffer: &mut CandidateLineBuffer,
) {
    let original = line_buffer.visible_line_bytes();
    send_shell_input_state(original.is_empty(), input_events);
    if line_buffer.soft_newline_enabled
        && !line_buffer.in_paste()
        && contains_soft_newline_sequence(original)
    {
        // First soft newline upgrades the draft into the prompt card
        // (#1721 D13): erase the inline echo, hand the buffered text to the
        // runtime, and let the capture own every following keystroke. The
        // leading `??` agent-marker is a routing gesture, not content
        // (#1932): strip it so the card opens with the prompt itself.
        let text = draft_text_from_bytes(original);
        let text = match text.strip_prefix("??") {
            Some(rest) => rest.trim_start_matches(' ').to_string(),
            None => text,
        };
        let _ = input_events.send(RawInputEvent::CandidateClearLine);
        let _ = input_events.send(RawInputEvent::PromptDraftOpen { text });
        line_buffer.clear();
        return;
    }
    let visible = redact_extension_setting_value(original);
    let hint = std::str::from_utf8(&visible)
        .ok()
        .and_then(candidate_inline_hint);
    let display = visible;
    line_buffer.relayed_len = display.len();
    let _ = input_events.send(RawInputEvent::CandidateRedraw {
        input: display,
        hint,
    });
}

/// Observe-only discoverability probe (#1721 T-c): when a soft-newline
/// shortcut is seen on a passthrough path (candidate buffer inactive), emit
/// a signal so the runtime can surface a one-time tip at the next
/// prompt-ready. The bytes themselves are always relayed unchanged.
fn observe_passthrough_soft_newline(bytes: &[u8], input_events: &dyn RawInputEventSink) {
    if contains_soft_newline_sequence(bytes) {
        let _ = input_events.send(RawInputEvent::SoftNewlineShortcutObserved);
    }
}

fn held_input_requests_cancel(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .any(|line| line.split_whitespace().next() == Some("/cancel"))
}

mod bash_submission_guard;
mod candidate;
mod exit_tracker;
mod path_prompt_submit;
mod soft_newline_upgrade;
mod tab_handoff;
use bash_submission_guard::{
    bash_submission_has_leading_whitespace, bash_submission_needs_guard, guarded_bash_submission,
    history_private_submission,
};
pub(super) use exit_tracker::ExplicitExitTracker;
use soft_newline_upgrade::{handle_prompt_line_soft_newline, PromptLineSoftNewline};

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
