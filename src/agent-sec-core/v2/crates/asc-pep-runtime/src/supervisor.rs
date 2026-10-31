//! Supervises the helper process group and verifies its installation handshake.

use crate::PepError;
use asc_policy_types::{ExecutionResult, ExecutionSpec};
use std::path::Path;

/// Waits for the isolated process tree without changing host signal handlers.
///
/// # Errors
/// Returns errors before spawn or for malformed helper output.
#[cfg(target_os = "linux")]
pub fn run(helper: &Path, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
    linux::run(helper, spec)
}

/// Linux namespace enforcement is unavailable on other operating systems.
///
/// # Errors
/// Always returns unsupported; never executes without isolation.
#[cfg(not(target_os = "linux"))]
pub fn run(_helper: &Path, _spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
    Err(PepError::Unsupported("linux-sandbox requires Linux".into()))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ExecutionResult, ExecutionSpec, Path, PepError};
    use asc_policy_types::BackendStatus;
    use rustix::fs::{MemfdFlags, SealFlags};
    use rustix::process::{Pid, Signal, kill_process, kill_process_group};
    use std::fs::File;
    use std::io::{BufRead as _, BufReader, IsTerminal as _, Read as _, Seek as _, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn io(error: rustix::io::Errno) -> std::io::Error {
        error.into()
    }

    #[derive(Default)]
    struct Handshake {
        result: ExecutionResult,
        exited: bool,
    }
    impl Handshake {
        fn accept(&mut self, message: Result<BackendStatus, String>) {
            match message {
                Ok(BackendStatus::Installed) if !self.result.installed && !self.exited => {
                    self.result.installed = true;
                }
                Ok(BackendStatus::Started)
                    if self.result.installed && !self.result.started && !self.exited =>
                {
                    self.result.started = true;
                }
                Ok(BackendStatus::Exited { exit_code, signal })
                    if self.result.started
                        && !self.exited
                        && (exit_code.is_some() ^ signal.is_some()) =>
                {
                    self.result.exit_code = exit_code;
                    self.result.signal = signal;
                    self.exited = true;
                }
                Ok(BackendStatus::Failed { message }) => self.result.error = Some(message),
                _ => self.result.error = Some("invalid backend handshake order or data".into()),
            }
        }
    }

    pub(super) fn run(helper: &Path, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
        let fd = rustix::fs::memfd_create(
            "asc-execution",
            MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
        )
        .map_err(io)?;
        let mut input = File::from(fd);
        input.write_all(&serde_json::to_vec(spec)?)?;
        input.rewind()?;
        rustix::fs::fcntl_add_seals(
            &input,
            SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
        )
        .map_err(io)?;
        let (read_fd, write_fd) =
            rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(io)?;
        // A foreground terminal must stay usable by interactive agents. We never
        // move the embedding process's terminal or signal its whole process group.
        let terminal = std::io::stdin().is_terminal()
            || std::io::stdout().is_terminal()
            || std::io::stderr().is_terminal();
        let mut command = Command::new(helper);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg("--execution-spec-fd")
            .arg(input.as_raw_fd().to_string())
            .arg("--status-fd")
            .arg(write_fd.as_raw_fd().to_string())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if !terminal {
            command.process_group(0);
        }
        let mut child = linux_sandbox::runtime::spawn_with_fds(
            &mut command,
            &[input.as_raw_fd(), write_fd.as_raw_fd()],
        )?;
        drop(input);
        drop(write_fd);
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| PepError::Protocol("invalid helper PID".into()))?;
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || read_status(File::from(read_fd), &sender));
        let mut handshake = Handshake::default();
        let outcome = wait(&mut child, pid, terminal, &receiver, &mut handshake);
        if outcome.is_err() {
            terminate(pid, terminal);
            let _ = child.wait();
        }
        // The helper-only write descriptor closes when its PID namespace is torn down.
        let _ = reader.join();
        for message in receiver.try_iter() {
            handshake.accept(message);
        }
        let monitor = outcome?;
        if handshake.result.error.is_none() {
            if let Some(signal) = monitor.signal() {
                handshake.result.cancel_signal.get_or_insert(signal);
            }
            if !handshake.exited && handshake.result.cancel_signal.is_none() {
                handshake.result.error = Some(
                    "backend exited without a complete installation/execution handshake".into(),
                );
            }
        }
        Ok(handshake.result)
    }

    fn read_status(file: File, sender: &mpsc::Sender<Result<BackendStatus, String>>) {
        let mut source = BufReader::new(file);
        loop {
            let mut line = String::new();
            let message = match (&mut source).take(16_385).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.len() <= 16_384 && line.ends_with('\n') => {
                    serde_json::from_str(&line).map_err(|error| error.to_string())
                }
                Ok(_) => Err("oversized or incomplete backend status".into()),
                Err(error) => Err(error.to_string()),
            };
            let malformed = message.is_err();
            if sender.send(message).is_err() || malformed {
                break;
            }
        }
    }

    fn terminate(pid: Pid, terminal: bool) {
        // Killing bubblewrap also tears down its PID namespace, including
        // descendants that changed their session or process group.
        if terminal {
            let _ = kill_process(pid, Signal::KILL);
        } else {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }

    fn wait(
        child: &mut Child,
        pid: Pid,
        terminal: bool,
        receiver: &mpsc::Receiver<Result<BackendStatus, String>>,
        handshake: &mut Handshake,
    ) -> Result<ExitStatus, std::io::Error> {
        let start = Instant::now();
        loop {
            for message in receiver.try_iter() {
                handshake.accept(message);
            }
            if !handshake.result.installed && start.elapsed() > Duration::from_secs(30) {
                handshake.result.error = Some("sandbox installation timed out".into());
            }
            if handshake.result.error.is_some() {
                terminate(pid, terminal);
            }
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
