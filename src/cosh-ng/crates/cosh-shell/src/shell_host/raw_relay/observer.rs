use super::*;

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
    reset: &CaptureRepresentationReset,
    before_observe: &mut B,
) -> io::Result<RawObserverAction>
where
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
    B: FnMut(&mut OscParser, &mut W) -> io::Result<()>,
{
    reset.take_committed_emit();
    let Ok(mut mode) = input_mode.lock() else {
        before_observe(parser, output)?;
        return parser.observe_events(output, event_observer);
    };
    let mut action =
        before_observe(parser, output).and_then(|()| parser.observe_events(output, event_observer));
    if let Err(error) = action {
        action = reset
            .defer_physical_error(error)
            .map(|()| RawObserverAction::HoldShellOutput);
    }
    let committed_emit = reset.has_committed_emit()
        && matches!(
            action,
            Ok(RawObserverAction::EmitToPty(_) | RawObserverAction::EmitToPtyWithPromptRestore(_))
        );
    if reset.display_is_failed()
        && action.is_ok()
        && !committed_emit
        && !matches!(action, Ok(RawObserverAction::InterruptForeground))
    {
        action = Ok(RawObserverAction::HoldShellOutput);
    }
    if !matches!(
        &action,
        Ok(RawObserverAction::EmitToPty(_) | RawObserverAction::EmitToPtyWithPromptRestore(_))
    ) {
        reset.take_committed_emit();
    }
    let acknowledged = latest_capture_submission_generation(&parser.events);
    // Retire before applying this pass's action, so the `CaptureInput` the observer
    // re-issues for the rebuilt card mints a fresh generation. See the handle's doc.
    if reset.consume() {
        retire_capture_for_representation(&mut mode);
    }
    let action = action?;
    update_locked_input_mode(&mut mode, &action, acknowledged);
    if reset.display_is_failed() {
        *mode = RawInputMode::Hold;
    }
    Ok(action)
}

pub(super) fn release_held_shell_output<W: Write, F>(
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

pub(super) fn drain_observer_until_released<W: Write, F>(
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

pub(super) fn merge_pending_prompt_restore(
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

pub(super) fn remember_pending_prompt_restore(
    action: &RawObserverAction,
    pending: &mut Option<RawObserverAction>,
) {
    if matches!(action, RawObserverAction::RestorePrompt { .. }) {
        *pending = Some(action.clone());
    }
}
