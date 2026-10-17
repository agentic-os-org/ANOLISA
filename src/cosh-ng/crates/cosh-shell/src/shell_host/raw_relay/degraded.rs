use std::io::{self, Write};
use std::sync::atomic::Ordering;

use super::{retire_capture_for_representation, CaptureRepresentationReset, RawInputMode};

#[derive(Debug)]
struct PhysicalDisplayError(io::Error);
impl std::fmt::Display for PhysicalDisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for PhysicalDisplayError {}

pub(crate) struct PhysicalDisplayWriter<'a, W>(pub(crate) &'a mut W);
impl<W: Write> Write for PhysicalDisplayWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.0.write(bytes) {
            Ok(0) if !bytes.is_empty() => Err(tag_physical_error(io::Error::new(
                io::ErrorKind::WriteZero,
                "physical display wrote zero bytes",
            ))),
            result => result.map_err(tag_physical_error),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush().map_err(tag_physical_error)
    }
}
pub(crate) fn tag_physical_error(error: io::Error) -> io::Error {
    if physical_error(&error) {
        error
    } else {
        io::Error::new(error.kind(), PhysicalDisplayError(error))
    }
}
pub(crate) fn physical_error(error: &io::Error) -> bool {
    let mut cause: &(dyn std::error::Error + 'static) = error;
    loop {
        if cause.is::<PhysicalDisplayError>() {
            return true;
        }
        let inner = cause
            .downcast_ref::<io::Error>()
            .and_then(|error| error.get_ref())
            .map(|error| error as &(dyn std::error::Error + 'static))
            .or_else(|| cause.source());
        match inner {
            Some(inner) => cause = inner,
            None => return false,
        }
    }
}

impl CaptureRepresentationReset {
    pub(crate) fn bind_display_handoff(&self, token: &str) {
        if !token.is_empty() {
            *self.display_handoff.lock().expect("display handoff lock") = Some(token.to_owned());
        }
    }
    pub(crate) fn display_handoff_token(&self) -> Option<String> {
        self.display_handoff
            .lock()
            .expect("display handoff lock")
            .clone()
    }
    pub(crate) fn display_is_failed(&self) -> bool {
        self.display_failed.load(Ordering::Acquire)
    }
    pub(crate) fn display_prompt_restore_pending(&self) -> bool {
        self.display_prompt_restore_pending.load(Ordering::Acquire)
    }
    pub(crate) fn defer_physical_error(&self, error: io::Error) -> io::Result<()> {
        if !physical_error(&error) || self.display_handoff_token().is_none() {
            return Err(error);
        }
        self.display_prompt_restore_pending
            .store(true, Ordering::Release);
        if !self.display_failed.swap(true, Ordering::AcqRel) {
            tracing::warn!(error = ?error, "approved handoff physical display failed; preserving OSC settlement");
        }
        self.signal();
        Ok(())
    }
    pub(crate) fn mark_committed_display_failure(&self) {
        self.display_prompt_restore_pending
            .store(true, Ordering::Release);
        self.display_failed.store(true, Ordering::Release);
        self.signal();
    }
    pub(crate) fn end_display_handoff(&self) {
        self.display_prompt_restore_pending
            .store(false, Ordering::Release);
        *self.display_handoff.lock().expect("display handoff lock") = None;
    }
}

pub(super) fn display<W: Write, F>(
    reset: &CaptureRepresentationReset,
    output: &mut W,
    mode: &std::sync::Arc<std::sync::Mutex<RawInputMode>>,
    body: F,
) -> io::Result<()>
where
    F: FnOnce(&mut PhysicalDisplayWriter<'_, W>) -> io::Result<()>,
{
    if let Err(error) = body(&mut PhysicalDisplayWriter(output)) {
        reset.defer_physical_error(error)?;
        if let Ok(mut mode) = mode.lock() {
            retire_capture_for_representation(&mut mode);
            *mode = RawInputMode::Hold;
        }
    }
    Ok(())
}

pub(super) fn recover<W: Write>(
    reset: &CaptureRepresentationReset,
    output: &mut W,
    rows: u16,
) -> io::Result<bool> {
    if !reset.display_is_failed() {
        return Ok(false);
    }
    let mut physical = PhysicalDisplayWriter(output);
    let result = (|| {
        physical.write_all(b"\x18\x1b[0m\x1b[?7h\x1b[?25h\r\n")?;
        for _ in 0..rows.max(1) {
            physical.write_all(b"\r\n")?;
        }
        physical.flush()
    })();
    if let Err(error) = result {
        reset.defer_physical_error(error)?;
        return Ok(false);
    }
    reset.display_failed.store(false, Ordering::Release);
    reset.signal();
    reset.notify_resize();
    Ok(true)
}

#[cfg(test)]
#[path = "degraded_tests.rs"]
pub(crate) mod component_fixture;

pub(super) fn update_input_mode_after_display(
    reset: &CaptureRepresentationReset,
    mode: &std::sync::Arc<std::sync::Mutex<RawInputMode>>,
    action: &crate::raw_input::RawObserverAction,
    acknowledged: Option<u64>,
) {
    if reset.display_is_failed() || reset.display_prompt_restore_pending() {
        if let Ok(mut mode) = mode.lock() {
            *mode = RawInputMode::Hold;
        }
    } else {
        crate::raw_input::update_input_mode(mode, action, acknowledged);
    }
}
