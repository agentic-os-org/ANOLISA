//! Deadline-bounded execution of external upgrade probe commands.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use wait_timeout::ChildExt;

const MAX_PIPE_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct CommandOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
}

type PipeOutput = (Vec<u8>, bool);

/// Process groups of in-flight probe commands.
///
/// Each command runs in its own process group, so terminal hangup never reaches it and
/// its deadline lives only in the probing thread. `std::process::exit` drops that thread
/// without killing the group; the shell therefore terminates registered groups itself.
#[derive(Default)]
pub(super) struct ProbeProcesses {
    state: Mutex<ProbeProcessState>,
}

#[derive(Default)]
struct ProbeProcessState {
    closed: bool,
    groups: Vec<Pid>,
}

impl ProbeProcesses {
    // Spawning under the lock keeps a concurrent `terminate` from missing a fresh group.
    fn spawn(&self, command: &mut Command) -> Option<Child> {
        let mut state = self.lock();
        if state.closed {
            return None;
        }
        let child = command.spawn().ok()?;
        state.groups.push(Pid::from_raw(child.id() as i32));
        Some(child)
    }

    fn release(&self, group: Pid) {
        self.lock().groups.retain(|registered| *registered != group);
    }

    /// Kills every in-flight probe group and refuses to spawn further probe commands.
    pub(super) fn terminate(&self) {
        let mut state = self.lock();
        state.closed = true;
        for group in state.groups.drain(..) {
            let _ = killpg(group, Signal::SIGKILL);
        }
    }

    // Shutdown may follow a relay panic; a poisoned lock still guards a valid list.
    fn lock(&self) -> MutexGuard<'_, ProbeProcessState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs `command` and returns within `timeout`, even when the process cannot be reaped
/// or a descendant that left its process group keeps the output pipes open.
pub(super) fn run_bounded_command(
    processes: &ProbeProcesses,
    mut command: Command,
    timeout: Duration,
) -> Option<CommandOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    #[cfg(target_os = "linux")]
    kill_with_parent_thread(&mut command);
    let deadline = Instant::now() + timeout;
    let mut child = processes.spawn(&mut command)?;
    let process_group = Pid::from_raw(child.id() as i32);
    let readers = child
        .stdout
        .take()
        .zip(child.stderr.take())
        .and_then(|(stdout, stderr)| {
            Some((spawn_pipe_reader(stdout)?, spawn_pipe_reader(stderr)?))
        });
    let status = readers.as_ref().and_then(|_| {
        child
            .wait_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
            .flatten()
    });
    // Exited commands may leave same-group descendants holding the pipes; descendants
    // that escaped the group are bounded by the drain deadline instead.
    let _ = killpg(process_group, Signal::SIGKILL);
    processes.release(process_group);
    let (Some(status), Some((stdout_reader, stderr_reader))) = (status, readers) else {
        let _ = child.kill();
        reap_in_background(child);
        return None;
    };
    let (stdout, stdout_truncated) = receive_before(&stdout_reader, deadline)?;
    let (_, stderr_truncated) = receive_before(&stderr_reader, deadline)?;
    if stdout_truncated || stderr_truncated {
        tracing::warn!(
            stdout_truncated,
            stderr_truncated,
            "upgrade probe command output exceeded the size limit"
        );
        return None;
    }
    Some(CommandOutput { status, stdout })
}

/// Backstops shell death that skips `ProbeProcesses::terminate`, such as a fatal signal.
///
/// The kernel signals when the spawning thread exits. That thread waits for the command
/// and kills its group before returning, so only an abnormal exit can trigger this; it
/// covers the direct child, not descendants that outlive it.
#[cfg(target_os = "linux")]
fn kill_with_parent_thread(command: &mut Command) {
    let expected_parent = std::process::id() as nix::libc::pid_t;
    // SAFETY: the hook runs between fork and exec and only calls the async-signal-safe
    // `prctl` and `getppid`, touching no memory shared with the parent.
    unsafe {
        command.pre_exec(move || {
            // Re-checking the parent closes the race where it died before `prctl` ran.
            if nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL, 0, 0, 0) < 0
                || nix::libc::getppid() != expected_parent
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn spawn_pipe_reader(
    pipe: impl Read + Send + 'static,
) -> Option<mpsc::Receiver<io::Result<PipeOutput>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("cosh-upgrade-probe-pipe".to_string())
        .spawn(move || {
            let _ = sender.send(read_bounded(pipe));
        })
        .ok()?;
    Some(receiver)
}

fn receive_before(
    receiver: &mpsc::Receiver<io::Result<PipeOutput>>,
    deadline: Instant,
) -> Option<PipeOutput> {
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(output) => output.ok(),
        Err(_) => {
            // The reader thread is abandoned; it exits once the last writer closes the pipe.
            tracing::debug!("upgrade probe command pipes stayed open past the deadline");
            None
        }
    }
}

// SIGKILL cannot interrupt uninterruptible sleep, so reaping must not hold the caller.
fn reap_in_background(mut child: Child) {
    let _ = thread::Builder::new()
        .name("cosh-upgrade-probe-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
}

fn read_bounded(mut reader: impl Read) -> io::Result<PipeOutput> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_PIPE_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok((output, truncated))
}
