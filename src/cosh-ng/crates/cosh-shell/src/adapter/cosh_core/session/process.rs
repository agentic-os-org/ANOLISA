//! Bounded lifecycle management for one-shot session-control processes.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use nix::libc;
use nix::{
    errno::Errno,
    fcntl::{fcntl, FcntlArg, OFlag},
};

use super::super::super::process::terminate_and_reap_process;

const MAX_SESSION_CONTROL_STDOUT_BYTES: usize = 1024 * 1024;
const MAX_SESSION_CONTROL_STDERR_BYTES: usize = 256 * 1024;
const OUTPUT_READER_POLL_INTERVAL_MS: i32 = 50;

pub(super) fn execute(
    program: &str,
    request: Vec<u8>,
    deadline: Instant,
    timeout: Duration,
) -> io::Result<Output> {
    let child = spawn(program, deadline)?;
    let mut process = SessionControlProcess::new(child);
    let stdin = process.child_mut().stdin.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "cosh-core session-control stdin is unavailable",
        )
    })?;
    process.start_request_writer(stdin, request)?;
    process.wait_with_output(deadline, timeout)
}

struct SessionControlProcess {
    child: Child,
    request_writer: Option<RequestWriter>,
    stdout_reader: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    stderr_reader: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    stdout_output: Option<Vec<u8>>,
    stderr_output: Option<Vec<u8>>,
    output_stop: Arc<AtomicBool>,
    reaped: bool,
}

struct RequestWriter {
    stdin: ChildStdin,
    request: Vec<u8>,
    written: usize,
}

impl SessionControlProcess {
    fn new(child: Child) -> Self {
        Self {
            child,
            request_writer: None,
            stdout_reader: None,
            stderr_reader: None,
            stdout_output: None,
            stderr_output: None,
            output_stop: Arc::new(AtomicBool::new(false)),
            reaped: false,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    fn start_request_writer(&mut self, stdin: ChildStdin, request: Vec<u8>) -> io::Result<()> {
        set_nonblocking(&stdin)?;
        self.request_writer = Some(RequestWriter {
            stdin,
            request,
            written: 0,
        });
        Ok(())
    }

    fn poll_request_writer(&mut self, deadline: Instant, timeout: Duration) -> io::Result<()> {
        let Some(writer) = self.request_writer.as_mut() else {
            return Ok(());
        };
        while writer.written < writer.request.len() {
            if Instant::now() >= deadline {
                return Err(map_request_write_error(session_control_timeout(timeout)));
            }
            match writer.stdin.write(&writer.request[writer.written..]) {
                Ok(0) => {
                    return Err(map_request_write_error(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "request pipe accepted zero bytes",
                    )));
                }
                Ok(written) => writer.written = writer.written.saturating_add(written),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(map_request_write_error(error)),
            }
        }
        match writer.stdin.flush() {
            Ok(()) => {
                self.request_writer = None;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(map_request_write_error(error)),
        }
    }

    fn wait_with_output(&mut self, deadline: Instant, timeout: Duration) -> io::Result<Output> {
        let stdout = self.child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "cosh-core session-control stdout is unavailable",
            )
        })?;
        let stderr = self.child.stderr.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "cosh-core session-control stderr is unavailable",
            )
        })?;
        let stdout_stop = Arc::clone(&self.output_stop);
        self.stdout_reader = Some(thread::spawn(move || {
            read_output(
                stdout,
                MAX_SESSION_CONTROL_STDOUT_BYTES,
                "stdout",
                stdout_stop,
            )
        }));
        let stderr_stop = Arc::clone(&self.output_stop);
        self.stderr_reader = Some(thread::spawn(move || {
            read_output(
                stderr,
                MAX_SESSION_CONTROL_STDERR_BYTES,
                "stderr",
                stderr_stop,
            )
        }));

        let status = loop {
            if let Err(error) = self.poll_request_writer(deadline, timeout) {
                self.terminate_and_reap();
                return Err(error);
            }
            if let Err(error) = self.poll_output_readers() {
                self.terminate_and_reap();
                return Err(error);
            }
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.reaped = true;
                    // A wrapper can exit while descendants retain its output pipes.
                    terminate_and_reap_process(&mut self.child);
                    self.stop_output_readers();
                    if self.request_writer.is_some() {
                        self.request_writer = None;
                        return Err(map_request_write_error(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "cosh-core exited before consuming the complete session request",
                        )));
                    }
                    break status;
                }
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    self.terminate_and_reap();
                    return Err(session_control_timeout(timeout));
                }
                Err(error) => {
                    self.terminate_and_reap();
                    return Err(error);
                }
            }
        };
        self.poll_output_readers()?;
        let stdout = match self.stdout_output.take() {
            Some(output) => output,
            None => join_output_reader(self.stdout_reader.take(), "stdout")?,
        };
        let stderr = match self.stderr_output.take() {
            Some(output) => output,
            None => join_output_reader(self.stderr_reader.take(), "stderr")?,
        };
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    fn terminate_and_reap(&mut self) {
        self.stop_output_readers();
        self.request_writer = None;
        if !self.reaped {
            terminate_and_reap_process(&mut self.child);
            self.reaped = true;
        }
        let _ = join_output_reader(self.stdout_reader.take(), "stdout");
        let _ = join_output_reader(self.stderr_reader.take(), "stderr");
        self.stdout_output = None;
        self.stderr_output = None;
    }

    fn poll_output_readers(&mut self) -> io::Result<()> {
        poll_output_reader(&mut self.stdout_reader, &mut self.stdout_output, "stdout")?;
        poll_output_reader(&mut self.stderr_reader, &mut self.stderr_output, "stderr")
    }

    fn stop_output_readers(&self) {
        self.output_stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for SessionControlProcess {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

fn read_output(
    mut reader: impl Read + AsRawFd,
    limit: usize,
    stream: &'static str,
    stop: Arc<AtomicBool>,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if !output_ready(reader.as_raw_fd(), &stop)? {
            return Ok(bytes);
        }
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(count) if bytes.len().saturating_add(count) > limit => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("cosh-core session-control {stream} exceeded {limit} bytes"),
                ));
            }
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn output_ready(fd: i32, stop: &AtomicBool) -> io::Result<bool> {
    loop {
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        let timeout = if stop.load(Ordering::SeqCst) {
            0
        } else {
            OUTPUT_READER_POLL_INTERVAL_MS
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            if stop.load(Ordering::SeqCst) {
                return Ok(false);
            }
            continue;
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cosh-core session-control output descriptor is invalid",
            ));
        }
        return Ok(true);
    }
}

fn poll_output_reader(
    handle: &mut Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    output: &mut Option<Vec<u8>>,
    stream: &str,
) -> io::Result<()> {
    if handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
        *output = Some(join_output_reader(handle.take(), stream)?);
    }
    Ok(())
}

fn join_output_reader(
    handle: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    stream: &str,
) -> io::Result<Vec<u8>> {
    let Some(handle) = handle else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cosh-core session-control {stream} reader was not started"),
        ));
    };
    handle.join().map_err(|_| {
        io::Error::other(format!(
            "cosh-core session-control {stream} reader panicked"
        ))
    })?
}

fn set_nonblocking(stdin: &ChildStdin) -> io::Result<()> {
    let descriptor = stdin.as_raw_fd();
    let flags = fcntl(descriptor, FcntlArg::F_GETFL).map_err(errno_to_io)?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(descriptor, FcntlArg::F_SETFL(flags))
        .map(drop)
        .map_err(errno_to_io)
}

fn errno_to_io(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn map_request_write_error(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("cosh-core session-control request write failed: {error}"),
    )
}

fn session_control_timeout(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "cosh-core session-control exceeded {}ms",
            timeout.as_millis()
        ),
    )
}

fn spawn(program: &str, deadline: Instant) -> io::Result<Child> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 0..MAX_ATTEMPTS {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "cosh-core session-control deadline elapsed before spawn",
            ));
        }
        let mut command = Command::new(program);
        command
            .arg("--session-control")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let result = command.spawn();
        match result {
            Err(error) if text_file_busy(&error) && attempt + 1 < MAX_ATTEMPTS => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(Duration::from_millis(10).min(remaining));
            }
            result => return result,
        }
    }
    // The final attempt cannot take the retry branch, so every reachable path returns above.
    unreachable!("the bounded spawn loop always returns on its final attempt")
}

fn text_file_busy(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}
