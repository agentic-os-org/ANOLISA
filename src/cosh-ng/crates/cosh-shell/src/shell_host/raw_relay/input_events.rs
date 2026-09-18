//! Raw input event delivery into the OSC parser and terminal display.

use std::io::{self, Write};
use std::sync::mpsc::Receiver;

use unicode_width::UnicodeWidthChar;

use crate::raw_input::RawInputEvent;

use super::{
    clear_prompt_ghost_line, OscParser, PromptPresentation, PromptReplayTracker, RESTORE_CURSOR,
    SAVE_CURSOR,
};

/// Terminal display columns of candidate echo bytes: ANSI escape sequences
/// are zero-width; other content is measured per Unicode width (CJK = 2).
/// Keeps native erase math correct for multi-byte drafts (#1721 G9).
pub(super) fn candidate_display_columns(bytes: &[u8]) -> usize {
    // Sums real display widths (east-asian wide chars count 2), so the
    // erase loop backspaces once per terminal column. Any terminal-specific
    // wide-char overwrite quirk is covered by the `\x1b[K` clear plus the
    // full-line rewrite that always follow the erase.
    let text = String::from_utf8_lossy(bytes);
    let mut columns = 0;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for terminator in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&terminator) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for terminator in chars.by_ref() {
                        if terminator == '\x07' {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        columns += ch.width().unwrap_or(0);
    }
    columns
}

fn erase_native_columns<W: Write>(output: &mut W, columns: usize) -> io::Result<()> {
    for _ in 0..columns {
        write!(output, "\x08 \x08")?;
    }
    Ok(())
}

pub(super) fn write_no_wrap_overlay<W, F>(output: &mut W, body: F) -> io::Result<()>
where
    W: Write,
    F: FnOnce(&mut W) -> io::Result<()>,
{
    let save_result = output.write_all(SAVE_CURSOR.as_bytes());
    let cursor_saved = save_result.is_ok();
    let render_result = save_result.and_then(|()| {
        output.write_all(b"\x1b[?7l")?;
        body(output)
    });

    // Keep cleanup independent: a recoverable body failure must not strand
    // the outer terminal in dim/no-wrap mode or at the overlay cursor.
    let mut cleanup_error = None;
    for sequence in [b"\x1b[0m".as_slice(), b"\x1b[?7h"] {
        if let Err(error) = output.write_all(sequence) {
            cleanup_error.get_or_insert(error);
        }
    }
    if cursor_saved {
        if let Err(error) = output.write_all(RESTORE_CURSOR.as_bytes()) {
            cleanup_error.get_or_insert(error);
        }
    }

    match render_result {
        // Preserve the initiating failure even if best-effort cleanup fails.
        Err(error) => Err(error),
        Ok(()) => cleanup_error.map_or(Ok(()), Err),
    }
}

// The inline hint must never trigger terminal auto-wrap: a wrapped tail
// lands on the next screen line where the erase-to-EOL of the following
// redraw/commit cannot reach it, leaving residue behind. The native
// prompt width is unknown here, so clipping by remaining columns is not
// possible; instead auto-wrap (DECAWM) is disabled around the write and
// the terminal clips the hint at the right edge itself.
fn write_inline_hint<W: Write>(output: &mut W, hint: &str) -> io::Result<()> {
    write_no_wrap_overlay(output, |output| write!(output, "\x1b[2m {hint}"))
}

pub(super) fn drain_raw_input_events<W: Write>(
    input_events: &Receiver<RawInputEvent>,
    parser: &mut OscParser,
    output: &mut W,
    prompt: &str,
    native_candidate_echoed_len: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &PromptPresentation,
) -> io::Result<bool> {
    let native_mode = prompt.is_empty();
    let mut eof_shutdown_requested = false;
    while let Ok(event) = input_events.try_recv() {
        match event {
            RawInputEvent::ShellInputActivity { empty } => {
                parser.push_shell_input_activity_event(empty)
            }
            RawInputEvent::PtyUserWrite {
                generation,
                line_submits,
            } => {
                // Any user bytes reaching the PTY invalidate the
                // prompt-cwd report: submit-detection is a documented
                // heuristic (CR/LF/Ctrl-O only), so a custom
                // `accept-line` binding must not slip past the
                // barrier. The parser collapses consecutive writes
                // into one event per prompt.
                parser.push_shell_pty_input_event();
                prompt_replay.observe_user_write(generation, line_submits)
            }
            RawInputEvent::CtrlC => parser.push_control_event("ctrl_c"),
            RawInputEvent::Esc => parser.push_control_event("esc"),
            RawInputEvent::SoftNewlineShortcutObserved => parser.push_soft_newline_shortcut_event(),
            RawInputEvent::MultilinePasteObserved => parser.push_multiline_paste_event(),
            RawInputEvent::EofShutdownRequested => eof_shutdown_requested = true,
            RawInputEvent::SyntheticPromptRepaint => parser.arm_synthetic_prompt_repaint(),
            RawInputEvent::PromptDraftOpen { text } => {
                let payload = serde_json::json!({ "text": text }).to_string();
                parser.push_prompt_draft_event("open", Some(&payload));
            }
            RawInputEvent::PromptDraftChanged {
                id,
                text,
                viewport,
                line_count,
                selected_completion,
            } => {
                let payload = serde_json::json!({
                    "id": id,
                    "text": text,
                    "line_count": line_count,
                    "selected_completion": selected_completion,
                    "first_row": viewport.first_row,
                    "hidden_above": viewport.hidden_above,
                    "hidden_below": viewport.hidden_below,
                    "rows": viewport.rows,
                    "cursor_row": viewport.cursor.0,
                    "cursor_col": viewport.cursor.1,
                })
                .to_string();
                parser.push_prompt_draft_event("changed", Some(&payload));
            }
            RawInputEvent::PromptDraftSubmit {
                id,
                text,
                slash,
                workspace_cwd,
            } => {
                let payload =
                    serde_json::json!({ "id": id, "text": text, "slash": slash }).to_string();
                parser.push_prompt_draft_event("submit", Some(&payload));
                // Preserve the owning capture's decision: controls use the same
                // consumers as shell slash commands; other drafts stay Agent input.
                let session_id = parser.session_id.clone();
                let reason = if slash {
                    "slash"
                } else if text.trim_start().starts_with("??") {
                    "agent_marker"
                } else {
                    "natural_language"
                };
                let text = if slash { text.trim().to_string() } else { text };
                let cwd = if slash { workspace_cwd } else { None };
                parser.push_intercept_event(&session_id, text, cwd, reason);
            }
            RawInputEvent::PromptDraftCancel { id } => {
                let payload = serde_json::json!({ "id": id }).to_string();
                parser.push_prompt_draft_event("cancel", Some(&payload));
            }
            RawInputEvent::CandidateRedraw { input, hint } => {
                if native_mode {
                    // Erase by display columns and rewrite the whole draft:
                    // byte-offset math breaks on CJK and marker bytes (#1721).
                    // Erase-to-EOL clears any stale inline hint residue.
                    erase_native_columns(output, *native_candidate_echoed_len)?;
                    write!(output, "\x1b[K")?;
                    output.write_all(&input)?;
                    if let Some(hint) = hint {
                        write_inline_hint(output, &hint)?;
                    }
                    *native_candidate_echoed_len = candidate_display_columns(&input);
                } else {
                    write!(output, "\r\x1b[2K")?;
                    prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
                    output.write_all(&input)?;
                    if let Some(hint) = hint {
                        write_inline_hint(output, &hint)?;
                    }
                }
                output.flush()?;
            }
            RawInputEvent::CandidateCommit(input) => {
                if native_mode {
                    erase_native_columns(output, *native_candidate_echoed_len)?;
                    write!(output, "\x1b[K")?;
                    output.write_all(&input)?;
                    *native_candidate_echoed_len = 0;
                } else {
                    write!(output, "\r\x1b[2K")?;
                    prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
                    output.write_all(&input)?;
                }
                writeln!(output)?;
                output.flush()?;
            }
            RawInputEvent::PromptGhostClear => {
                clear_prompt_ghost_line(
                    parser,
                    output,
                    prompt,
                    native_candidate_echoed_len,
                    prompt_presentation,
                )?;
            }
            RawInputEvent::PromptGhostAccepted { suggestion_id } => {
                parser.push_prompt_ghost_event("accepted", suggestion_id.as_deref());
            }
            RawInputEvent::PromptGhostCycle { text } => {
                clear_prompt_ghost_line(
                    parser,
                    output,
                    prompt,
                    native_candidate_echoed_len,
                    prompt_presentation,
                )?;
                super::write_prompt_ghost(output, &text, true)?;
                output.flush()?;
            }
            RawInputEvent::PromptGhostDismissed => {
                parser.push_prompt_ghost_event("dismissed", None)
            }
            RawInputEvent::AssistanceToggled => {
                write!(output, "\r\x1b[2K")?;
                prompt_presentation.write_restored_prompt(output, parser.last_prompt_display())?;
                output.flush()?;
            }
            RawInputEvent::PromptGhostIntercept {
                input,
                suggestion_id,
            } => {
                let session_id = parser.session_id.clone();
                let component = suggestion_id
                    .map(|id| format!("prompt_ghost:{id}"))
                    .unwrap_or_else(|| "prompt_ghost".to_string());
                parser.push_intercept_event(&session_id, input, None, &component);
            }
            RawInputEvent::CandidateClearLine => {
                if native_mode {
                    erase_native_columns(output, *native_candidate_echoed_len)?;
                    write!(output, "\x1b[K")?;
                    *native_candidate_echoed_len = 0;
                } else {
                    write!(output, "\r\x1b[2K")?;
                    prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
                }
                output.flush()?;
            }
            RawInputEvent::UserIntercept(input, reason) => {
                let session_id = parser.session_id.clone();
                parser.push_intercept_event(&session_id, input, None, reason.as_str())
            }
            RawInputEvent::UserInterceptWithRouting {
                input,
                reason,
                cwd,
                sensitive,
            } => {
                let session_id = parser.session_id.clone();
                parser.push_intercept_event_with_routing(
                    &session_id,
                    input,
                    Some(cwd),
                    reason.as_str(),
                    None,
                    false,
                    sensitive,
                )
            }
            RawInputEvent::NativePathPromptIntercept {
                input,
                cwd,
                sensitive,
            } => {
                let session_id = parser.session_id.clone();
                parser.push_intercept_event_with_routing(
                    &session_id,
                    input,
                    Some(cwd),
                    "path_prompt",
                    None,
                    false,
                    sensitive,
                )
            }
            RawInputEvent::UserInterceptAtPrompt {
                input,
                reason,
                pending_submits,
            } => parser.push_intercept_event_at_prompt(input, reason.as_str(), pending_submits),
            RawInputEvent::CaptureSubmitted {
                kind,
                target_id,
                generation,
            } => parser.push_capture_event(
                crate::types::ShellCaptureLifecycle::Submitted,
                generation,
                Some(kind),
                Some(&target_id),
            ),
            RawInputEvent::CaptureDrained { generation } => parser.push_capture_event(
                crate::types::ShellCaptureLifecycle::Drained,
                generation,
                None,
                None,
            ),
            RawInputEvent::CaptureExpired { generation } => parser.push_capture_event(
                crate::types::ShellCaptureLifecycle::Expired,
                generation,
                None,
                None,
            ),
            RawInputEvent::CaptureOverflow { generation } => parser.push_capture_event(
                crate::types::ShellCaptureLifecycle::Overflow,
                generation,
                None,
                None,
            ),
            RawInputEvent::CaptureInputRejected { generation, .. } => parser.push_capture_event(
                crate::types::ShellCaptureLifecycle::InputRejected,
                generation,
                None,
                None,
            ),
            RawInputEvent::CardFocus(id, selected) => {
                parser.push_card_event("focus", &format!("{id}:{selected}"))
            }
            RawInputEvent::CardToggle(id, selected) => {
                parser.push_card_event("toggle", &format!("{id}:{selected}"))
            }
            RawInputEvent::CardInput(id, text) => {
                parser.push_card_event("input", &format!("{id}:{text}"))
            }
            RawInputEvent::CardSecretInput(id, text) => {
                parser.push_secret_card_event("input", &format!("{id}:{text}"))
            }
            RawInputEvent::CardApprove(id) => parser.push_card_event("approve", &id),
            RawInputEvent::CardApproveTurn(id) => parser.push_card_event("approve_turn", &id),
            RawInputEvent::CardAlwaysTrust(id) => parser.push_card_event("always_trust", &id),
            RawInputEvent::CardDeny(id) => parser.push_card_event("deny", &id),
            RawInputEvent::CardDetails(id) => parser.push_card_event("details", &id),
            RawInputEvent::CardCancel(id) => parser.push_card_event("cancel", &id),
            RawInputEvent::CardAnswer(answer) => parser.push_card_event("answer", &answer),
            RawInputEvent::QuestionSubmitAttempt(id) => {
                parser.push_card_event("question_submit_empty", &id)
            }
            RawInputEvent::CardSecretAnswer(answer) => {
                parser.push_secret_card_event("answer", &answer)
            }
            RawInputEvent::QuestionCancel(id) => parser.push_card_event("question_cancel", &id),
            RawInputEvent::QuestionAbort(id) => parser.push_card_event("question_abort", &id),
            RawInputEvent::EvidenceSend(id) => parser.push_card_event("evidence_send", &id),
            RawInputEvent::EvidenceIgnore(id) => parser.push_card_event("evidence_ignore", &id),
            RawInputEvent::EvidenceCancel(id) => parser.push_card_event("evidence_cancel", &id),
            RawInputEvent::ModeFocus(id, selected) => {
                parser.push_card_event("mode_focus", &format!("{id}:{selected}"))
            }
            RawInputEvent::ModeSet(id, selected) => {
                parser.push_card_event("mode_set", &format!("{id}:{selected}"))
            }
            RawInputEvent::ModeCancel(id) => parser.push_card_event("mode_cancel", &id),
            RawInputEvent::ConfigFocus(id, selected) => {
                parser.push_card_event("config_focus", &format!("{id}:{selected}"))
            }
            RawInputEvent::ConfigSave(id) => parser.push_card_event("config_save", &id),
            RawInputEvent::ConfigCancel(id) => parser.push_card_event("config_cancel", &id),
            RawInputEvent::ConfigLanguageFocus(id, selected) => {
                parser.push_card_event("config_language_focus", &format!("{id}:{selected}"))
            }
            RawInputEvent::ConfigLanguageSet(id, selected) => {
                parser.push_card_event("config_language_set", &format!("{id}:{selected}"))
            }
            RawInputEvent::ConfigLanguageCancel(id) => {
                parser.push_card_event("config_language_cancel", &id)
            }
            RawInputEvent::SessionFocus(id, selected) => {
                parser.push_card_event("session_focus", &format!("{id}:{selected}"))
            }
            RawInputEvent::SessionToggle(id, selected) => {
                parser.push_card_event("session_toggle", &format!("{id}:{selected}"))
            }
            RawInputEvent::SessionResume(id, selected) => {
                parser.push_card_event("session_resume", &format!("{id}:{selected}"))
            }
            RawInputEvent::SessionDelete(id) => parser.push_card_event("session_delete", &id),
            RawInputEvent::SessionClearConfirm(id) => {
                parser.push_card_event("session_clear_confirm", &id)
            }
            RawInputEvent::SessionCancel(id) => parser.push_card_event("session_cancel", &id),
        }
    }
    Ok(eof_shutdown_requested)
}
