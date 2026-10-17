use super::*;
use std::sync::{Arc, Mutex};

struct FailedDisplay;
impl Write for FailedDisplay {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(5))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(5))
    }
}

pub(crate) fn physical_failures_require_an_emitted_token_and_never_mask_other_errors() {
    let reset = CaptureRepresentationReset::default();
    let mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    assert!(display(&reset, &mut FailedDisplay, &mode, |output| output.flush()).is_err());
    assert!(!reset.display_is_failed());
    reset.bind_display_handoff("approved-token");
    display(&reset, &mut FailedDisplay, &mode, |output| output.flush())
        .expect("defer display only");
    assert!(reset.display_is_failed());
    assert_eq!(*mode.lock().unwrap(), RawInputMode::Hold);
    assert!(display(&reset, &mut Vec::new(), &mode, |_| {
        Err(io::Error::other("parser or storage failure"))
    })
    .is_err());
    assert!(!recover(&reset, &mut FailedDisplay, 24).expect("still failed"));
    assert!(reset.display_is_failed());
    let mut healthy = Vec::new();
    assert!(recover(&reset, &mut healthy, 24).expect("actual fresh flush"));
    assert!(!reset.display_is_failed());
    update_input_mode_after_display(
        &reset,
        &mode,
        &crate::raw_input::RawObserverAction::Continue,
        None,
    );
    assert_eq!(
        *mode.lock().unwrap(),
        RawInputMode::Hold,
        "a fresh region cannot release input before the original prompt is flushed"
    );
    assert!(healthy.starts_with(b"\x18"));
    assert!(
        healthy.windows(6).any(|bytes| bytes == b"\x1b[?25h"),
        "physical recovery must restore the caret hidden by the worker"
    );
    assert_eq!(healthy.iter().filter(|byte| **byte == b'\n').count(), 25);
    assert_eq!(
        *mode.lock().unwrap(),
        RawInputMode::Hold,
        "recovery alone never grants input"
    );
    display(&reset, &mut FailedDisplay, &mode, |output| output.flush()).unwrap();
    assert!(
        reset.display_is_failed(),
        "timeout is tested during actual physical failure"
    );
    let root = tempfile::tempdir().unwrap();
    let mut parser = super::super::OscParser::new(
        "timeout-hold".into(),
        root.path().to_owned(),
        "token".into(),
    );
    let action = super::super::observe_with_input_mode_lock(
        &mut |_, _: &mut Vec<u8>| Ok(crate::raw_input::RawObserverAction::InterruptForeground),
        &mut parser,
        &mut Vec::new(),
        &mode,
        &reset,
        &mut |_, _| Ok(()),
    )
    .expect("timeout action must be preserved");
    assert_eq!(
        action,
        crate::raw_input::RawObserverAction::InterruptForeground
    );
    let observed_mode = mode.lock().unwrap().clone();
    *mode.lock().unwrap() = RawInputMode::Hold;
    update_input_mode_after_display(
        &reset,
        &mode,
        &crate::raw_input::RawObserverAction::Continue,
        None,
    );
    let resolved_mode = mode.lock().unwrap().clone();
    assert_eq!(
        (observed_mode, resolved_mode),
        (RawInputMode::Hold, RawInputMode::Hold),
        "observer and post-resolve update must both preserve Hold during display failure"
    );
    let request = crate::types::ShellHandoffRequest::new(
        "printf authorized",
        "$ printf authorized",
        "approved_provider_shell_tool",
        "user",
        "approval",
        "run",
        1,
    )
    .unwrap();
    let action = super::super::observe_with_input_mode_lock(
        &mut |_, _: &mut Vec<u8>| {
            reset.permit_committed_emit();
            Ok(crate::raw_input::RawObserverAction::EmitToPty(
                request.clone(),
            ))
        },
        &mut parser,
        &mut Vec::new(),
        &mode,
        &reset,
        &mut |_, _| Ok(()),
    )
    .unwrap();
    assert!(
        matches!(action, crate::raw_input::RawObserverAction::EmitToPty(_)),
        "approved permit must preserve actual effect while input stays held"
    );
    assert_eq!(*mode.lock().unwrap(), RawInputMode::Hold);
    assert!(reset.take_committed_emit());
    reset.end_display_handoff();
    assert!(display(&reset, &mut FailedDisplay, &mode, |output| output.flush()).is_err());
}

pub(crate) fn queued_overlay_failure_stays_hold_and_preserves_eof() {
    let root = tempfile::tempdir().unwrap();
    let mut parser = super::super::OscParser::new(
        "degraded-drain".into(),
        root.path().to_owned(),
        "token".into(),
    );
    let reset = CaptureRepresentationReset::default();
    reset.bind_display_handoff("emitted-token");
    let mode = Arc::new(Mutex::new(RawInputMode::RawPassthrough));
    let (sender, receiver) = std::sync::mpsc::channel();
    sender
        .send(super::super::RawInputEvent::EofShutdownRequested)
        .unwrap();
    sender
        .send(super::super::RawInputEvent::CandidateRedraw {
            input: b"old ghost".to_vec(),
            hint: None,
        })
        .unwrap();
    let mut replay = super::super::PromptReplayTracker::new(Default::default());
    let presentation = super::super::PromptPresentation::new(false);
    assert!(super::super::drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut FailedDisplay,
        "prompt> ",
        &mut 0,
        &mut replay,
        &presentation,
        Some(&reset),
        Some(&mode)
    )
    .expect("defer overlay"));
    assert_eq!(*mode.lock().unwrap(), RawInputMode::Hold);
    assert!(reset.display_is_failed());
}

struct ZeroDisplay;
impl Write for ZeroDisplay {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(crate) fn zero_write_is_physical_failure_without_masking_storage_write_zero() {
    let reset = CaptureRepresentationReset::default();
    let mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let error = display(&reset, &mut ZeroDisplay, &mode, |output| {
        output.write_all(b"frame")
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    assert!(physical_error(&error));
    reset.bind_display_handoff("emitted-token");
    display(&reset, &mut ZeroDisplay, &mode, |output| {
        output.write_all(b"frame")
    })
    .expect("physical zero write deferred");
    assert_eq!(*mode.lock().unwrap(), RawInputMode::Hold);
    let error = display(&reset, &mut Vec::new(), &mode, |_| {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "storage zero write",
        ))
    })
    .unwrap_err();
    assert!(!physical_error(&error));
}
