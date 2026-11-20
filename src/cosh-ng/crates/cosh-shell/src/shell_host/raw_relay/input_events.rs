//! Raw input event delivery into the OSC parser and terminal display.

use std::io::{self, Write};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use super::super::prompt_replay::prompt_replay_bytes;
use super::ShellEventView;
use crate::raw_input::{update_locked_input_mode, RawInputEvent, RawInputMode, RawObserverAction};
use crate::types::ShellEvent;

use super::display_scan::candidate_display_columns;
use super::{OscParser, PromptPresentation, PromptReplayTracker, RESTORE_CURSOR, SAVE_CURSOR};

/// Publishes a prompt snapshot only after the current PTY batch is drained.
pub(super) fn publish_prompt_snapshot_if_drained(parser: &OscParser, pty_drained: bool) {
    if pty_drained {
        parser.publish_quiescent_prompt_snapshot();
    }
}

/// Applies prompt-boundary evidence before the runtime observes the cut.
/// Occupancy held for unclassified input persists for the session.
pub(super) fn apply_prompt_boundary_evidence(
    parser: &mut OscParser,
    prompt_replay: &mut PromptReplayTracker,
    blocked_prompt_generation: &mut Option<u64>,
) {
    let hold_prompt_occupancy = prompt_replay.observe_prompt_boundary();
    let has_unconsumed_submit = prompt_replay.has_unconsumed_submit();
    *blocked_prompt_generation = (has_unconsumed_submit || hold_prompt_occupancy)
        .then(|| prompt_replay.last_written_generation());

    if hold_prompt_occupancy {
        parser.push_shell_prompt_input_barrier();
    }
}

fn erase_native_columns<W: Write>(output: &mut W, columns: usize) -> io::Result<()> {
    for _ in 0..columns {
        write!(output, "\x08 \x08")?;
    }
    Ok(())
}

/// Terminal-side memory of the candidate echo. Erasing costs three bytes per
/// column and rewriting repeats the whole draft, so redrawing per input read
/// is quadratic in a pasted draft (#10318). Remembering what is on screen lets
/// a draft that only grew append its new suffix.
#[derive(Debug, Default)]
pub(super) struct CandidateEcho {
    /// Bytes currently displayed after the prompt. Empty whenever a clear,
    /// commit, or overlay left the cursor off the echo's end.
    written: Vec<u8>,
    /// Display columns of `written`, used by the native-mode erase loop.
    columns: usize,
    /// The last draw ended with an inline hint past the draft.
    hint_written: bool,
}

impl CandidateEcho {
    /// Whether the terminal already shows exactly `input`.
    fn matches(&self, input: &[u8]) -> bool {
        !self.written.is_empty() && !self.hint_written && self.written == input
    }

    /// Length of the already-drawn prefix of `input`, when the echo can be
    /// extended in place instead of being erased and rewritten.
    fn appendable_prefix_len(&self, input: &[u8]) -> Option<usize> {
        (!self.written.is_empty()
            && !self.hint_written
            && input.len() > self.written.len()
            && input.starts_with(&self.written))
        .then_some(self.written.len())
    }

    /// Display columns currently echoed, for erase math and tests.
    pub(super) fn display_columns(&self) -> usize {
        self.columns
    }

    /// Drops the memory because something else now owns the line: a clear,
    /// a commit, an overlay, or shell output repainting the screen.
    pub(super) fn reset(&mut self) {
        self.written.clear();
        self.columns = 0;
        self.hint_written = false;
    }
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
    candidate_echo: &mut CandidateEcho,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
) -> io::Result<bool> {
    let native_mode = prompt.is_empty();
    let mut eof_shutdown_requested = false;
    while let Ok(event) = input_events.try_recv() {
        match event {
            RawInputEvent::ShellInputActivity { empty } => {
                parser.push_shell_input_activity_event(empty);
                if !empty {
                    prompt_presentation.selection_surface_mut().defer();
                }
            }
            RawInputEvent::PtyUserWrite {
                generation,
                line_submits,
            } => {
                // Any user bytes reaching the PTY invalidate the
                // prompt-cwd report: submit-detection is a documented
                // heuristic (CR/LF/Ctrl-O only), so a custom
                // `accept-line` binding must not slip past either
                // evidence barrier. Repeated writes may collapse, so prompt
                // occupancy remains closed until the shell proves it is idle.
                parser.push_shell_pty_input_events();
                parser.push_shell_prompt_submits(line_submits);
                prompt_replay.observe_user_write(generation, line_submits);
            }
            RawInputEvent::CtrlC => parser.push_control_event("ctrl_c"),
            RawInputEvent::Esc => parser.push_control_event("esc"),
            RawInputEvent::SoftNewlineShortcutObserved => parser.push_soft_newline_shortcut_event(),
            RawInputEvent::MultilinePasteObserved => parser.push_multiline_paste_event(),
            RawInputEvent::EofShutdownRequested => eof_shutdown_requested = true,
            RawInputEvent::SyntheticPromptRepaint => parser.arm_synthetic_prompt_repaint(),
            RawInputEvent::PromptDraftOpen { text } => {
                prompt_presentation.selection_surface_mut().note_draft();
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
                if hint.is_none() && candidate_echo.matches(&input) {
                    // The draft did not change (for example the paste closer
                    // arriving in its own read); redrawing would repeat it.
                    continue;
                }
                match candidate_echo.appendable_prefix_len(&input) {
                    Some(prefix_len) => output.write_all(&input[prefix_len..])?,
                    None => {
                        if native_mode {
                            // Erase by display columns and rewrite the whole
                            // draft: byte-offset math breaks on CJK and marker
                            // bytes (#1721). Erase-to-EOL clears any stale
                            // inline hint residue.
                            erase_native_columns(output, candidate_echo.columns)?;
                            write!(output, "\x1b[K")?;
                            output.write_all(&input)?;
                        } else {
                            write!(output, "\r\x1b[2K")?;
                            prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
                            output.write_all(&input)?;
                        }
                    }
                }
                // Measure the whole draft: a CJK or emoji sequence split
                // across two reads counts as replacement glyphs when only the
                // appended suffix is measured, drifting the erase width that
                // a later shrink relies on (#1721).
                candidate_echo.columns = candidate_display_columns(&input);
                if let Some(hint) = &hint {
                    write_inline_hint(output, hint)?;
                }
                candidate_echo.hint_written = hint.is_some();
                candidate_echo.written = input;
                output.flush()?;
            }
            RawInputEvent::CandidateCommit(input) => {
                if native_mode {
                    erase_native_columns(output, candidate_echo.columns)?;
                    write!(output, "\x1b[K")?;
                    output.write_all(&input)?;
                } else {
                    write!(output, "\r\x1b[2K")?;
                    prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
                    output.write_all(&input)?;
                }
                output.write_all(b"\r\n")?;
                candidate_echo.reset();
                output.flush()?;
            }
            RawInputEvent::PromptGhostClear => {
                clear_prompt_ghost_line(
                    parser,
                    output,
                    prompt,
                    candidate_echo,
                    prompt_presentation,
                )?;
            }
            RawInputEvent::PromptGhostAccepted { suggestion_id } => {
                parser.push_prompt_ghost_event("accepted", suggestion_id.as_deref());
            }
            RawInputEvent::PromptGhostCycle {
                text,
                suggestion_id,
                generation,
            } => {
                let (width, height) = ratatui::crossterm::terminal::size().unwrap_or((0, 0));
                let plan = prompt_presentation.selection_surface_mut().plan_cycle(
                    suggestion_id.as_deref(),
                    generation,
                    prompt,
                    parser.display_position(),
                    width,
                    height,
                );
                match plan {
                    super::selection_surface::CyclePlan::RedrawList { lines } => {
                        redraw_suggestion_list(output, &lines, prompt, &text, prompt_presentation)?;
                        candidate_echo.reset();
                    }
                    super::selection_surface::CyclePlan::PromptLocal { label } => {
                        clear_prompt_ghost_line(
                            parser,
                            output,
                            prompt,
                            candidate_echo,
                            prompt_presentation,
                        )?;
                        super::display_write::write_prompt_ghost_labeled(
                            output,
                            &text,
                            true,
                            Some(&label),
                        )?;
                        candidate_echo.reset();
                        output.flush()?;
                    }
                    super::selection_surface::CyclePlan::Ignore => {}
                }
            }
            RawInputEvent::PromptGhostDismissed => {
                parser.push_prompt_ghost_event("dismissed", None)
            }
            RawInputEvent::AssistanceToggled => {
                write!(output, "\r\x1b[2K")?;
                prompt_presentation.write_restored_prompt(output, parser.last_prompt_display())?;
                candidate_echo.reset();
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
                    erase_native_columns(output, candidate_echo.columns)?;
                    write!(output, "\x1b[K")?;
                } else {
                    write!(output, "\r\x1b[2K")?;
                    prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
                }
                candidate_echo.reset();
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
            RawInputEvent::TrialControl { id, action } => {
                parser.push_card_event("trial_control", &format!("{id}:{action}"))
            }
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

pub(super) fn latest_capture_submission_generation(events: &[ShellEvent]) -> Option<u64> {
    for event in events.iter().rev() {
        let Some(capture) = event.capture.as_ref() else {
            continue;
        };
        match capture.lifecycle {
            crate::types::ShellCaptureLifecycle::Submitted => return Some(capture.generation),
            crate::types::ShellCaptureLifecycle::Drained
            | crate::types::ShellCaptureLifecycle::Expired
            | crate::types::ShellCaptureLifecycle::Overflow
            | crate::types::ShellCaptureLifecycle::InputRejected => return None,
        }
    }
    None
}
pub(super) fn observe_with_input_mode_lock<W: Write, F, B>(
    event_observer: &mut F,
    parser: &mut OscParser,
    output: &mut W,
    input_mode: &Arc<Mutex<RawInputMode>>,
    before_observe: &mut B,
) -> io::Result<RawObserverAction>
where
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
    B: FnMut(&mut OscParser, &mut W) -> io::Result<()>,
{
    let mut mode = input_mode
        .lock()
        .map_err(|_| io::Error::other("raw input mode lock poisoned"))?;
    before_observe(parser, output)?;
    let action = parser.observe_events(output, event_observer)?;
    let acknowledged = latest_capture_submission_generation(&parser.events);
    update_locked_input_mode(&mut mode, &action, acknowledged);
    Ok(action)
}

pub(super) fn redraw_suggestion_list<W: Write>(
    output: &mut W,
    lines: &[String],
    prompt: &str,
    ghost_text: &str,
    prompt_presentation: &PromptPresentation,
) -> io::Result<()> {
    let rows = lines.len() + 1;
    write!(output, "\x1b[?7l\x1b[{rows}A")?;
    for line in lines {
        write!(output, "\r\x1b[2K{line}\n")?;
    }
    write!(output, "\n\r\x1b[2K")?;
    prompt_presentation.write_replayed_prompt(output, prompt.as_bytes())?;
    super::display_write::write_prompt_ghost(output, ghost_text, true)?;
    write!(output, "\x1b[?7h")?;
    output.flush()
}

fn clear_prompt_ghost_line<W: Write>(
    parser: &OscParser,
    output: &mut W,
    fallback_prompt: &str,
    candidate_echo: &mut CandidateEcho,
    prompt_presentation: &PromptPresentation,
) -> io::Result<()> {
    write!(output, "\r\x1b[2K")?;
    let replay = prompt_replay_bytes(parser.last_prompt_display());
    if replay.is_empty() {
        prompt_presentation.write_replayed_prompt(output, fallback_prompt.as_bytes())?;
    } else {
        prompt_presentation.write_replayed_prompt(output, replay)?;
    }
    candidate_echo.reset();
    output.flush()
}
