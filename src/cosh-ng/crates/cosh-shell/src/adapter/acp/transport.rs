//! Bounded stdio transport for one supervised ACP Agent subprocess.
//!
//! Lines are capped before the whole line is allocated, and reads happen on a
//! dedicated thread so the synchronous client loop can honor cancellation and
//! idle deadlines without a second async runtime.

use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::process::{Child, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::super::{
    spawn_provider_child, terminate_and_reap_process, AdapterError, PreparedInvocation,
    ProviderPromptArgMode, ProviderStdinMode, StderrTail,
};
use super::{CANCEL_GRACE, POLL_SLICE};

#[cfg(test)]
mod tests;

/// Pending inbound facts the client loop has not consumed yet. A foreground
/// review can hold that loop for as long as a human needs, so the queue needs a
/// capacity of its own on top of the per-line limit.
const MAX_QUEUED_FRAMES: usize = 4096;
/// The frame cap alone would still allow 4096 maximum-size lines, so the byte
/// cap is what bounds memory when the peer sends large frames.
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;

/// Live queue occupancy, shared by the single reader that adds and the single
/// client loop that releases.
struct Backlog {
    frames: AtomicUsize,
    bytes: AtomicUsize,
    /// Latched overload, kept outside the queue so a stop reply that was queued
    /// before the bound tripped cannot report a normal end for a cut stream.
    overloaded: AtomicBool,
}

impl Backlog {
    /// Takes the latched overload once, so it is reported exactly once and
    /// ahead of any frame the queue still holds.
    fn take_overload(&self) -> Option<LineEvent> {
        if self.overloaded.swap(false, Ordering::Relaxed) {
            Some(LineEvent::Failed(format!(
                "acp inbound backlog exceeds {MAX_QUEUED_FRAMES} frames or {MAX_QUEUED_BYTES} bytes"
            )))
        } else {
            None
        }
    }

    fn release(&self, event: &LineEvent) {
        if let LineEvent::Line(line) = event {
            self.frames.fetch_sub(1, Ordering::Relaxed);
            self.bytes.fetch_sub(line.len(), Ordering::Relaxed);
        }
    }
}

/// One bounded inbound transport fact.
#[derive(Debug)]
pub(super) enum LineEvent {
    Line(String),
    Closed,
    Failed(String),
}

/// Owns the Agent subprocess, its bounded stdout reader and stderr tail.
pub(super) struct AgentTransport {
    child: Child,
    lines: Receiver<LineEvent>,
    backlog: Arc<Backlog>,
    reader: Option<JoinHandle<()>>,
    stderr: StderrTail,
    cancelled: Arc<AtomicBool>,
}

impl AgentTransport {
    /// Launches one Agent with piped stdio in its own session, reusing the
    /// hardened Shell provider spawn path (no shell expansion, setsid).
    pub(super) fn launch(
        program: &str,
        args: &[String],
        max_line_bytes: usize,
        stderr_tail_bytes: usize,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, AdapterError> {
        let prepared = PreparedInvocation {
            program: program.to_string(),
            args: args.to_vec(),
            prompt: String::new(),
        };
        let mut child = spawn_provider_child(
            &prepared,
            "acp",
            ProviderStdinMode::Piped,
            ProviderPromptArgMode::None,
        )?;
        // A peer that stops draining stdin fills the pipe, and a blocking write
        // there would keep the client loop from ever reaching its cancel check.
        if let Some(fd) = child.stdin.as_ref().map(AsRawFd::as_raw_fd) {
            if let Err(failure) = set_nonblocking(fd) {
                // No transport exists yet, so `Child`'s Drop would neither kill
                // nor wait: this early return owns reaping the spawned peer.
                terminate_and_reap_process(&mut child);
                return Err(failure);
            }
        }
        let stdout = child.stdout.take().ok_or_else(|| AdapterError {
            message: "failed to capture acp agent stdout".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| AdapterError {
            message: "failed to capture acp agent stderr".to_string(),
        })?;
        let tail = StderrTail::new(stderr_tail_bytes);
        tail.drain_in_background(stderr);
        let (sender, lines) = mpsc::channel();
        let backlog = Arc::new(Backlog {
            frames: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            overloaded: AtomicBool::new(false),
        });
        let reader_backlog = Arc::clone(&backlog);
        // The channel stays unbounded so a full queue can never block the reader
        // inside `send`, which would keep `shutdown` from joining it.
        let reader = thread::spawn(move || {
            read_bounded_lines(stdout, max_line_bytes, sender, reader_backlog)
        });
        Ok(Self {
            child,
            lines,
            backlog,
            reader: Some(reader),
            stderr: tail,
            cancelled,
        })
    }

    /// Writes one already-encoded JSON-RPC frame as a single line, staying
    /// interruptible by cancellation while the peer is not draining stdin.
    pub(super) fn write_frame(&mut self, frame: &str) -> Result<(), AdapterError> {
        let stdin = self.child.stdin.as_mut().ok_or_else(|| AdapterError {
            message: "acp agent stdin is closed".to_string(),
        })?;
        let fd = stdin.as_raw_fd();
        write_cancellable(fd, frame.as_bytes(), &self.cancelled)
            .and_then(|()| write_cancellable(fd, b"\n", &self.cancelled))
    }

    /// Waits for one inbound fact, reporting an idle timeout as `Ok(None)`.
    pub(super) fn next_line(&self, timeout: Duration) -> Result<Option<LineEvent>, AdapterError> {
        Ok(receive_line(&self.lines, &self.backlog, timeout))
    }

    pub(super) fn stderr_tail(&self) -> String {
        self.stderr.snapshot()
    }

    /// Whether the Agent process already exited, so a kept connection is not
    /// handed to a new user request. Reaping here is what `shutdown` already
    /// does first, so it adds no second kill of a possibly reused pid.
    pub(super) fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Terminates the Agent process group and joins the reader thread.
    pub(super) fn shutdown(&mut self) {
        terminate_and_reap_process(&mut self.child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for AgentTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Marks the write end non-blocking so a full pipe yields a short write instead
/// of an uninterruptible sleep. Only this process holds the write description.
fn set_nonblocking(fd: RawFd) -> Result<(), AdapterError> {
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    let applied = if flags < 0 {
        -1
    } else {
        unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) }
    };
    if applied < 0 {
        return Err(AdapterError {
            message: format!(
                "failed to set acp agent stdin non-blocking: {}",
                std::io::Error::last_os_error()
            ),
        });
    }
    Ok(())
}

fn write_cancelled() -> AdapterError {
    AdapterError {
        message: "acp frame write cancelled".to_string(),
    }
}

fn write_failure(failure: std::io::Error) -> AdapterError {
    AdapterError {
        message: format!("failed to write acp frame: {failure}"),
    }
}

/// Writes every byte in bounded poll slices. A write that cannot progress ends
/// as soon as cancellation is seen. A write that keeps progressing gets one
/// budget measured from the first cancelled slice, so a peer that drains a
/// little at a time cannot keep the write alive past the cancel grace by never
/// letting a slice time out. `session/cancel` and a pre-cancelled handshake are
/// written while the flag is already set and are small enough to fit inside that
/// budget. Without a cancel no deadline applies, so a slow peer is never cut off.
fn write_cancellable(fd: RawFd, bytes: &[u8], cancelled: &AtomicBool) -> Result<(), AdapterError> {
    let mut written = 0;
    let mut cancel_deadline: Option<Instant> = None;
    while written < bytes.len() {
        let mut descriptor = nix::libc::pollfd {
            fd,
            events: nix::libc::POLLOUT,
            revents: 0,
        };
        let ready = unsafe { nix::libc::poll(&mut descriptor, 1, POLL_SLICE.as_millis() as i32) };
        if ready < 0 {
            let failure = std::io::Error::last_os_error();
            if failure.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(write_failure(failure));
        }
        if ready == 0 {
            if cancelled.load(Ordering::SeqCst) {
                return Err(write_cancelled());
            }
            continue;
        }
        if cancelled.load(Ordering::SeqCst) {
            // Set once: progress must not restart the post-cancel budget.
            let deadline = *cancel_deadline.get_or_insert_with(|| Instant::now() + CANCEL_GRACE);
            if Instant::now() >= deadline {
                return Err(write_cancelled());
            }
        }
        if descriptor.revents & (nix::libc::POLLERR | nix::libc::POLLNVAL | nix::libc::POLLHUP) != 0
        {
            return Err(write_failure(std::io::Error::from_raw_os_error(
                nix::libc::EPIPE,
            )));
        }
        let count = unsafe {
            nix::libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written)
        };
        if count < 0 {
            let failure = std::io::Error::last_os_error();
            match failure.kind() {
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock => continue,
                _ => return Err(write_failure(failure)),
            }
        }
        if count == 0 {
            return Err(write_failure(std::io::Error::from_raw_os_error(
                nix::libc::EPIPE,
            )));
        }
        written += count as usize;
    }
    Ok(())
}

/// Receives one inbound fact. The overload is checked both before and after the
/// receive: the reader can latch it while this call is waiting, and a frame
/// received in that same call must not report a normal end for a cut stream.
fn receive_line(
    lines: &Receiver<LineEvent>,
    backlog: &Backlog,
    timeout: Duration,
) -> Option<LineEvent> {
    if let Some(failure) = backlog.take_overload() {
        return Some(failure);
    }
    match lines.recv_timeout(timeout) {
        Ok(event) => Some(expose_received(backlog, event)),
        Err(RecvTimeoutError::Timeout) => None,
        Err(RecvTimeoutError::Disconnected) => Some(LineEvent::Closed),
    }
}

/// Selects what to expose for an already-received fact. An overload latched while
/// the receive was waiting outranks the frame, which is dropped because the
/// stream has failed; the occupancy is released either way.
fn expose_received(backlog: &Backlog, event: LineEvent) -> LineEvent {
    backlog.release(&event);
    match backlog.take_overload() {
        Some(failure) => failure,
        None => event,
    }
}

/// Queues one decoded line, or fails the stream once the queue is full. Stopping
/// the reader is what bounds memory: the peer then writes into a closed pipe
/// instead of growing our heap while the review is still pending.
fn queue_line(backlog: &Backlog, sender: &mpsc::Sender<LineEvent>, line: String) -> bool {
    let frames = backlog.frames.load(Ordering::Relaxed);
    let bytes = backlog.bytes.load(Ordering::Relaxed);
    if frames + 1 > MAX_QUEUED_FRAMES || bytes + line.len() > MAX_QUEUED_BYTES {
        // Latching instead of queueing the failure is what makes it survive a
        // stop reply already waiting in front of it. The reader stops here, so
        // the next poll slice surfaces the failure without a channel send.
        backlog.overloaded.store(true, Ordering::Relaxed);
        return false;
    }
    backlog.frames.fetch_add(1, Ordering::Relaxed);
    backlog.bytes.fetch_add(line.len(), Ordering::Relaxed);
    sender.send(LineEvent::Line(line)).is_ok()
}

fn read_bounded_lines(
    mut stdout: ChildStdout,
    max_line_bytes: usize,
    sender: mpsc::Sender<LineEvent>,
    backlog: Arc<Backlog>,
) {
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = match stdout.read(&mut buffer) {
            Ok(0) => {
                if !pending.is_empty() {
                    match String::from_utf8(std::mem::take(&mut pending)) {
                        Ok(line) => {
                            if !queue_line(&backlog, &sender, line) {
                                return;
                            }
                        }
                        Err(_) => {
                            let _ = sender.send(LineEvent::Failed(
                                "acp agent stdout is not valid UTF-8".to_string(),
                            ));
                            return;
                        }
                    }
                }
                let _ = sender.send(LineEvent::Closed);
                return;
            }
            Ok(read) => read,
            Err(failure) => {
                let _ = sender.send(LineEvent::Failed(format!(
                    "failed to read acp agent stdout: {failure}"
                )));
                return;
            }
        };
        let mut chunk = &buffer[..read];
        while let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
            let (line, rest) = chunk.split_at(position);
            if pending.len() + line.len() > max_line_bytes {
                let _ = sender.send(LineEvent::Failed(format!(
                    "acp agent line exceeds {max_line_bytes} byte limit"
                )));
                return;
            }
            pending.extend_from_slice(line);
            match String::from_utf8(std::mem::take(&mut pending)) {
                Ok(line) => {
                    if !queue_line(&backlog, &sender, line) {
                        return;
                    }
                }
                Err(_) => {
                    let _ = sender.send(LineEvent::Failed(
                        "acp agent stdout is not valid UTF-8".to_string(),
                    ));
                    return;
                }
            }
            chunk = &rest[1..];
        }
        if pending.len() + chunk.len() > max_line_bytes {
            let _ = sender.send(LineEvent::Failed(format!(
                "acp agent line exceeds {max_line_bytes} byte limit"
            )));
            return;
        }
        pending.extend_from_slice(chunk);
    }
}
