use std::fs;
#[cfg(unix)]
use std::io;
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::hooks::model::HookInput;
use crate::types::CommandBlock;
use crate::types::HookFinding;

use super::ExternalHookConfig;

#[cfg(any(target_os = "linux", target_os = "android"))]
const PINNED_HOOK_FD_ROOT: &str = "/proc/self/fd";
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const PINNED_HOOK_FD_ROOT: &str = "/dev/fd";

pub(super) fn hook_input_from_block(block: &CommandBlock) -> HookInput {
    let output_preview = block
        .output
        .terminal_output_ref
        .as_deref()
        .and_then(|path| read_preview(path, 50))
        .unwrap_or_default();
    HookInput {
        command: redact(&block.command),
        cwd: redact(&block.cwd),
        exit_code: block.exit_code,
        duration_ms: block.duration_ms,
        output_ref: block.output.terminal_output_ref.as_deref().map(redact),
        output_bytes: block.output.terminal_output_bytes,
        output_preview: redact(&output_preview),
    }
}

pub(super) fn redact_hook_finding(finding: &mut HookFinding) {
    finding.hook_id = redact(&finding.hook_id);
    finding.title = redact(&finding.title);
    finding.description = redact(&finding.description);
    finding.suggestion = redact(&finding.suggestion);
    finding.skill = finding.skill.as_deref().map(redact);
    finding.cli_hint = finding.cli_hint.as_deref().map(redact);
    finding.context_refs = finding
        .context_refs
        .iter()
        .map(|value| redact(value))
        .collect();
}

fn redact(value: &str) -> String {
    crate::evidence::redact_sensitive_text(value).0
}

fn read_preview(path: &str, max_lines: usize) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let preview: String = content
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    if preview.is_empty() {
        None
    } else {
        Some(preview)
    }
}

pub(super) fn run_external_hook(
    config: &ExternalHookConfig,
    #[cfg(unix)] executable: &fs::File,
    input: &HookInput,
) -> Option<HookFinding> {
    let input_json = serde_json::to_string(input).ok()?;
    let safe_path = redact(&config.path.to_string_lossy());

    #[cfg(unix)]
    let (mut command, _inherited_executable) = command_for_pinned_hook(executable)
        .map_err(|error| {
            tracing::error!(
                target: "cosh_hook",
                path = %safe_path,
                "external hook descriptor preparation failed: {error}"
            );
        })
        .ok()?;
    #[cfg(not(unix))]
    let mut command = Command::new(&config.path);

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Lead a fresh process group so a timeout kill also reaps any
        // grandchildren the hook script spawned.
        .process_group(0)
        .spawn()
        .map_err(|e| {
            tracing::error!(
                target: "cosh_hook",
                path = %safe_path,
                "external hook spawn failed: {e}"
            );
        })
        .ok()?;

    // One absolute deadline covers stdin delivery, process exit, and
    // stdout draining; overrunning any stage kills the whole group.
    let clamped_ms = config.timeout_ms.min(10_000);
    let deadline = Instant::now() + Duration::from_millis(clamped_ms);

    // Write stdin from a helper thread: a hook that never reads stdin must
    // not stall the caller, and the thread exits on EPIPE once the child
    // (or its killed process group) closes the read end. The receiver
    // reports delivery completion so it can be awaited under the deadline.
    let stdin_done_rx = spawn_stdin_writer(child.stdin.take(), input_json.into_bytes());

    // Read stdout from a helper thread so draining honors the deadline
    // even when a grandchild keeps the pipe open after the hook exits.
    let stdout_rx = spawn_stdout_reader(child.stdout.take());

    let remaining = deadline.saturating_duration_since(Instant::now());
    let status = match child.wait_timeout(remaining) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill_hook_tree(&mut child, &safe_path);
            tracing::warn!(
                target: "cosh_hook",
                path = %safe_path,
                timeout_ms = config.timeout_ms,
                "external hook timed out"
            );
            return None;
        }
        Err(e) => {
            kill_hook_tree(&mut child, &safe_path);
            tracing::warn!(
                target: "cosh_hook",
                path = %safe_path,
                "external hook wait failed: {e}"
            );
            return None;
        }
    };

    if !status.success() {
        // Report the real exit failure rather than a downstream I/O
        // timeout, and kill any descendants the failing hook left behind.
        kill_hook_process_group(child.id(), &safe_path);
        tracing::warn!(
            target: "cosh_hook",
            path = %safe_path,
            "external hook exited with error"
        );
        return None;
    }

    // The hook exited successfully; stdin delivery and stdout draining
    // must still both complete within the same absolute deadline.
    let remaining = deadline.saturating_duration_since(Instant::now());
    match stdin_done_rx.recv_timeout(remaining) {
        // Disconnected means the writer thread died; nothing left to wait on.
        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // A grandchild inherited stdin and never reads it: kill the
            // leftover group so the blocked writer unblocks via EPIPE.
            kill_hook_process_group(child.id(), &safe_path);
            tracing::warn!(
                target: "cosh_hook",
                path = %safe_path,
                timeout_ms = config.timeout_ms,
                "external hook stdin delivery timed out"
            );
            return None;
        }
    }

    let remaining = deadline.saturating_duration_since(Instant::now());
    let stdout_buf = match stdout_rx.recv_timeout(remaining) {
        Ok(buf) => buf,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The hook exited but a grandchild still holds the pipe: kill
            // the leftover group rather than stall past the deadline.
            kill_hook_process_group(child.id(), &safe_path);
            tracing::warn!(
                target: "cosh_hook",
                path = %safe_path,
                timeout_ms = config.timeout_ms,
                "external hook output drain timed out"
            );
            return None;
        }
        // The reader thread died without sending; treat as empty output.
        Err(mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&stdout_buf);
    if stdout.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<HookFinding>(stdout.trim())
        .map_err(|e| {
            tracing::warn!(
                target: "cosh_hook",
                path = %safe_path,
                "external hook invalid JSON output: {e}"
            );
        })
        .ok()
}

#[cfg(unix)]
fn command_for_pinned_hook(executable: &fs::File) -> io::Result<(Command, fs::File)> {
    let fd = nix::unistd::dup(executable.as_raw_fd())
        .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
    // `dup` clears CLOEXEC. Script interpreters must inherit this descriptor
    // so the platform's descriptor path remains valid through exec.
    let inherited = unsafe { fs::File::from_raw_fd(fd) };
    Ok((
        Command::new(format!("{PINNED_HOOK_FD_ROOT}/{fd}")),
        inherited,
    ))
}

/// Delivers the hook's stdin payload on a background thread; the receiver
/// reports completion (full write, EPIPE, or any other write error).
fn spawn_stdin_writer(
    stdin: Option<std::process::ChildStdin>,
    payload: Vec<u8>,
) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    match stdin {
        Some(mut stdin) => {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&payload);
                // Drop stdin before signalling so the child sees EOF.
                drop(stdin);
                let _ = tx.send(());
            });
        }
        None => {
            let _ = tx.send(());
        }
    }
    rx
}

/// Drains the hook's stdout (capped at 8 KiB) on a background thread; the
/// receiver yields the collected bytes once the cap or EOF is reached.
fn spawn_stdout_reader(stdout: Option<std::process::ChildStdout>) -> mpsc::Receiver<Vec<u8>> {
    const MAX_HOOK_OUTPUT: usize = 8192;
    let (tx, rx) = mpsc::channel();
    match stdout {
        Some(mut out) => {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = vec![0u8; MAX_HOOK_OUTPUT];
                let mut total_read = 0;
                loop {
                    let remaining = MAX_HOOK_OUTPUT - total_read;
                    if remaining == 0 {
                        break;
                    }
                    match out.read(&mut buf[total_read..]) {
                        Ok(0) => break,
                        Ok(n) => total_read += n,
                        Err(_) => break,
                    }
                }
                buf.truncate(total_read);
                let _ = tx.send(buf);
            });
        }
        None => {
            let _ = tx.send(Vec::new());
        }
    }
    rx
}

/// SIGKILLs the hook's process group, then fallback-kills and reaps the
/// still-running hook script itself.
fn kill_hook_tree(child: &mut std::process::Child, safe_path: &str) {
    kill_hook_process_group(child.id(), safe_path);
    let _ = child.kill();
    let _ = child.wait();
}

/// SIGKILLs the hook's process group; ESRCH means it already exited.
fn kill_hook_process_group(pgid: u32, safe_path: &str) {
    use nix::errno::Errno;
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    match killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(e) => {
            tracing::warn!(
                target: "cosh_hook",
                path = %safe_path,
                pgid,
                "failed to kill external hook process group: {e}"
            );
        }
    }
}
