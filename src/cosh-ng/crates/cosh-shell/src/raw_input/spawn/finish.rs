use super::*;

pub(in super::super) fn finish_input_relay(
    master: &mut File,
    input_events: &dyn RawInputEventSink,
    input_classifier: &InputClassifier,
    input_mode: &Arc<Mutex<RawInputMode>>,
    state: &mut RawInputRelayState,
) -> io::Result<()> {
    if let Some(deadline) = state
        .zsh_path_prompt_buffering
        .as_ref()
        .and_then(|buffering| buffering.tab_typeahead_deadline())
    {
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
    flush_deferred_zsh_tab_typeahead(
        true,
        Instant::now(),
        master,
        input_classifier,
        input_events,
        input_mode,
        state,
    )?;
    // EOF is cancellation, not the timeout path. Pending escape/suffix bytes
    // are Cosh-owned lookahead and must never become PTY input during
    // shutdown.
    let current_mode = current_raw_input_mode(input_mode);
    let dismiss_prompt_ghost = state.pending_prompt_ghost_escape.take().is_some()
        || state.pending_replaced_prompt_ghost_suffix.take().is_some()
        || matches!(current_mode, RawInputMode::PromptGhost { .. });
    state.pending_delay_escape.take();
    state.pending_assistance_escape.take();
    if dismiss_prompt_ghost {
        let _ = input_events.send(RawInputEvent::CandidateClearLine);
        let _ = input_events.send(RawInputEvent::PromptGhostDismissed);
        if matches!(current_mode, RawInputMode::PromptGhost { .. }) {
            if let Ok(mut mode) = input_mode.lock() {
                *mode = RawInputMode::Passthrough;
            }
        }
    }
    if let RawInputMode::Submitted { generation, .. } = current_raw_input_mode(input_mode) {
        expire_capture_submission(input_mode, generation);
    }
    abandon_active_capture(input_mode);
    if matches!(
        current_raw_input_mode(input_mode),
        RawInputMode::Draining { .. }
    ) {
        let RawInputRelayState {
            card_state,
            line_buffer,
            native_line_state,
            exit_tracker,
            capture_owned_input,
            input_generation,
            line_submits,
            main_prompt_gate,
            zsh_path_prompt_buffering,
            ..
        } = state;
        let mut relay = InputRelayContext {
            master,
            input_classifier,
            input_events,
            input_mode,
            input_generation,
            line_submits,
            line_buffer,
            native_line_state,
            exit_tracker,
            main_prompt_gate,
            slash_route_enabled: false,
            zsh_path_prompt_buffering: zsh_path_prompt_buffering.as_mut(),
        };
        drain_abandoned_capture(card_state, capture_owned_input, &mut relay)?;
    }
    // Candidate bytes were never submitted to the Shell. EOF cancels them;
    // flushing a lone `?`, slash prefix, or partial paste delimiter before
    // `exit` would turn display state into executable input.
    let candidate_paste_active = state.line_buffer.in_paste();
    if state.line_buffer.is_active() {
        state.line_buffer.clear();
        let _ = input_events.send(RawInputEvent::CandidateClearLine);
    }
    if state.exit_tracker.saw_explicit_exit() {
        return Ok(());
    }
    if !candidate_paste_active && state.native_line_state.is_empty() {
        write_user_bytes_to_pty(
            master,
            &state.input_generation,
            &mut state.line_submits,
            input_events,
            &state.main_prompt_gate,
            b"exit\n",
        )?;
    } else {
        let _ = input_events.send(RawInputEvent::EofShutdownRequested);
    }
    Ok(())
}
