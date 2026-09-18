use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::libc;
use serde_json::{json, Value};

use super::CoshCoreAdapter;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Owns one read-only registry request through cancellation and thread termination.
#[derive(Debug)]
pub struct EcsProbeTask {
    cancelled: Arc<AtomicBool>,
    wake: UnixStream,
    worker: Option<JoinHandle<Result<Value, String>>>,
}

impl EcsProbeTask {
    /// Interrupts the request without relinquishing its cleanup ownership.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.wake.shutdown(Shutdown::Write);
    }

    /// Returns a result only after the worker and its owned child have terminated.
    pub fn try_finish(&mut self) -> Option<Result<Value, String>> {
        if !self.worker.as_ref()?.is_finished() {
            return None;
        }
        let worker = self.worker.take()?;
        Some(
            worker
                .join()
                .unwrap_or_else(|_| Err("ECS probe worker failed".into())),
        )
    }
}

impl Drop for EcsProbeTask {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                tracing::warn!("ECS probe worker failed during shutdown");
            }
        }
    }
}

impl CoshCoreAdapter {
    /// Starts an isolated, cancellable ECS prepare or verify request.
    pub fn start_ecs_probe(&self, action: &str) -> io::Result<EcsProbeTask> {
        if !matches!(action, "prepare" | "verify") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid ECS probe action",
            ));
        }
        let mut workspace = self
            .shell_cwd
            .lock()
            .map_err(|_| io::Error::other("shell workspace lock poisoned"))?
            .clone();
        if workspace.is_none() {
            workspace = self
                .session
                .lock()
                .map_err(|_| io::Error::other("core session lock poisoned"))?
                .active_workspace_scope()
                .map(str::to_string);
        }
        let program = self.program.clone();
        let action = action.to_string();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (wake, worker_wake) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        worker_wake.set_nonblocking(true)?;
        // Settled before the worker can spawn, so no owned child predates it.
        keep_children_waitable();
        let worker = thread::Builder::new()
            .name("cosh-auth-ecs-probe".into())
            .spawn(move || {
                run_probe(
                    &program,
                    workspace.as_deref(),
                    &action,
                    &worker_cancelled,
                    &worker_wake,
                )
            })?;
        Ok(EcsProbeTask {
            cancelled,
            wake,
            worker: Some(worker),
        })
    }
}

struct ProbeChild(Child);

/// Keeps terminated probe children waitable by this process.
///
/// Signalling the recorded PID/PGID in cleanup is only sound while this process
/// is the sole reaper of the probe child: once it is reaped the number is freed
/// and can name an unrelated group. That invariant rests on cosh-shell installing
/// no `SIGCHLD` reaping handler and doing no wildcard `waitpid(-1)` (every wait is
/// targeted at a specific `Child`), so the only competing reaper is the kernel
/// under an inherited `SIGCHLD=SIG_IGN`, which auto-reaps with no zombie. This
/// normalizes that one case back to the default disposition, which retains the
/// zombie until [`ProbeChild`] waits for it. A caught handler cannot be inherited
/// across `execve`, so the inherited disposition is only ever `SIG_IGN` or
/// `SIG_DFL`; a non-ignore disposition is left untouched so an in-process handler
/// (e.g. a test's `wait-timeout`) is not clobbered. The disposition is not
/// restored because a restore would reopen the same window. Introducing any
/// reaping `SIGCHLD` handler or wildcard wait in production would break the
/// sole-reaper invariant and require a pidfd-based path instead.
fn keep_children_waitable() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
        tracing::error!(
            error = %io::Error::last_os_error(),
            "ECS probe could not read the inherited SIGCHLD disposition"
        );
        return;
    }
    if action.sa_sigaction != libc::SIG_IGN {
        return;
    }
    if unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) } == libc::SIG_ERR {
        tracing::error!(
            error = %io::Error::last_os_error(),
            "ECS probe could not stop the kernel from reaping owned children"
        );
    }
}

/// Whether the recorded PID/PGID still belongs to this probe.
///
/// A reaped child releases its PID, and the process group is named by that same
/// number, so signalling after the wait succeeded (or after the kernel reaped the
/// child under an inherited `SIGCHLD=SIG_IGN`) can hit an unrelated group.
fn owns_signalable_group(wait: &io::Result<Option<ExitStatus>>) -> bool {
    matches!(wait, Ok(None))
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        let pid = self.0.id() as i32;
        let mut reported_error = false;
        let mut signalled = false;
        loop {
            let wait = self.0.try_wait();
            if !signalled && owns_signalable_group(&wait) {
                // The isolated probe cannot share a process group with a live agent.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                signalled = true;
            }
            match wait {
                Ok(Some(_)) => break,
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                // Inherited SIGCHLD ignore can let the kernel reap the child first.
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break,
                Err(error) => {
                    if !reported_error {
                        tracing::error!(%error, "ECS probe child could not be reaped");
                        reported_error = true;
                    }
                    // Keep ownership until reaped; a failed cleanup cannot become a completed task.
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

fn run_probe(
    program: &str,
    workspace: Option<&str>,
    action: &str,
    cancelled: &AtomicBool,
    wake: &UnixStream,
) -> Result<Value, String> {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    check_deadline(cancelled, deadline)?;
    let mut command = Command::new(program);
    command
        .arg("--registry")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(workspace) = workspace {
        command.arg("--workspace").arg(workspace);
    }
    let mut child = ProbeChild(
        command
            .spawn()
            .map_err(|_| "Could not start ECS probe".to_string())?,
    );
    let mut input = child.0.stdin.take().ok_or("ECS probe stdin unavailable")?;
    let mut output = child
        .0
        .stdout
        .take()
        .ok_or("ECS probe stdout unavailable")?;
    set_nonblocking(input.as_raw_fd()).map_err(|_| "Could not configure ECS probe stdin")?;
    set_nonblocking(output.as_raw_fd()).map_err(|_| "Could not configure ECS probe stdout")?;
    let request_id = format!(
        "ecs-probe-{}-{}",
        std::process::id(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    let mut request = json!({
        "type": "registry_request", "request_id": request_id,
        "domain": "auth", "action": action,
        "params": {"provider_type": "aliyun", "auth_source": "ecs_ram_role"}
    })
    .to_string()
    .into_bytes();
    request.push(b'\n');
    let mut written = 0;
    while written < request.len() {
        check_deadline(cancelled, deadline)?;
        match input.write(&request[written..]) {
            Ok(0) => return Err("ECS probe stdin closed".into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(input.as_raw_fd(), libc::POLLOUT, wake, cancelled, deadline)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("Could not write ECS probe request".into()),
        }
    }
    drop(input);
    let mut response = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        check_deadline(cancelled, deadline)?;
        match output.read(&mut buffer) {
            Ok(0) => return Err("ECS probe response ended unexpectedly".into()),
            Ok(count) => {
                response.extend_from_slice(&buffer[..count]);
                if response.len() > MAX_RESPONSE_BYTES {
                    return Err("ECS probe response exceeded its size limit".into());
                }
                while let Some(end) = response.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<_> = response.drain(..=end).collect();
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    return parse_response(&line, &request_id);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(output.as_raw_fd(), libc::POLLIN, wake, cancelled, deadline)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("Could not read ECS probe response".into()),
        }
    }
}

fn check_deadline(cancelled: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("ECS probe cancelled".into())
    } else if Instant::now() >= deadline {
        Err("ECS probe timed out".into())
    } else {
        Ok(())
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wait_ready(
    fd: RawFd,
    events: i16,
    wake: &UnixStream,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<(), String> {
    check_deadline(cancelled, deadline)?;
    let mut fds = [
        libc::pollfd {
            fd,
            events,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let remaining = deadline.saturating_duration_since(Instant::now());
    let milliseconds = remaining.as_millis().min(100) as i32;
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, milliseconds) };
    if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
        return Err("ECS probe readiness check failed".into());
    }
    check_deadline(cancelled, deadline)
}

fn parse_response(line: &[u8], request_id: &str) -> Result<Value, String> {
    let response: Value = serde_json::from_slice(line).map_err(|_| "Invalid ECS probe response")?;
    if response["type"] != "registry_response" || response["request_id"] != request_id {
        return Err("Mismatched ECS probe response".into());
    }
    if response["success"] == true {
        return response
            .get("data")
            .cloned()
            .ok_or_else(|| "Missing ECS probe result".into());
    }
    let reason = match response["data"]["error_code"].as_str() {
        Some("metadata_access_denied") => "Instance metadata access was denied",
        Some("metadata_timeout") => "Instance metadata request timed out",
        Some("metadata_unreachable") => "Instance metadata could not be reached",
        Some("invalid_metadata_response") => "Instance metadata returned invalid credentials",
        _ => "ECS credential check failed",
    };
    Err(reason.into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    #[test]
    fn only_an_unreaped_child_may_be_signalled_by_numeric_id() {
        assert!(owns_signalable_group(&Ok(None)));
        // A reaped PID/PGID can be reused, so the recorded number is no longer ours.
        assert!(!owns_signalable_group(&Ok(Some(ExitStatus::from_raw(0)))));
        for code in [libc::ECHILD, libc::EPERM, libc::EINVAL] {
            assert!(!owns_signalable_group(&Err(io::Error::from_raw_os_error(
                code
            ))));
        }
    }
}
