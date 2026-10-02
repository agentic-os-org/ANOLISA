//! Bounded readers for runtime stdout framing and diagnostic stderr tails.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

/// Failure while decoding a bounded newline-delimited frame.
#[derive(Debug, Error)]
pub enum BoundedLineError {
    /// Reading the underlying pipe failed.
    #[error("failed to read runtime stdout: {0}")]
    Io(#[from] io::Error),
    /// A peer exceeded the configured wire-frame limit.
    #[error("runtime stdout line exceeds the {limit}-byte limit")]
    TooLarge {
        /// Maximum accepted frame size, excluding the line delimiter.
        limit: usize,
    },
    /// The wire frame was not valid UTF-8.
    #[error("runtime stdout line is not valid UTF-8")]
    InvalidUtf8,
}

/// Newline-delimited reader that never allocates beyond one bounded frame.
#[derive(Debug)]
pub struct BoundedLineReader<R> {
    reader: BufReader<R>,
    max_line_bytes: usize,
}

/// Result of waiting for one line from an asynchronous bounded reader.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BoundedLineRead {
    /// One complete line was read.
    Line(String),
    /// The underlying stream reached EOF.
    Eof,
    /// No line became available within the caller's deadline.
    TimedOut,
}

/// Single-reader background adapter that keeps protocol control responsive.
#[derive(Debug)]
pub(crate) struct BoundedLineChannel {
    receiver: Option<Receiver<Result<Option<String>, BoundedLineError>>>,
    reader: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct WriteRequest {
    frame: Vec<u8>,
    reply: SyncSender<io::Result<()>>,
}

/// Single-writer background adapter that bounds pipe-write latency for owners.
#[derive(Debug)]
pub(crate) struct BoundedWriteChannel {
    sender: Option<SyncSender<WriteRequest>>,
    writer: Option<JoinHandle<()>>,
}

impl BoundedWriteChannel {
    pub(crate) fn spawn<W>(mut writer: W) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel::<WriteRequest>(1);
        let writer = thread::Builder::new()
            .name("cosh-runtime-stdin".to_owned())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let result = writer
                        .write_all(&request.frame)
                        .and_then(|()| writer.flush());
                    let failed = result.is_err();
                    let _ = request.reply.send(result);
                    if failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            writer: Some(writer),
        })
    }

    pub(crate) fn write_timeout(&self, frame: Vec<u8>, timeout: Duration) -> io::Result<()> {
        let sender = self.sender.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "runtime stdin writer unavailable",
            )
        })?;
        let (reply, result) = mpsc::sync_channel(1);
        let deadline = std::time::Instant::now() + timeout;
        let mut request = WriteRequest { frame, reply };
        loop {
            match sender.try_send(request) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    request = returned;
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "runtime stdin queue deadline exceeded",
                        ));
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "runtime stdin writer stopped",
                    ));
                }
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        result
            .recv_timeout(remaining)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "runtime stdin write deadline exceeded",
                ),
                RecvTimeoutError::Disconnected => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime stdin writer stopped")
                }
            })?
    }

    pub(crate) fn finish(mut self) {
        self.sender.take();
        if self.writer.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
        }
    }
}

impl BoundedLineChannel {
    pub(crate) fn spawn<R>(reader: R, max_line_bytes: usize) -> io::Result<Self>
    where
        R: Read + Send + 'static,
    {
        // A single queued frame applies backpressure without allowing a silent
        // or chatty Agent to monopolize the supervisor owner thread.
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::Builder::new()
            .name("cosh-runtime-stdout".to_string())
            .spawn(move || {
                let mut reader = BoundedLineReader::new(reader, max_line_bytes);
                loop {
                    let result = reader.read_line();
                    let terminal = !matches!(result, Ok(Some(_)));
                    if sender.send(result).is_err() || terminal {
                        break;
                    }
                }
            })?;
        Ok(Self {
            receiver: Some(receiver),
            reader: Some(reader),
        })
    }

    pub(crate) fn read_timeout(
        &self,
        timeout: Duration,
    ) -> Result<BoundedLineRead, BoundedLineError> {
        let receiver = self.receiver.as_ref().ok_or_else(|| {
            BoundedLineError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "runtime stdout receiver unavailable",
            ))
        })?;
        match receiver.recv_timeout(timeout) {
            Ok(Ok(Some(line))) => Ok(BoundedLineRead::Line(line)),
            Ok(Ok(None)) => Ok(BoundedLineRead::Eof),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Ok(BoundedLineRead::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(BoundedLineError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "runtime stdout reader stopped without EOF",
            ))),
        }
    }

    pub(crate) fn finish(mut self) {
        // Drop the receiver first so a reader blocked on the bounded sender can
        // exit even when the child emitted an unread frame during shutdown.
        self.receiver.take();
        if self.reader.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

impl<R: Read> BoundedLineReader<R> {
    /// Wraps a reader with a non-zero per-line byte limit.
    ///
    /// # Panics
    ///
    /// Panics when `max_line_bytes` is zero. Launch validation prevents that
    /// configuration for supervised runtimes.
    pub fn new(reader: R, max_line_bytes: usize) -> Self {
        assert!(max_line_bytes > 0, "bounded line limit must be non-zero");
        Self {
            reader: BufReader::new(reader),
            max_line_bytes,
        }
    }

    /// Reads one UTF-8 line without its trailing CR/LF delimiter.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O failure, invalid UTF-8, or a frame larger
    /// than the configured bound.
    pub fn read_line(&mut self) -> Result<Option<String>, BoundedLineError> {
        let mut frame = Vec::with_capacity(self.max_line_bytes.min(8 * 1024));
        let mut limited = self.reader.by_ref().take(self.max_line_bytes as u64 + 2);
        let bytes_read = limited.read_until(b'\n', &mut frame)?;
        if bytes_read == 0 {
            return Ok(None);
        }

        let has_newline = frame.last() == Some(&b'\n');
        let without_newline = frame.len() - usize::from(has_newline);
        let has_carriage_return =
            has_newline && without_newline > 0 && frame.get(without_newline - 1) == Some(&b'\r');
        let payload_len = without_newline - usize::from(has_carriage_return);
        if payload_len > self.max_line_bytes || (!has_newline && frame.len() > self.max_line_bytes)
        {
            return Err(BoundedLineError::TooLarge {
                limit: self.max_line_bytes,
            });
        }

        frame.truncate(payload_len);
        String::from_utf8(frame)
            .map(Some)
            .map_err(|_| BoundedLineError::InvalidUtf8)
    }
}

#[derive(Debug)]
struct StderrTail {
    bytes: VecDeque<u8>,
    capacity: usize,
    discarded_bytes: u64,
    read_error: Option<String>,
}

impl StderrTail {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            capacity,
            discarded_bytes: 0,
            read_error: None,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let overflow = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.capacity);
        for _ in 0..overflow.min(self.bytes.len()) {
            self.bytes.pop_front();
        }

        if chunk.len() >= self.capacity {
            self.bytes.clear();
            let start = chunk.len() - self.capacity;
            self.bytes.extend(&chunk[start..]);
        } else {
            self.bytes.extend(chunk);
        }
        self.discarded_bytes = self.discarded_bytes.saturating_add(overflow as u64);
    }

    fn snapshot(&self) -> StderrSnapshot {
        let bytes = self.bytes.iter().copied().collect::<Vec<_>>();
        StderrSnapshot {
            tail: String::from_utf8_lossy(&bytes).into_owned(),
            discarded_bytes: self.discarded_bytes,
            read_error: self.read_error.clone(),
        }
    }
}

/// Bounded diagnostic output retained after a runtime exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrSnapshot {
    /// Lossy UTF-8 view of the most recent stderr bytes.
    pub tail: String,
    /// Number of older bytes discarded to preserve the bound.
    pub discarded_bytes: u64,
    /// Reader failure, when stderr could not be drained to EOF.
    pub read_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StderrCollector {
    tail: Arc<Mutex<StderrTail>>,
    reader: Option<JoinHandle<()>>,
}

impl StderrCollector {
    pub(crate) fn spawn<R>(mut stderr: R, capacity: usize) -> io::Result<Self>
    where
        R: Read + Send + 'static,
    {
        let tail = Arc::new(Mutex::new(StderrTail::new(capacity)));
        let reader_tail = Arc::clone(&tail);
        let reader = thread::Builder::new()
            .name("cosh-runtime-stderr".to_string())
            .spawn(move || {
                let mut chunk = [0_u8; 8 * 1024];
                loop {
                    match stderr.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => {
                            let Ok(mut tail) = reader_tail.lock() else {
                                break;
                            };
                            tail.push(&chunk[..read]);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            if let Ok(mut tail) = reader_tail.lock() {
                                tail.read_error = Some(error.to_string());
                            }
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            tail,
            reader: Some(reader),
        })
    }

    pub(crate) fn finish(mut self) -> StderrSnapshot {
        // The child has already been reaped before settlement, so its pipe
        // should close promptly. A short bound preserves final diagnostics
        // without allowing a leaked descendant fd to block shutdown.
        let deadline = Instant::now() + Duration::from_millis(100);
        while self
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.is_finished())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        if self.reader.as_ref().is_some_and(JoinHandle::is_finished) {
            if self
                .reader
                .take()
                .is_some_and(|reader| reader.join().is_err())
            {
                if let Ok(mut tail) = self.tail.lock() {
                    tail.read_error = Some("stderr reader thread panicked".to_string());
                }
            }
        } else if let Ok(mut tail) = self.tail.lock() {
            tail.read_error = Some("stderr reader still active at settlement".to_string());
        }
        self.snapshot()
    }

    pub(crate) fn snapshot(&self) -> StderrSnapshot {
        self.tail
            .lock()
            .map(|tail| tail.snapshot())
            .unwrap_or_else(|_| StderrSnapshot {
                tail: String::new(),
                discarded_bytes: 0,
                read_error: Some("stderr tail lock poisoned".to_string()),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn bounded_line_reader_accepts_crlf_and_eof_frame() {
        let input = Cursor::new(b"first\r\nsecond".to_vec());
        let mut reader = BoundedLineReader::new(input, 16);

        assert_eq!(reader.read_line().unwrap().as_deref(), Some("first"));
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("second"));
        assert_eq!(reader.read_line().unwrap(), None);
    }

    #[test]
    fn bounded_line_reader_handles_empty_lf_and_crlf_frames() {
        let input = Cursor::new(b"\n\r\n".to_vec());
        let mut reader = BoundedLineReader::new(input, 8);

        assert_eq!(reader.read_line().unwrap().as_deref(), Some(""));
        assert_eq!(reader.read_line().unwrap().as_deref(), Some(""));
        assert_eq!(reader.read_line().unwrap(), None);
    }

    #[test]
    fn bounded_line_reader_rejects_oversized_frame_without_unbounded_allocation() {
        let input = Cursor::new(b"123456789\n".to_vec());
        let mut reader = BoundedLineReader::new(input, 8);

        assert!(matches!(
            reader.read_line(),
            Err(BoundedLineError::TooLarge { limit: 8 })
        ));
    }

    #[test]
    fn stderr_tail_retains_only_latest_bytes() {
        let mut tail = StderrTail::new(5);
        tail.push(b"abc");
        tail.push(b"defg");

        assert_eq!(tail.snapshot().tail, "cdefg");
        assert_eq!(tail.snapshot().discarded_bytes, 2);
    }
}
