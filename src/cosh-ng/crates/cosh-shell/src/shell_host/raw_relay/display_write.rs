//! Pending shell output and prompt-ghost writes for the raw relay.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::raw_input::RawInputMode;

use super::super::osc::OscParser;
use super::super::prompt_presentation::PromptPresentation;
use super::super::prompt_replay::PromptReplayTracker;
use super::display_scan::scan_display;
use super::input_events::write_no_wrap_overlay;
use super::CandidateEcho;

/// Drops the echo memory unless `[start, end)` of the display stream is
/// side-effect-free. An OSC title, SGR styling or a cursor-style change is
/// forwarded to the terminal but leaves every cell where it is, so a later
/// redraw may still append to the drawn prefix. Text and any cursor-movement
/// or erase sequence — including zero-width ones such as `\x1b[H\x1b[2J` and
/// `\r\x1b[K` — rewrite the line, so the memory no longer describes it.
pub(super) fn reset_candidate_echo_for_display(
    parser: &OscParser,
    start: usize,
    end: usize,
    candidate_echo: &mut CandidateEcho,
) -> io::Result<()> {
    let inert = end <= start || scan_display(parser.read_display_range(start, end)?.as_ref()).inert;
    if !inert {
        candidate_echo.reset();
    }
    Ok(())
}

pub(super) fn write_pending_display<W: Write>(
    parser: &OscParser,
    output: &mut W,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
) -> io::Result<()> {
    let display_end = parser.display_position();
    prompt_presentation.write_display_slice(
        parser,
        output,
        *display_start,
        display_end,
        prompt_replay,
    )?;
    *display_start = display_end;
    Ok(())
}

pub(super) fn write_pending_display_preserving_prompt_ghost<W: Write>(
    parser: &OscParser,
    output: &mut W,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
    input_mode: &Arc<Mutex<RawInputMode>>,
    candidate_echo: &mut CandidateEcho,
) -> io::Result<()> {
    // Shell output that repaints or moves the line the candidate echo
    // assumed (a job notification, a new prompt, a clear) invalidates the
    // memory. Output that only restyles or retitles the terminal does not.
    let display_end = parser.display_position();
    reset_candidate_echo_for_display(parser, *display_start, display_end, candidate_echo)?;
    write_pending_display(
        parser,
        output,
        display_start,
        prompt_replay,
        prompt_presentation,
    )?;
    let ghost = input_mode.lock().ok().and_then(|mode| match &*mode {
        RawInputMode::PromptGhost { text, route } => Some((
            text.clone(),
            matches!(
                route,
                crate::raw_input::PromptGhostRoute::AgentSelection { .. }
            ),
        )),
        _ => None,
    });
    if let Some((text, selection)) = ghost {
        write_prompt_ghost(output, &text, selection)?;
    }
    Ok(())
}

pub(super) fn write_prompt_ghost<W: Write>(
    output: &mut W,
    text: &str,
    selection: bool,
) -> io::Result<()> {
    write_prompt_ghost_labeled(output, text, selection, None)
}

pub(super) fn write_prompt_ghost_labeled<W: Write>(
    output: &mut W,
    text: &str,
    selection: bool,
    label: Option<&str>,
) -> io::Result<()> {
    let marker = if selection { " ›" } else { "" };
    let prefix = match label {
        Some(label) => format!("{marker} {label} "),
        None => format!("{marker} "),
    };
    write_no_wrap_overlay(output, |output| write!(output, "\x1b[2m{prefix}{text}"))
}
