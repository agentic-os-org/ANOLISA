//! Bounded line sink between the synchronous kernel writer and the ACP task.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;

/// Accepts kernel JSONL and hands whole frames to the projection task.
///
/// The kernel's `emit` discards write errors, so a failed `try_send` cannot be
/// reported through `io::Result`. Overflow is recorded in a shared flag that the
/// host task checks before answering the prompt, which keeps a slow consumer
/// from silently losing output.
pub(crate) struct EventSink {
    events: mpsc::Sender<Value>,
    buffer: Vec<u8>,
    max_line_bytes: usize,
    overflowed: Arc<AtomicBool>,
}

impl EventSink {
    pub(crate) fn new(
        events: mpsc::Sender<Value>,
        max_line_bytes: usize,
        overflowed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            events,
            buffer: Vec::new(),
            max_line_bytes,
            overflowed,
        }
    }

    fn finish_line(&mut self) {
        if self.buffer.len() > self.max_line_bytes {
            self.overflowed.store(true, Ordering::SeqCst);
            self.buffer.clear();
            return;
        }
        let parsed = serde_json::from_slice::<Value>(&self.buffer);
        self.buffer.clear();
        let Ok(value) = parsed else {
            self.overflowed.store(true, Ordering::SeqCst);
            return;
        };
        if self.events.try_send(value).is_err() {
            self.overflowed.store(true, Ordering::SeqCst);
        }
    }
}

impl Write for EventSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.overflowed.load(Ordering::SeqCst) {
            return Ok(buf.len());
        }
        let mut rest = buf;
        while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
            self.buffer.extend_from_slice(&rest[..end]);
            self.finish_line();
            rest = &rest[end + 1..];
            if self.overflowed.load(Ordering::SeqCst) {
                return Ok(buf.len());
            }
        }
        // Bound the partial line before growing it, so a peer that never sends
        // a newline cannot make this allocation unbounded.
        if self.buffer.len().saturating_add(rest.len()) > self.max_line_bytes {
            self.overflowed.store(true, Ordering::SeqCst);
            self.buffer.clear();
            return Ok(buf.len());
        }
        self.buffer.extend_from_slice(rest);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
