//! Nonblocking three-pipe exchange with one absolute execution deadline.

mod child;
pub(super) mod foreground;

use self::child::OwnedChild;
use crate::{CommandSpec, Error, Limits, Output, Stream};
use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    sync::atomic::{AtomicBool, Ordering},
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

fn drain(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    stream: Stream,
) -> Result<bool, Error> {
    let mut buffer = [0u8; CHUNK_BYTES];
    // A continuously writable peer must not starve the other pipe or deadline.
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(n) => {
            if n > limit.saturating_sub(bytes.len()) {
                return Err(Error::OutputLimit { stream, limit });
            }
            bytes.extend_from_slice(&buffer[..n]);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(io_error("pipe read", error)),
    }
}

struct Captured {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    sent: usize,
}

fn check_stop(deadline: Instant, cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Error::DeadlineExceeded);
    }
    Ok(())
}

fn exchange(
    child: &mut OwnedChild,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Captured, Error> {
    let (stdin, mut stdout, mut stderr) = child.take_pipes()?;
    nonblocking(&stdin)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut stdin = Some(stdin);
    let mut captured = Captured {
        stdout: Vec::new(),
        stderr: Vec::new(),
        sent: 0,
    };
    let mut out_done = false;
    let mut err_done = false;
    loop {
        check_stop(deadline, cancelled)?;
        if captured.sent == input.len() {
            stdin.take();
        }
        if let Some(pipe) = stdin.as_mut() {
            let remaining = &input[captured.sent..];
            match pipe.write(&remaining[..remaining.len().min(CHUNK_BYTES)]) {
                Ok(0) => return Err(io_error("stdin write", io::ErrorKind::WriteZero.into())),
                Ok(n) => captured.sent += n,
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
            out_done = drain(
                &mut stdout,
                &mut captured.stdout,
                limits.stdout_bytes,
                Stream::Stdout,
            )?;
        }
        if !err_done {
            err_done = drain(
                &mut stderr,
                &mut captured.stderr,
                limits.stderr_bytes,
                Stream::Stderr,
            )?;
        }
        if out_done && err_done && child.observe_exit().map_err(|e| io_error("wait", e))? {
            return Ok(captured);
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

pub(super) fn run(
    command: &CommandSpec,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Output, Error> {
    check_stop(deadline, cancelled)?;
    if input.len() > limits.input_bytes {
        return Err(Error::InputLimit {
            limit: limits.input_bytes,
            actual: input.len(),
        });
    }
    let mut child = OwnedChild::spawn(command)?;
    let result = exchange(&mut child, input, limits, deadline, cancelled);
    // Never report an execution result while group cleanup remains unverified.
    let status = child.cleanup().map_err(|source| Error::Cleanup {
        pid: child.id(),
        source,
    })?;
    let captured = result?;
    Ok(Output {
        status,
        stdout: captured.stdout,
        stderr: captured.stderr,
        input_bytes_written: captured.sent,
    })
}
