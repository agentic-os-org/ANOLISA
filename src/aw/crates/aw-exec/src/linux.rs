//! Nonblocking stream exchange with one absolute execution deadline.

mod child;
pub(super) mod foreground;

use self::child::OwnedChild;
use crate::{CleanupMode, CommandSpec, Error, Limits, Output, Stream};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::Path,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(2);
const CHUNK_BYTES: usize = 8192;

fn io_error(operation: &'static str, source: io::Error) -> Error {
    Error::Io { operation, source }
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<(), Error> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fd remains owned by the live pipe; fcntl only changes its I/O mode.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io_error("pipe setup", io::Error::last_os_error()));
    }
    Ok(())
}

/// Outcome of one nonblocking pipe operation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The peer closed the stream.
    Eof,
    /// Bytes moved or a signal interrupted the read; retry without waiting.
    Progress,
    /// Nothing was ready.
    Idle,
}

fn drain(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    stream: Stream,
) -> Result<Step, Error> {
    let mut buffer = [0u8; CHUNK_BYTES];
    // A continuously writable peer must not starve the other pipe or deadline.
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(Step::Eof),
        Ok(n) => {
            if n > limit.saturating_sub(bytes.len()) {
                return Err(Error::OutputLimit { stream, limit });
            }
            bytes.extend_from_slice(&buffer[..n]);
            Ok(Step::Progress)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Step::Idle),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(Step::Progress),
        Err(error) => Err(io_error("pipe read", error)),
    }
}

/// Waits until an open pipe is ready or `timeout` elapses.
///
/// The timeout keeps exit, cancellation and deadline checks on the polling
/// interval; readiness only shortens the wait. With no open pipe this is a
/// plain bounded sleep while the leader's exit is awaited.
fn wait_ready(fds: &mut [libc::pollfd], timeout: Duration) -> Result<(), Error> {
    if fds.is_empty() {
        thread::sleep(timeout);
        return Ok(());
    }
    // Round up so a sub-millisecond remainder still yields instead of spinning.
    let millis = timeout.as_micros().div_ceil(1000).min(i32::MAX as u128) as libc::c_int;
    // SAFETY: fds is a live, exclusively borrowed pollfd slice of the given length.
    if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(io_error("pipe poll", error));
        }
    }
    Ok(())
}

fn pollfd(fd: &impl AsRawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    }
}

struct Captured {
    result: Vec<u8>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    sent: usize,
}

fn exchange(
    child: &mut OwnedChild,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
    mut result_pipe: Option<&File>,
) -> Result<Captured, Error> {
    let (stdin, mut stdout, mut stderr) = child.take_pipes()?;
    nonblocking(&stdin)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut stdin = Some(stdin);
    let mut captured = Captured {
        result: Vec::new(),
        stdout: Vec::new(),
        stderr: Vec::new(),
        sent: 0,
    };
    let mut out_done = false;
    let mut err_done = false;
    loop {
        crate::check_active(deadline, cancelled)?;
        if captured.sent == input.len() {
            stdin.take();
        }
        // Waiting a full interval after every chunk would cap each pipe near
        // CHUNK_BYTES per POLL_INTERVAL; only an idle round may wait.
        let mut progressed = false;
        if let Some(pipe) = stdin.as_mut() {
            let remaining = &input[captured.sent..];
            match pipe.write(&remaining[..remaining.len().min(CHUNK_BYTES)]) {
                Ok(0) => return Err(io_error("stdin write", io::ErrorKind::WriteZero.into())),
                Ok(n) => {
                    captured.sent += n;
                    progressed = true;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    stdin.take();
                }
                Err(error) => return Err(io_error("stdin write", error)),
            }
        }
        if !out_done {
            let step = drain(
                &mut stdout,
                &mut captured.stdout,
                limits.stdout_bytes,
                Stream::Stdout,
            )?;
            out_done = step == Step::Eof;
            progressed |= step != Step::Idle;
        }
        if !err_done {
            let step = drain(
                &mut stderr,
                &mut captured.stderr,
                limits.stderr_bytes,
                Stream::Stderr,
            )?;
            err_done = step == Step::Eof;
            progressed |= step != Step::Idle;
        }
        let exited =
            out_done && err_done && child.observe_exit().map_err(|e| io_error("wait", e))?;
        // Read after observing exit so the writer's final bytes cannot be missed.
        if let Some(pipe) = result_pipe.as_mut() {
            let step = drain(
                pipe,
                &mut captured.result,
                limits.stdout_bytes,
                Stream::Result,
            )?;
            if step == Step::Progress {
                continue;
            }
        }
        if exited {
            return Ok(captured);
        }
        if progressed {
            continue;
        }
        let mut fds = Vec::with_capacity(4);
        if let Some(pipe) = result_pipe {
            fds.push(pollfd(pipe, libc::POLLIN));
        }
        if let Some(pipe) = stdin.as_ref() {
            fds.push(pollfd(pipe, libc::POLLOUT));
        }
        if !out_done {
            fds.push(pollfd(&stdout, libc::POLLIN));
        }
        if !err_done {
            fds.push(pollfd(&stderr, libc::POLLIN));
        }
        wait_ready(
            &mut fds,
            POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
        )?;
    }
}

fn result_fifo(path: &Path) -> Result<File, Error> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io_error("result pipe path", e.into()))?;
    // SAFETY: name is NUL-terminated and remains valid across mkfifo.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return Err(io_error("create result pipe", io::Error::last_os_error()));
    }
    // O_RDWR prevents premature EOF before a wrapper opens the FIFO.
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| io_error("open result pipe", e))
}

pub(super) fn run(
    command: &CommandSpec,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
    cleanup: CleanupMode,
    result_path: Option<&Path>,
) -> Result<(Output, Vec<u8>), Error> {
    crate::check_active(deadline, cancelled)?;
    if input.len() > limits.input_bytes {
        return Err(Error::InputLimit {
            limit: limits.input_bytes,
            actual: input.len(),
        });
    }
    let result_pipe = result_path.map(result_fifo).transpose()?;
    let mut child = OwnedChild::spawn(command)?;
    let result = exchange(
        &mut child,
        input,
        limits,
        deadline,
        cancelled,
        result_pipe.as_ref(),
    );
    let terminated = if cleanup == CleanupMode::Cooperative && result.is_err() {
        child.terminate()
    } else {
        Ok(())
    };
    // Never report an execution result while group cleanup remains unverified.
    let status = child.cleanup().map_err(|source| Error::Cleanup {
        pid: child.id(),
        source,
    })?;
    terminated.map_err(|source| Error::Cleanup {
        pid: child.id(),
        source,
    })?;
    let captured = result?;
    Ok((
        Output {
            status,
            stdout: captured.stdout,
            stderr: captured.stderr,
            input_bytes_written: captured.sent,
        },
        captured.result,
    ))
}
