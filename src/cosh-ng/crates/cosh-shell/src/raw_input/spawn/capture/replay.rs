use super::native_interrupt::{
    classify_native_ctrl_c, signal_native_replay_interrupt, NativeCtrlC,
};
use super::*;

pub(super) fn complete_and_replay_capture(
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    generation: u64,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    #[cfg(test)]
    before_replay_commit();
    let shared = relay.input_mode.clone();
    let mut mode = shared
        .lock()
        .map_err(|_| io::Error::other("raw input mode lock poisoned"))?;
    let installed = complete_locked_capture_replay(&mut mode, generation);
    capture_owned_input
        .replay_control
        .store(REPLAY_PENDING, Ordering::Release);
    let result = replay_or_reject_after_drain_locked(
        installed,
        card_state,
        capture_owned_input,
        generation,
        &mut mode,
        relay,
    );
    if !capture_owned_input.has_pending_replay() {
        capture_owned_input
            .replay_control
            .store(REPLAY_IDLE, Ordering::Release);
    }
    result
}

#[cfg(test)]
pub(super) fn replay_or_reject_after_drain(
    installed: Option<(RawInputMode, PostCaptureOwner, bool)>,
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    generation: u64,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    let shared = relay.input_mode.clone();
    let mut mode = shared
        .lock()
        .map_err(|_| io::Error::other("raw input mode lock poisoned"))?;
    replay_or_reject_after_drain_locked(
        installed,
        card_state,
        capture_owned_input,
        generation,
        &mut mode,
        relay,
    )
}

/// Deliver the quarantined bytes to the observer-acknowledged
/// post-capture owner; they re-enter the relay exactly like live input
/// read under that owner's snapshot (no PTY bypass, #1913 D4), so a
/// buffered Ctrl-C under a delay owner still cancels the agent instead
/// of leaking into bash. Unroutable landings (follow-up capture, prompt
/// ghost) reject visibly (fail-safe C4).
fn replay_or_reject_after_drain_locked(
    installed: Option<(RawInputMode, PostCaptureOwner, bool)>,
    card_state: &mut CardInputState,
    capture_owned_input: &mut CaptureOwnedInput,
    generation: u64,
    locked_mode: &mut RawInputMode,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    if installed
        .as_ref()
        .is_some_and(|(_, _, invalidated)| *invalidated)
    {
        reject_quarantined_input(capture_owned_input, generation, relay);
        return Ok(());
    }
    let bytes = capture_owned_input.take_bytes();
    if bytes.is_empty() {
        return Ok(());
    }
    let owner_alive = installed
        .as_ref()
        .is_some_and(|(mode, _, _)| locked_mode.input_ownership() == mode.input_ownership());
    let replay_mode = match installed {
        Some((mode @ RawInputMode::Terminal { .. }, PostCaptureOwner::MainPrompt, false))
        | Some((mode @ RawInputMode::Delay { .. }, PostCaptureOwner::Delay, false))
        | Some((mode @ RawInputMode::RawPassthrough, PostCaptureOwner::RawPassthrough, false))
            if owner_alive =>
        {
            Some(mode)
        }
        Some((RawInputMode::Hold, PostCaptureOwner::Hold, false)) if owner_alive => {
            // A hold owner has no consumer that could deliver ordinary
            // text (held input only recognizes cancel controls), so the
            // batch cannot count as delivered. Preserve the live-typing
            // cancel semantics for recognized controls, then surface the
            // batch as a visible rejection.
            send_held_input_events(&bytes, relay.input_events);
            None
        }
        _ => None,
    };
    let Some(mode) = replay_mode else {
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureInputRejected {
                generation,
                byte_len: bytes.len(),
            });
        return Ok(());
    };
    // Routing may lock its mode again; its working copy stays inside the real owner's lock.
    let working = Arc::new(Mutex::new(locked_mode.clone()));
    let mut context = InputRelayContext {
        master: relay.master,
        input_classifier: relay.input_classifier,
        input_events: relay.input_events,
        input_mode: &working,
        input_generation: relay.input_generation,
        line_submits: relay.line_submits,
        line_buffer: relay.line_buffer,
        native_line_state: relay.native_line_state,
        exit_tracker: relay.exit_tracker,
        main_prompt_gate: relay.main_prompt_gate,
        slash_route_enabled: relay.slash_route_enabled,
        zsh_path_prompt_buffering: relay.zsh_path_prompt_buffering.as_deref_mut(),
    };
    let mut committed_exit_tracker = context.exit_tracker.clone();
    let write_guard = NonblockingReplayWrite::enter(context.master)?;
    let mut deferred_input = None;
    let result = relay_input_chunk(
        &bytes,
        mode,
        card_state,
        capture_owned_input,
        &mut deferred_input,
        RelayReadContext::default(),
        &mut context,
    );
    *locked_mode = working
        .lock()
        .map_err(|_| io::Error::other("replay mode lock poisoned"))?
        .clone();
    let deferred_bytes = write_guard.take_deferred();
    let committed_bytes = write_guard.take_committed();
    if result.is_ok() && !deferred_bytes.is_empty() {
        committed_exit_tracker.observe_shell_bytes(&committed_bytes);
        let exit_tracker_after_replay =
            std::mem::replace(context.exit_tracker, committed_exit_tracker);
        let native_line_state = context.native_line_state.clone();
        context.native_line_state.invalidate();
        capture_owned_input.pending_replay = Some(PendingCaptureReplay {
            bytes: deferred_bytes,
            offset: 0,
            generation,
            owner: locked_mode.clone(),
            native_line_state,
            exit_tracker_after_replay,
        });
    }
    result
}

pub(super) fn flush_pending_capture_replay(
    capture_owned_input: &mut CaptureOwnedInput,
    relay: &mut InputRelayContext<'_>,
) -> io::Result<()> {
    let Some(mut pending) = capture_owned_input.pending_replay.take() else {
        return Ok(());
    };
    let mut mode = relay
        .input_mode
        .lock()
        .map_err(|_| io::Error::other("raw input mode lock poisoned"))?;
    if !same_replay_owner(&pending.owner, &mode) {
        if matches!(&*mode, RawInputMode::Draining { generation, .. } if *generation == pending.generation)
        {
            complete_locked_capture_replay(&mut mode, pending.generation);
        }
        drop(mode);
        capture_owned_input
            .replay_control
            .store(REPLAY_IDLE, Ordering::Release);
        let _ = relay
            .input_events
            .send(RawInputEvent::CaptureInputRejected {
                generation: pending.generation,
                byte_len: pending.bytes.len() - pending.offset,
            });
        let _ = relay.input_events.send(RawInputEvent::CaptureDrained {
            generation: pending.generation,
        });
        return Ok(());
    }
    while pending.offset < pending.bytes.len() {
        match std::io::Write::write(relay.master, &pending.bytes[pending.offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write capture replay to PTY",
                ))
            }
            Ok(count) => {
                relay
                    .exit_tracker
                    .observe_shell_bytes(&pending.bytes[pending.offset..pending.offset + count]);
                pending.offset += count;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                capture_owned_input.pending_replay = Some(pending);
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
    *relay.native_line_state = pending.native_line_state;
    *relay.exit_tracker = pending.exit_tracker_after_replay;
    drop(mode);
    capture_owned_input
        .replay_control
        .store(REPLAY_IDLE, Ordering::Release);
    let _ = relay.input_events.send(RawInputEvent::CaptureDrained {
        generation: pending.generation,
    });
    Ok(())
}

fn same_replay_owner(owner: &RawInputMode, observed: &RawInputMode) -> bool {
    owner.input_ownership() == observed.input_ownership()
        || (matches!(owner, RawInputMode::Terminal { .. })
            && matches!(observed, RawInputMode::Passthrough))
}

fn input_has_new_replay_owner(input: &InputRead, owner: &RawInputMode) -> bool {
    matches!(input, InputRead::Bytes { observed_mode, ownership_changed_during_read, .. }
        if *ownership_changed_during_read || !same_replay_owner(owner, observed_mode))
}

fn input_byte_len(input: &InputRead) -> usize {
    match input {
        InputRead::Bytes { bytes, .. } => bytes.len(),
        _ => 0,
    }
}

pub(super) fn is_replay_control(input: &InputRead, policy: NativeCtrlC) -> bool {
    match input {
        InputRead::Bytes {
            bytes,
            observed_mode,
            ..
        } => {
            bytes.contains(&super::super::super::CTRL_C)
                && !(policy == NativeCtrlC::Literal && is_native_replay_owner(observed_mode))
        }
        InputRead::Eof | InputRead::Error(_) => true,
        InputRead::CaptureInvalidated => false,
    }
}

fn queue_pending_replay_input(
    input: InputRead,
    state: &mut RawInputRelayState,
    generation: u64,
    input_events: &dyn RawInputEventSink,
    front: bool,
    owner: &RawInputMode,
    policy: NativeCtrlC,
) {
    if matches!(input, InputRead::CaptureInvalidated) {
        return;
    }
    let byte_len = input_byte_len(&input);
    // A queued control read has its own bounded reader chunk; only ordinary bytes consume this budget.
    if !is_replay_control(&input, policy)
        && state.pending_replay_input_bytes + byte_len > CAPTURE_QUARANTINE_MAX_BYTES
    {
        let control_bytes: usize = state
            .pending_replay_input
            .iter()
            .filter(|queued| is_replay_control(queued, policy))
            .map(input_byte_len)
            .sum();
        if state.pending_replay_input_bytes - control_bytes + byte_len
            > CAPTURE_QUARANTINE_MAX_BYTES
        {
            let mut rejected = 0;
            state.pending_replay_input.retain(|queued| {
                if !is_replay_control(queued, policy) && !input_has_new_replay_owner(queued, owner)
                {
                    rejected += input_byte_len(queued);
                    false
                } else {
                    true
                }
            });
            state.pending_replay_input_bytes -= rejected;
            let reject_incoming = !input_has_new_replay_owner(&input, owner);
            if reject_incoming {
                rejected += byte_len;
            }
            let _ = input_events.send(RawInputEvent::CaptureOverflow { generation });
            let _ = input_events.send(RawInputEvent::CaptureInputRejected {
                generation,
                byte_len: rejected,
            });
            if reject_incoming {
                return;
            }
        }
    }
    state.pending_replay_input_bytes += byte_len;
    if front {
        state.pending_replay_input.push_front(input);
    } else {
        state.pending_replay_input.push_back(input);
    }
}

fn reject_pending_replay_prefix(
    state: &mut RawInputRelayState,
    owner: &RawInputMode,
    generation: u64,
    input_events: &dyn RawInputEventSink,
    prefix_len: usize,
) -> usize {
    let mut retained = std::collections::VecDeque::new();
    let mut rejected = 0;
    for _ in 0..prefix_len {
        let Some(input) = state.pending_replay_input.pop_front() else {
            break;
        };
        let old_owner = matches!(&input, InputRead::Bytes { observed_mode, ownership_changed_during_read: false, .. }
            if same_replay_owner(owner, observed_mode));
        if old_owner {
            rejected += input_byte_len(&input);
        } else {
            retained.push_back(input);
        }
    }
    let retained_len = retained.len();
    retained.append(&mut state.pending_replay_input);
    state.pending_replay_input = retained;
    state.pending_replay_input_bytes -= rejected;
    if rejected > 0 {
        let _ = input_events.send(RawInputEvent::CaptureInputRejected {
            generation,
            byte_len: rejected,
        });
    }
    retained_len
}

fn cancel_pending_replay(state: &mut RawInputRelayState, input_events: &dyn RawInputEventSink) {
    if let Some(pending) = state.capture_owned_input.pending_replay.take() {
        let _ = input_events.send(RawInputEvent::CaptureInputRejected {
            generation: pending.generation,
            byte_len: pending.bytes.len() - pending.offset,
        });
        let _ = input_events.send(RawInputEvent::CaptureDrained {
            generation: pending.generation,
        });
    }
    state
        .capture_owned_input
        .replay_control
        .store(REPLAY_IDLE, Ordering::Release);
}

fn is_native_replay_owner(mode: &RawInputMode) -> bool {
    matches!(
        mode,
        RawInputMode::Passthrough | RawInputMode::RawPassthrough | RawInputMode::Terminal { .. }
    )
}

pub(super) fn replay_hint_blocks_writing(
    hint: u8,
    owner: &RawInputMode,
    policy: NativeCtrlC,
) -> bool {
    hint == REPLAY_EOF
        || (hint == REPLAY_CTRL_C
            && !(policy == NativeCtrlC::Literal && is_native_replay_owner(owner)))
}

pub(in super::super) fn replay_mode_accepts_interrupt(mode: &RawInputMode) -> bool {
    matches!(
        mode,
        RawInputMode::Passthrough
            | RawInputMode::RawPassthrough
            | RawInputMode::Terminal { .. }
            | RawInputMode::Delay { .. }
    )
}

#[cfg(test)]
pub(super) fn consume_pending_replay_control(
    master: &File,
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
    read_ahead: Option<&Receiver<InputRead>>,
) -> io::Result<bool> {
    let policy = classify_native_ctrl_c(state.native_terminal.as_ref())?;
    consume_pending_replay_control_with_policy(
        master,
        input_events,
        input_mode,
        state,
        read_ahead,
        policy,
    )
}

fn consume_pending_replay_control_with_policy(
    master: &File,
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
    read_ahead: Option<&Receiver<InputRead>>,
    policy: NativeCtrlC,
) -> io::Result<bool> {
    let Some(pending) = state.capture_owned_input.pending_replay.as_ref() else {
        return Ok(false);
    };
    let owner = pending.owner.clone();
    let generation = pending.generation;
    let new_deferred_owner = state.deferred_input.as_ref().is_some_and(|input| {
        input_has_new_replay_owner(input, &owner) && !is_replay_control(input, policy)
    });
    let new_queued_owner = state
        .pending_replay_input
        .iter()
        .any(|input| input_has_new_replay_owner(input, &owner));
    let mut control_ready = state
        .pending_replay_input
        .iter()
        .any(|input| is_replay_control(input, policy))
        || state
            .deferred_input
            .as_ref()
            .is_some_and(|input| is_replay_control(input, policy));
    if new_deferred_owner || (new_queued_owner && !control_ready) {
        let count = state.pending_replay_input.len();
        reject_pending_replay_prefix(state, &owner, generation, input_events, count);
        if !new_deferred_owner {
            if let Some(input) = state.deferred_input.take() {
                let byte_len = input_byte_len(&input);
                if byte_len > 0 {
                    let _ = input_events.send(RawInputEvent::CaptureInputRejected {
                        generation,
                        byte_len,
                    });
                }
            }
        }
        cancel_pending_replay(state, input_events);
        return Ok(true);
    }
    if let Some(input) = state.deferred_input.take() {
        control_ready |= is_replay_control(&input, policy);
        let front = !matches!(input, InputRead::Eof | InputRead::Error(_));
        queue_pending_replay_input(
            input,
            state,
            generation,
            input_events,
            front,
            &owner,
            policy,
        );
    }
    while !control_ready {
        let input = match read_ahead.map(Receiver::try_recv) {
            Some(Ok(input)) => input,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => InputRead::Eof,
            _ => break,
        };
        control_ready = is_replay_control(&input, policy);
        if input_has_new_replay_owner(&input, &owner) && !control_ready {
            let count = state.pending_replay_input.len();
            reject_pending_replay_prefix(state, &owner, generation, input_events, count);
            cancel_pending_replay(state, input_events);
            queue_pending_replay_input(
                input,
                state,
                generation,
                input_events,
                false,
                &owner,
                policy,
            );
            return Ok(true);
        }
        queue_pending_replay_input(
            input,
            state,
            generation,
            input_events,
            false,
            &owner,
            policy,
        );
    }
    if let Some(index) = state
        .pending_replay_input
        .iter()
        .position(|input| matches!(input, InputRead::Eof | InputRead::Error(_)))
    {
        reject_pending_replay_prefix(state, &owner, generation, input_events, index);
        cancel_pending_replay(state, input_events);
        if state.pending_replay_input.len() == 1 {
            state.deferred_input = state.pending_replay_input.pop_front();
        }
        return Ok(true);
    }
    if !same_replay_owner(&owner, &current_raw_input_mode(input_mode)) {
        return Ok(false);
    }
    if policy == NativeCtrlC::Literal && is_native_replay_owner(&owner) {
        return Ok(false);
    }
    let index = state.pending_replay_input.iter().position(|input| {
        matches!(input,
        InputRead::Bytes { bytes, observed_mode, ownership_changed_during_read: false, .. }
        if same_replay_owner(&owner, observed_mode) && replay_mode_accepts_interrupt(observed_mode)
            && bytes.contains(&super::super::super::CTRL_C))
    });
    let Some(index) = index else { return Ok(false) };
    let signaled = if is_native_replay_owner(&owner) {
        match state.native_terminal.as_ref() {
            Some(terminal) => signal_native_replay_interrupt(master, terminal, policy)?,
            None => false,
        }
    } else {
        false
    };
    let Some(InputRead::Bytes {
        mut bytes,
        received_at,
        observed_mode,
        ownership_changed_during_read,
        pending_shell_submits,
    }) = state.pending_replay_input.remove(index)
    else {
        return Ok(false);
    };
    state.pending_replay_input_bytes -= bytes.len();
    let retained_prefix =
        reject_pending_replay_prefix(state, &owner, generation, input_events, index);
    cancel_pending_replay(state, input_events);
    state.line_buffer.clear();
    state.native_line_state.clear();
    if signaled {
        let _ = input_events.send(RawInputEvent::CtrlC);
    }
    let position = bytes
        .iter()
        .position(|byte| *byte == super::super::super::CTRL_C)
        .unwrap_or(0);
    bytes.drain(..position + usize::from(signaled));
    if !bytes.is_empty() {
        state.pending_replay_input_bytes += bytes.len();
        state.pending_replay_input.insert(
            retained_prefix,
            InputRead::Bytes {
                bytes,
                received_at,
                observed_mode,
                ownership_changed_during_read,
                pending_shell_submits,
            },
        );
    }
    Ok(true)
}

pub(in super::super) fn wait_for_pending_capture_replay(
    master: &mut File,
    input_classifier: &InputClassifier,
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
    wake: Option<&UnixStream>,
    read_ahead: Option<&Receiver<InputRead>>,
) -> io::Result<()> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    while state.capture_owned_input.has_pending_replay() {
        let policy = classify_native_ctrl_c(state.native_terminal.as_ref())?;
        if consume_pending_replay_control_with_policy(
            master,
            input_events,
            input_mode,
            state,
            read_ahead,
            policy,
        )? {
            return Ok(());
        }
        if state
            .pending_replay_input
            .iter()
            .any(|input| is_replay_control(input, policy))
        {
            if let Some(pending) = state.capture_owned_input.pending_replay.as_ref() {
                let owner = pending.owner.clone();
                let generation = pending.generation;
                let count = state.pending_replay_input.len();
                reject_pending_replay_prefix(state, &owner, generation, input_events, count);
                cancel_pending_replay(state, input_events);
            }
            return Ok(());
        }
        let pending_owner = state
            .capture_owned_input
            .pending_replay
            .as_ref()
            .map(|pending| (pending.owner.clone(), pending.generation));
        // The reader hints before publishing its read; literal terminal bytes never block replay.
        let hint = state
            .capture_owned_input
            .replay_control
            .load(Ordering::Acquire);
        let control_in_flight = pending_owner
            .as_ref()
            .is_some_and(|(owner, _)| replay_hint_blocks_writing(hint, owner, policy));
        if !control_in_flight {
            let mut quarantine = std::mem::take(&mut state.capture_owned_input);
            let mut relay = super::super::input_relay_context(
                master,
                input_classifier,
                input_events,
                input_mode,
                state,
            );
            let result = flush_pending_capture_replay(&mut quarantine, &mut relay);
            state.capture_owned_input = quarantine;
            result?;
            if let Some((owner, generation)) = pending_owner {
                let mode = current_raw_input_mode(input_mode);
                if !same_replay_owner(&owner, &mode) {
                    let count = state.pending_replay_input.len();
                    reject_pending_replay_prefix(state, &owner, generation, input_events, count);
                }
            }
        }
        if !state.capture_owned_input.has_pending_replay() {
            break;
        }
        let mut descriptors = [
            nix::libc::pollfd {
                fd: master.as_raw_fd(),
                events: nix::libc::POLLOUT,
                revents: 0,
            },
            nix::libc::pollfd {
                fd: wake.map_or(-1, AsRawFd::as_raw_fd),
                events: nix::libc::POLLIN,
                revents: 0,
            },
        ];
        // The reader also consumes this wake socket; recheck ownership only while output is pending.
        let ready =
            unsafe { nix::libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 25) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if descriptors[1].revents & nix::libc::POLLIN != 0 {
            if let Some(mut wake) = wake {
                let mut bytes = [0; 64];
                match wake.read(&mut bytes) {
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(())
}
