use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::libc;

use crate::input::InputClassifier;

use super::super::generation::UserPtyInputGeneration;
use super::super::mode::{current_raw_input_mode, RawInputMode};
use super::super::pty::{set_pty_winsize, signal_process_group};
use super::super::{MainPromptGate, RawInputEvent, RawRelayAction, ESC};
use super::deadline::next_pending_deadline;
use super::{
    finish_input_relay, flush_deferred_zsh_tab_typeahead, flush_pending_prompt_ghost_escape,
    flush_pending_replaced_prompt_ghost_suffix, is_pending_shell_submission,
    relay_input_bytes_with_read_ahead, relay_input_for_mode, should_split_passthrough_batch,
    RawInputEventSink, RawInputRelayState, RawInputShellRoute, RelayReadContext,
    WakingRawInputEventSender,
};

pub(super) struct PendingDelayEscape {
    pub(super) bytes: Vec<u8>,
    pub(super) deadline: Instant,
    pub(super) generation: u64,
}

pub(super) fn stale_delay_escape_reached_interactive_owner(
    bytes: &[u8],
    observed_mode: &RawInputMode,
    current_mode: &RawInputMode,
) -> bool {
    bytes == [ESC]
        && matches!(observed_mode, RawInputMode::Delay { .. })
        && matches!(
            current_mode,
            RawInputMode::Capture { .. }
                | RawInputMode::Submitted { .. }
                | RawInputMode::Draining { .. }
                | RawInputMode::Terminal { .. }
                | RawInputMode::PromptGhost { .. }
        )
}

pub(in crate::raw_input) fn relay_input_bytes(
    bytes: &[u8],
    received_at: Instant,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    relay_input_bytes_with_read_ahead(
        bytes,
        received_at,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
        RelayReadContext::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_pending_delay_escape(
    pending: PendingDelayEscape,
    bytes: &[u8],
    received_at: Instant,
    mode: &RawInputMode,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
    read_context: RelayReadContext<'_>,
) -> io::Result<Option<Vec<u8>>> {
    if !matches!(
        mode,
        RawInputMode::Delay { generation } if *generation == pending.generation
    ) {
        if matches!(
            mode,
            RawInputMode::Passthrough | RawInputMode::RawPassthrough
        ) {
            relay_input_for_mode(
                &pending.bytes,
                mode.clone(),
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                RelayReadContext::default(),
            )?;
        }
        relay_input_bytes_with_read_ahead(
            bytes,
            received_at,
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
            read_context,
        )?;
        return Ok(None);
    }
    if received_at > pending.deadline {
        let _ = input_events.send(RawInputEvent::Esc);
        relay_input_bytes_with_read_ahead(
            bytes,
            received_at,
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
            read_context,
        )?;
        return Ok(None);
    }
    let mut combined = pending.bytes;
    combined.extend_from_slice(bytes);
    Ok(Some(combined))
}

pub(super) fn flush_pending_delay_escape(
    force: bool,
    now: Instant,
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    let should_flush = state
        .pending_delay_escape
        .as_ref()
        .is_some_and(|pending| force || now >= pending.deadline);
    if !should_flush {
        return Ok(());
    }
    let Some(pending) = state.pending_delay_escape.take() else {
        return Ok(());
    };
    let mode = super::super::mode::current_raw_input_mode(input_mode);
    match mode {
        RawInputMode::Delay { generation } if generation == pending.generation => {
            let _ = input_events.send(RawInputEvent::Esc);
            return Ok(());
        }
        RawInputMode::Passthrough | RawInputMode::RawPassthrough => {}
        _ => return Ok(()),
    }
    relay_input_for_mode(
        &pending.bytes,
        mode,
        master,
        input_events,
        input_classifier,
        input_mode,
        state,
        RelayReadContext::default(),
    )
}

fn wait_for_raw_action(
    duration: Duration,
    master: &mut File,
    input_events: &WakingRawInputEventSender,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    let action_end = Instant::now() + duration;
    while let Some(deadline) = next_pending_deadline(state) {
        if deadline > action_end {
            break;
        }
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
        flush_deferred_zsh_tab_typeahead(
            false,
            Instant::now(),
            master,
            input_classifier,
            input_events,
            input_mode,
            state,
        )?;
        flush_pending_prompt_ghost_escape(
            false,
            Instant::now(),
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
        )?;
        flush_pending_delay_escape(
            false,
            Instant::now(),
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
        )?;
        let mode = super::super::mode::current_raw_input_mode(input_mode);
        flush_pending_replaced_prompt_ghost_suffix(
            false,
            Instant::now(),
            &mode,
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
        )?;
        input_events.notify_relay();
    }
    thread::sleep(action_end.saturating_duration_since(Instant::now()));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_raw_action_relay(
    actions: Vec<RawRelayAction>,
    master: File,
    child_pid: u32,
    input_events: Sender<RawInputEvent>,
    input_classifier: InputClassifier,
    input_mode: Arc<Mutex<RawInputMode>>,
    input_generation: UserPtyInputGeneration,
    main_prompt_gate: MainPromptGate,
    slash_route_enabled: bool,
) -> JoinHandle<io::Result<()>> {
    spawn_raw_action_relay_with_wake(
        actions,
        master,
        child_pid,
        input_events,
        input_classifier,
        input_mode,
        input_generation,
        RawInputShellRoute::new(main_prompt_gate, slash_route_enabled, None),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_raw_action_relay_with_wake(
    actions: Vec<RawRelayAction>,
    mut master: File,
    child_pid: u32,
    input_events: Sender<RawInputEvent>,
    input_classifier: InputClassifier,
    input_mode: Arc<Mutex<RawInputMode>>,
    input_generation: UserPtyInputGeneration,
    shell_route: RawInputShellRoute,
    wake: Option<UnixStream>,
) -> JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        let input_events = WakingRawInputEventSender::new(input_events, wake);
        let mut state = RawInputRelayState::with_shell_route(input_generation, shell_route);
        for action in actions {
            flush_deferred_zsh_tab_typeahead(
                false,
                Instant::now(),
                &mut master,
                &input_classifier,
                &input_events,
                &input_mode,
                &mut state,
            )?;
            flush_pending_prompt_ghost_escape(
                false,
                Instant::now(),
                &mut master,
                &input_events,
                &input_classifier,
                &input_mode,
                &mut state,
            )?;
            flush_pending_delay_escape(
                false,
                Instant::now(),
                &mut master,
                &input_events,
                &input_classifier,
                &input_mode,
                &mut state,
            )?;
            let mode = super::super::mode::current_raw_input_mode(&input_mode);
            flush_pending_replaced_prompt_ghost_suffix(
                false,
                Instant::now(),
                &mode,
                &mut master,
                &input_events,
                &input_classifier,
                &input_mode,
                &mut state,
            )?;
            match action {
                RawRelayAction::Write(bytes) => relay_action_write_batch(
                    &bytes,
                    &mut master,
                    &input_events,
                    &input_classifier,
                    &input_mode,
                    &mut state,
                )?,
                RawRelayAction::Resize(winsize) => {
                    set_pty_winsize(master.as_raw_fd(), winsize)?;
                    signal_process_group(child_pid, libc::SIGWINCH)?;
                }
                RawRelayAction::Wait(duration) => wait_for_raw_action(
                    duration,
                    &mut master,
                    &input_events,
                    &input_classifier,
                    &input_mode,
                    &mut state,
                )?,
            }
            input_events.notify_relay();
        }
        let result = finish_input_relay(
            &mut master,
            &input_events,
            &input_classifier,
            &input_mode,
            &mut state,
        );
        input_events.notify_relay();
        result
    })
}

fn relay_action_write_batch(
    bytes: &[u8],
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    let mode = current_raw_input_mode(input_mode);
    if !should_split_passthrough_batch(bytes, &mode, input_classifier, state) {
        return relay_input_bytes(
            bytes,
            Instant::now(),
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
        );
    }

    let mut offset = 0;
    let mut pending_shell_submits = 0usize;
    while offset < bytes.len() {
        let tail = &bytes[offset..];
        let Some(submit) = tail.iter().position(|byte| matches!(byte, b'\n' | b'\r')) else {
            relay_input_bytes_with_read_ahead(
                tail,
                Instant::now(),
                master,
                input_events,
                input_classifier,
                input_mode,
                state,
                RelayReadContext {
                    pending_shell_submits,
                    ..RelayReadContext::default()
                },
            )?;
            break;
        };
        let submit_len = if tail.get(submit) == Some(&b'\r') && tail.get(submit + 1) == Some(&b'\n')
        {
            2
        } else {
            1
        };
        let end = submit + submit_len;
        let line = &tail[..submit];
        let mode = current_raw_input_mode(input_mode);
        let shell_owned = matches!(mode, RawInputMode::Passthrough)
            && is_pending_shell_submission(line, state, input_classifier);
        relay_input_bytes_with_read_ahead(
            &tail[..end],
            Instant::now(),
            master,
            input_events,
            input_classifier,
            input_mode,
            state,
            RelayReadContext {
                pending_shell_submits,
                ..RelayReadContext::default()
            },
        )?;
        if shell_owned {
            pending_shell_submits = pending_shell_submits.saturating_add(1);
        }
        offset += end;
    }
    Ok(())
}
