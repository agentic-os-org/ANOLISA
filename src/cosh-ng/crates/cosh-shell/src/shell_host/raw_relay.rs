use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Child;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::pty::Winsize;

use crate::raw_input::{
    update_input_mode, update_locked_input_mode, RawInputEvent, RawInputMode, RawObserverAction,
    UserPtyInputGeneration,
};
use crate::types::{CommandOrigin, ShellEvent, ShellEventKind};

use super::model::ShellEventView;
use super::osc::{DisplayCutKind, OscParser};
use super::prompt_presentation::PromptPresentation;
use super::prompt_replay::{prompt_replay_bytes, PromptReplayTracker};

mod activity;
mod eof_shutdown;
mod input_events;
mod input_readiness;
pub(super) mod interactive_sentinel;
mod pty_emit;
mod terminal_recovery;
mod terminal_size;

use activity::{
    poll_driver_completion, relay_wait_timeout, wait_for_relay_activity, RelayActivity,
};
pub(super) use activity::{DriverCompletion, RawActionWatchdog};
use eof_shutdown::{advance_eof_shutdown, request_eof_shutdown};
use input_events::{candidate_display_columns, drain_raw_input_events, write_no_wrap_overlay};
use input_readiness::RawInputReadinessProbe;
use interactive_sentinel::{
    emit_interactive_hint_if_waiting, InputWaitStatus, InteractiveHintKind, SentinelThrottle,
};
use pty_emit::resolve_pty_emit;
#[cfg(test)]
use pty_emit::restore_prompt_display_before_handoff;
use terminal_recovery::{restore_terminal_after_interrupted_command, PendingTerminalRecovery};
use terminal_size::sync_outer_terminal_winsize;

// DECSC/DECRC are the terminfo `sc`/`rc` sequences for xterm-compatible
// terminals. Unlike CSI s/u, they also restore the cursor in macOS Terminal.
const SAVE_CURSOR: &str = "\x1b7";
const RESTORE_CURSOR: &str = "\x1b8";
const PTY_READ_BATCH_BYTES: usize = 256 * 1024;

fn publish_prompt_snapshot_if_drained(parser: &OscParser, pty_drained: bool) {
    if pty_drained {
        parser.publish_quiescent_prompt_snapshot();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_raw_until_exit<W: Write, F>(
    master: &mut File,
    terminal: &File,
    child: &mut Child,
    parser: &mut OscParser,
    prompt_presentation: &mut PromptPresentation,
    output: &mut W,
    event_observer: &mut F,
    input_events: &Receiver<RawInputEvent>,
    driver_completion: &Receiver<DriverCompletion>,
    wake: &mut UnixStream,
    resize: &mut UnixStream,
    input_mode: &Arc<Mutex<RawInputMode>>,
    input_generation: &UserPtyInputGeneration,
    last_winsize: &mut Winsize,
    prompt: &str,
    recovery_request_file: &Path,
    handoff_request_file: &Path,
    bounded_bash_handoff: bool,
    watchdog: Option<&RawActionWatchdog>,
    input_wait_status: &InputWaitStatus,
    hint_i18n: &crate::i18n::I18n,
    input_wait_timeout_secs: u64,
    hint_card_renderer: Option<&crate::shell_host::HintCardRenderer>,
) -> io::Result<bool>
where
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
{
    let mut buffer = [0_u8; 8192];
    let mut display_start = parser.display_position();
    let mut native_candidate_echoed_len = 0;
    let mut prompt_replay = PromptReplayTracker::new(input_generation.clone());
    let mut last_pty_output: Option<Instant> = None;
    let mut pending_terminal_restore = PendingTerminalRecovery::default();
    let mut pending_prompt_restore = None;
    let mut input_readiness = RawInputReadinessProbe::from_env();
    let mut driver_completed_at = None;
    let mut eof_shutdown = None;
    // #2025: interactive sentinel state — sampling throttle plus the hint
    // kind already shown for the current agent-handoff episode.
    let mut sentinel_throttle = SentinelThrottle::new();
    let mut sentinel_shown: Option<InteractiveHintKind> = None;
    // #1932 F4: ask the outer terminal to report modifier-carrying editing
    // keys (modifyOtherKeys level 1, e.g. Shift+Enter -> CSI 27;2;13~).
    // Written on the ordered output path after startup rendering settled;
    // unsupporting terminals ignore it, RawModeGuard withdraws it on exit.
    output.write_all(b"\x1b[>4;1m")?;
    output.flush()?;
    sync_outer_terminal_winsize(master.as_raw_fd(), child.id(), last_winsize)?;
    loop {
        if restore_terminal_after_interrupted_command(
            terminal.as_raw_fd(),
            parser,
            &mut pending_terminal_restore,
        )? {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        if drain_raw_input_events(
            input_events,
            parser,
            output,
            prompt,
            &mut native_candidate_echoed_len,
            &mut prompt_replay,
            prompt_presentation,
        )? {
            request_eof_shutdown(master, terminal, child, &mut eof_shutdown)?;
        }
        poll_driver_completion(
            driver_completion,
            master,
            terminal,
            child,
            &mut driver_completed_at,
        )?;
        let mut observer_action = merge_pending_prompt_restore(
            observe_with_input_mode_lock(event_observer, parser, output, input_mode)?,
            &mut pending_prompt_restore,
        );
        observer_action = resolve_pty_emit(
            master,
            child.id(),
            terminal.as_raw_fd(),
            parser,
            output,
            input_mode,
            observer_action,
            &mut display_start,
            &mut prompt_replay,
            prompt_presentation,
            &mut pending_terminal_restore,
            recovery_request_file,
            handoff_request_file,
            bounded_bash_handoff,
        )?;
        remember_pending_prompt_restore(&observer_action, &mut pending_prompt_restore);
        update_input_mode(
            input_mode,
            &observer_action,
            latest_capture_submission_generation(&parser.events),
        );
        let mut hold_shell_output = observer_action.hold_shell_output();
        if !hold_shell_output && parser.display_position() > display_start {
            write_pending_display_preserving_prompt_ghost(
                parser,
                output,
                &mut display_start,
                &mut prompt_replay,
                prompt_presentation,
                input_mode,
            )?;
            output.flush()?;
        }
        let mut batch_bytes = 0usize;
        let mut pty_drained = false;
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    batch_bytes = batch_bytes.saturating_add(n);
                    last_pty_output = Some(Instant::now());
                    sentinel_throttle.note_output();
                    // Output resumed => the foreground is no longer sitting
                    // on a read; the input-wait timeout clock restarts on
                    // the next eligible sample (#2161 clear-on-activity).
                    input_wait_status.clear();
                    parser.feed(&buffer[..n])?;
                    prompt_presentation.observe(parser);
                    // Relay-side events sent before a PTY write (e.g. the
                    // synthetic prompt-repaint arm) must land before the
                    // display cuts produced by that write's echo are
                    // handled: re-drain here, the top-of-loop drain alone
                    // loses that ordering inside this read loop (#1932).
                    if drain_raw_input_events(
                        input_events,
                        parser,
                        output,
                        prompt,
                        &mut native_candidate_echoed_len,
                        &mut prompt_replay,
                        prompt_presentation,
                    )? {
                        request_eof_shutdown(master, terminal, child, &mut eof_shutdown)?;
                    }
                    let display_cuts = parser.drain_intervention_display_cuts();
                    for (cut, cut_kind) in display_cuts {
                        let cut = cut.min(parser.display_position());
                        // Only a real prompt boundary (precmd) confirms the
                        // shell finished responding to the relay writes seen
                        // so far; an intercepted line's remaining response is
                        // just the prompt repaint replay dedup strips.
                        match cut_kind {
                            DisplayCutKind::PromptBoundary => {
                                prompt_replay.observe_prompt_boundary();
                                // #1932: the soft-newline upgrade submitted a
                                // synthetic empty line for this boundary; its
                                // accept echo is visually blank, so drop it
                                // instead of surfacing a stray blank line.
                                if parser.take_synthetic_prompt_repaint()
                                    && cut > display_start
                                    && candidate_display_columns(
                                        parser.read_display_range(display_start, cut)?.as_ref(),
                                    ) == 0
                                {
                                    display_start = cut;
                                }
                            }
                            DisplayCutKind::Intercept => prompt_replay.observe_intercept_cut(),
                        }
                        if !hold_shell_output && cut > display_start {
                            prompt_presentation.write_display_slice(
                                parser,
                                output,
                                display_start,
                                cut,
                                &mut prompt_replay,
                            )?;
                            output.flush()?;
                            display_start = cut;
                        }
                        observer_action = merge_pending_prompt_restore(
                            observe_with_input_mode_lock(
                                event_observer,
                                parser,
                                output,
                                input_mode,
                            )?,
                            &mut pending_prompt_restore,
                        );
                        observer_action = resolve_pty_emit(
                            master,
                            child.id(),
                            terminal.as_raw_fd(),
                            parser,
                            output,
                            input_mode,
                            observer_action,
                            &mut display_start,
                            &mut prompt_replay,
                            prompt_presentation,
                            &mut pending_terminal_restore,
                            recovery_request_file,
                            handoff_request_file,
                            bounded_bash_handoff,
                        )?;
                        remember_pending_prompt_restore(
                            &observer_action,
                            &mut pending_prompt_restore,
                        );
                        update_input_mode(
                            input_mode,
                            &observer_action,
                            latest_capture_submission_generation(&parser.events),
                        );
                        hold_shell_output = observer_action.hold_shell_output();
                        if !hold_shell_output && parser.display_position() > display_start {
                            write_pending_display_preserving_prompt_ghost(
                                parser,
                                output,
                                &mut display_start,
                                &mut prompt_replay,
                                prompt_presentation,
                                input_mode,
                            )?;
                            output.flush()?;
                        }
                    }
                    // Ordinary display and passive runtime transitions are
                    // resolved once after the readiness batch. Intervention
                    // cuts stay immediate because they define semantic
                    // boundaries that can change the input owner.
                    if batch_bytes >= PTY_READ_BATCH_BYTES {
                        break;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    pty_drained = true;
                    break;
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) if child.try_wait()?.is_some() => {
                    if !advance_eof_shutdown(&mut eof_shutdown)? {
                        break;
                    }
                    release_held_shell_output(
                        event_observer,
                        parser,
                        output,
                        &mut display_start,
                        &mut prompt_replay,
                        prompt_presentation,
                    )?;
                    return Ok(eof_shutdown.is_some());
                }
                Err(err) => return Err(err),
            }
        }

        if child.try_wait()?.is_some() {
            if !advance_eof_shutdown(&mut eof_shutdown)? {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            release_held_shell_output(
                event_observer,
                parser,
                output,
                &mut display_start,
                &mut prompt_replay,
                prompt_presentation,
            )?;
            return Ok(eof_shutdown.is_some());
        }
        advance_eof_shutdown(&mut eof_shutdown)?;
        emit_interactive_hint_if_waiting(
            master.as_raw_fd(),
            child.id() as i32,
            parser,
            output,
            &mut sentinel_throttle,
            &mut sentinel_shown,
            input_wait_status,
            hint_i18n,
            input_wait_timeout_secs,
            hint_card_renderer,
        )?;
        if let Some(watchdog) = watchdog {
            if watchdog.expired(driver_completed_at) {
                child.kill()?;
                child.wait()?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "raw action relay watchdog: shell did not exit after the trailing exit was relayed",
                ));
            }
        }
        if restore_terminal_after_interrupted_command(
            terminal.as_raw_fd(),
            parser,
            &mut pending_terminal_restore,
        )? {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        if drain_raw_input_events(
            input_events,
            parser,
            output,
            prompt,
            &mut native_candidate_echoed_len,
            &mut prompt_replay,
            prompt_presentation,
        )? {
            request_eof_shutdown(master, terminal, child, &mut eof_shutdown)?;
        }
        poll_driver_completion(
            driver_completion,
            master,
            terminal,
            child,
            &mut driver_completed_at,
        )?;
        observer_action = merge_pending_prompt_restore(
            observe_with_input_mode_lock(event_observer, parser, output, input_mode)?,
            &mut pending_prompt_restore,
        );
        observer_action = resolve_pty_emit(
            master,
            child.id(),
            terminal.as_raw_fd(),
            parser,
            output,
            input_mode,
            observer_action,
            &mut display_start,
            &mut prompt_replay,
            prompt_presentation,
            &mut pending_terminal_restore,
            recovery_request_file,
            handoff_request_file,
            bounded_bash_handoff,
        )?;
        remember_pending_prompt_restore(&observer_action, &mut pending_prompt_restore);
        update_input_mode(
            input_mode,
            &observer_action,
            latest_capture_submission_generation(&parser.events),
        );
        let runtime_poll_pending = matches!(
            &observer_action,
            RawObserverAction::HoldShellOutput | RawObserverAction::DelayShellOutput
        );
        hold_shell_output = observer_action.hold_shell_output();
        if !hold_shell_output && parser.display_position() > display_start {
            write_pending_display_preserving_prompt_ghost(
                parser,
                output,
                &mut display_start,
                &mut prompt_replay,
                prompt_presentation,
                input_mode,
            )?;
            output.flush()?;
        }
        input_readiness.acknowledge_if_ready(output, input_mode)?;
        publish_prompt_snapshot_if_drained(parser, pty_drained);
        // The PTY is drained (WouldBlock) at this point: write off
        // submissions a foreground program consumed once the shell has
        // painted a prompt after the last boundary and idles at it. A bare
        // precmd is not enough — the user's PROMPT_COMMAND and PS1 paint run
        // after the marker, so silence alone must never clear the ledger.
        prompt_replay.reconcile_idle_at_prompt(
            parser.has_active_foreground_command(),
            parser.has_prompt_painted_since_ready(),
            last_pty_output,
        );
        let foreground_command_active = parser.has_active_foreground_command();
        let activity = wait_for_relay_activity(
            master.as_raw_fd(),
            wake,
            resize,
            relay_wait_timeout(
                watchdog,
                driver_completed_at,
                eof_shutdown.as_ref(),
                runtime_poll_pending,
                foreground_command_active,
                prompt_replay.idle_reconcile_remaining(
                    foreground_command_active,
                    parser.has_prompt_painted_since_ready(),
                    last_pty_output,
                ),
            ),
        )?;
        if activity.resize {
            sync_outer_terminal_winsize(master.as_raw_fd(), child.id(), last_winsize)?;
        }
    }
}

fn latest_capture_submission_generation(events: &[ShellEvent]) -> Option<u64> {
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
fn observe_with_input_mode_lock<W: Write, F>(
    event_observer: &mut F,
    parser: &mut OscParser,
    output: &mut W,
    input_mode: &Arc<Mutex<RawInputMode>>,
) -> io::Result<RawObserverAction>
where
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
{
    let Ok(mut mode) = input_mode.lock() else {
        return parser.observe_events(output, event_observer);
    };
    let action = parser.observe_events(output, event_observer)?;
    let acknowledged = latest_capture_submission_generation(&parser.events);
    update_locked_input_mode(&mut mode, &action, acknowledged);
    Ok(action)
}

fn release_held_shell_output<W: Write, F>(
    event_observer: &mut F,
    parser: &mut OscParser,
    output: &mut W,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
) -> io::Result<()>
where
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
{
    drain_observer_until_released(event_observer, parser, output)?;
    if parser.display_position() > *display_start {
        write_pending_display(
            parser,
            output,
            display_start,
            prompt_replay,
            prompt_presentation,
        )?;
        output.flush()?;
    }
    Ok(())
}

fn write_pending_display<W: Write>(
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

fn write_pending_display_preserving_prompt_ghost<W: Write>(
    parser: &OscParser,
    output: &mut W,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
    input_mode: &Arc<Mutex<RawInputMode>>,
) -> io::Result<()> {
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

fn write_prompt_ghost<W: Write>(output: &mut W, text: &str, selection: bool) -> io::Result<()> {
    let marker = if selection { " ›" } else { "" };
    write_no_wrap_overlay(output, |output| write!(output, "\x1b[2m{marker} {text}"))
}

fn drain_observer_until_released<W: Write, F>(
    event_observer: &mut F,
    parser: &mut OscParser,
    output: &mut W,
) -> io::Result<()>
where
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
{
    for _ in 0..1_000 {
        if !parser
            .observe_events(output, event_observer)?
            .hold_shell_output()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn clear_prompt_ghost_line<W: Write>(
    parser: &OscParser,
    output: &mut W,
    fallback_prompt: &str,
    native_candidate_echoed_len: &mut usize,
    prompt_presentation: &PromptPresentation,
) -> io::Result<()> {
    write!(output, "\r\x1b[2K")?;
    let replay = prompt_replay_bytes(parser.last_prompt_display());
    if replay.is_empty() {
        prompt_presentation.write_replayed_prompt(output, fallback_prompt.as_bytes())?;
    } else {
        prompt_presentation.write_replayed_prompt(output, replay)?;
    }
    *native_candidate_echoed_len = 0;
    output.flush()
}

fn shell_has_active_foreground_command(events: &[ShellEvent]) -> bool {
    let mut active = std::collections::HashSet::new();
    for event in events {
        let Some(command_id) = event.command_id.as_ref() else {
            continue;
        };
        match event.kind {
            ShellEventKind::CommandStarted => {
                active.insert(command_id.as_str());
            }
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed => {
                active.remove(command_id.as_str());
            }
            _ => {}
        }
    }
    !active.is_empty()
}

fn shell_has_completed_foreground_command(events: &[ShellEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
        )
    })
}

fn merge_pending_prompt_restore(
    observed: RawObserverAction,
    pending: &mut Option<RawObserverAction>,
) -> RawObserverAction {
    match observed {
        action @ RawObserverAction::RestorePrompt { .. } => {
            pending.take();
            action
        }
        action @ RawObserverAction::Continue => pending.take().unwrap_or(action),
        action => {
            pending.take();
            action
        }
    }
}

fn remember_pending_prompt_restore(
    action: &RawObserverAction,
    pending: &mut Option<RawObserverAction>,
) {
    if matches!(action, RawObserverAction::RestorePrompt { .. }) {
        *pending = Some(action.clone());
    }
}

fn mark_pending_prompt_replayed(
    parser: &OscParser,
    prompt: &[u8],
    display_start: &mut usize,
) -> io::Result<()> {
    if prompt.is_empty() || *display_start > parser.display_position() {
        return Ok(());
    }
    if parser.display_starts_with_at(*display_start, prompt)? {
        *display_start += prompt.len();
    }
    Ok(())
}

#[cfg(test)]
#[path = "raw_relay_tests.rs"]
mod tests;
