#![forbid(unsafe_code)]
#![allow(
    clippy::result_large_err,
    reason = "crate APIs share public CoshError; per-function allows obscure one API trade-off"
)]
//! cosh-platform: Distribution Abstraction Layer for the cosh deterministic interaction layer.
//!
//! Detects the current distro and routes pkg/svc operations to the
//! appropriate backend (dnf, apt, zypper, etc.).

pub mod audit;
pub mod checkpoint;
pub mod detect;
pub mod pkg;
pub mod process;
pub mod svc;

pub mod validate;

use std::io::Read;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cosh_types::error::{CoshError, ErrorCode};

const PKG_TIMEOUT: Duration = Duration::from_secs(120);
const SVC_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum bytes to collect from a single stdout or stderr pipe.
/// Output beyond this limit is treated as an error to prevent OOM.
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Run an external command with a timeout. Reads stdout/stderr in background
/// threads to avoid pipe-buffer deadlock. Returns `ErrorCode::Timeout` if the
/// process exceeds the deadline, or `ErrorCode::OutputTooLarge` if either pipe
/// exceeds `MAX_OUTPUT_BYTES`. Truncation is detected while the child still
/// runs, so `OutputTooLarge` is returned before the deadline even when a
/// grandchild keeps the process group alive past the size limit. The deadline
/// also covers draining stdout and stderr: a grandchild that keeps the pipes
/// open after the direct child exited gets its whole process group killed
/// instead of stalling the caller.
pub fn run_command(
    cmd: &mut Command,
    timeout: Duration,
    subsystem: &str,
) -> Result<Output, CoshError> {
    // Lead a fresh process group so a timeout can reap grandchildren too.
    process::isolate_process_group(cmd);
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            CoshError::new(
                ErrorCode::Unknown,
                format!("Failed to spawn command: {}", e),
                subsystem,
            )
        })?;
    let pgid = child.id();

    // Drain pipes in background threads to prevent buffer-full deadlock;
    // results come back over channels so draining can honor the deadline.
    let stdout_rx = drain_pipe(child.stdout.take());
    let stderr_rx = drain_pipe(child.stderr.take());

    let deadline = Instant::now() + timeout;
    // Cache outputs received early so the post-exit drain does not
    // re-consume them. Polling here also lets the loop detect a truncated
    // pipe while the child still runs: without it a grandchild that keeps
    // the group alive past the size limit would mask OutputTooLarge as
    // Timeout by waiting until the deadline.
    let mut stdout_out: Option<DrainedOutput> = None;
    let mut stderr_out: Option<DrainedOutput> = None;
    let status = loop {
        if probe_truncated(&stdout_rx, &mut stdout_out) {
            kill_group_and_reap(&mut child);
            return Err(output_too_large_error("stdout", subsystem));
        }
        if probe_truncated(&stderr_rx, &mut stderr_out) {
            kill_group_and_reap(&mut child);
            return Err(output_too_large_error("stderr", subsystem));
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group_and_reap(&mut child);
                    return Err(timeout_error(timeout, subsystem));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                kill_group_and_reap(&mut child);
                return Err(CoshError::new(
                    ErrorCode::Unknown,
                    format!("Failed to wait for command: {}", e),
                    subsystem,
                ));
            }
        }
    };

    // The direct child has exited, but a grandchild may still hold the
    // pipes open; drain both in parallel so a truncated stderr is not
    // masked by a blocking stdout recv (or vice versa).
    loop {
        if probe_truncated(&stdout_rx, &mut stdout_out) {
            kill_group(pgid);
            return Err(output_too_large_error("stdout", subsystem));
        }
        if probe_truncated(&stderr_rx, &mut stderr_out) {
            kill_group(pgid);
            return Err(output_too_large_error("stderr", subsystem));
        }
        if stdout_out.is_some() && stderr_out.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            return Err(timeout_error(timeout, subsystem));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let stdout = stdout_out.unwrap();
    let stderr = stderr_out.unwrap();

    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn timeout_error(timeout: Duration, subsystem: &str) -> CoshError {
    CoshError::new(
        ErrorCode::Timeout,
        format!("Command timed out after {}s", timeout.as_secs()),
        subsystem,
    )
    .recoverable(true)
    .with_hint("The operation took too long. Retry or check system load.")
}

fn output_too_large_error(pipe: &str, subsystem: &str) -> CoshError {
    CoshError::new(
        ErrorCode::OutputTooLarge,
        format!("Command {pipe} exceeded {} bytes", MAX_OUTPUT_BYTES),
        subsystem,
    )
}

/// SIGKILLs the whole group, then fallback-kills and reaps the direct child.
fn kill_group_and_reap(child: &mut std::process::Child) {
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_group(pgid: u32) {
    if let Err(e) = process::kill_process_group(pgid) {
        tracing::warn!(
            target: "cosh_platform",
            pgid,
            "failed to kill timed-out process group: {e}"
        );
    }
}

/// Output collected from one pipe, plus a flag indicating whether the
/// pipe was truncated because it exceeded the size limit.
struct DrainedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Drains one output pipe on a background thread; the receiver yields the
/// collected bytes once the pipe reaches EOF or the size limit is hit.
fn drain_pipe<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<DrainedOutput> {
    let (tx, rx) = mpsc::channel();
    match pipe {
        Some(r) => {
            std::thread::spawn(move || {
                let mut buf = Vec::with_capacity(4096);
                let mut reader = std::io::BufReader::new(r);
                let mut truncated = false;
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            if buf.len() > MAX_OUTPUT_BYTES.saturating_sub(n) {
                                let take = MAX_OUTPUT_BYTES.saturating_sub(buf.len());
                                buf.extend_from_slice(&chunk[..take]);
                                truncated = true;
                                break;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        Err(_) => break,
                    }
                }
                let _ = tx.send(DrainedOutput {
                    bytes: buf,
                    truncated,
                });
            });
        }
        None => {
            let _ = tx.send(DrainedOutput {
                bytes: Vec::new(),
                truncated: false,
            });
        }
    }
    rx
}

/// Non-blocking probe of a drained pipe.
///
/// Returns `true` when the pipe has reported truncation, so the caller
/// can kill the group and surface `OutputTooLarge` before the deadline.
/// A non-truncated result is stashed in `cached` so a later probe (in the
/// wait loop or the post-exit drain) does not re-consume it. A
/// `Disconnected` channel is stashed as empty output so the post-exit
/// drain does not stall waiting for a result that will never arrive.
fn probe_truncated(rx: &mpsc::Receiver<DrainedOutput>, cached: &mut Option<DrainedOutput>) -> bool {
    if cached.is_some() {
        return false;
    }
    match rx.try_recv() {
        Ok(out) if out.truncated => true,
        Ok(out) => {
            *cached = Some(out);
            false
        }
        Err(mpsc::TryRecvError::Empty) => false,
        Err(mpsc::TryRecvError::Disconnected) => {
            *cached = Some(DrainedOutput {
                bytes: Vec::new(),
                truncated: false,
            });
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Best-effort SIGKILL of recorded PIDs and their groups so a failed
    /// assertion does not leak processes into CI.
    struct PidCleanup(Vec<i32>);

    impl Drop for PidCleanup {
        fn drop(&mut self) {
            for pid in &self.0 {
                let _ = Command::new("sh")
                    .arg("-c")
                    .arg(format!("kill -9 -- -{pid} {pid} 2>/dev/null"))
                    .status();
            }
        }
    }

    /// Whether `pid` can still execute code. Zombie (Z) and dead (X)
    /// states count as terminated: SIGKILL already landed but the parent
    /// has not reaped the entry yet, and `kill -0` would still report
    /// such a PID as alive.
    ///
    /// Fails closed: an unrunnable or misbehaving `ps` panics instead of
    /// letting the liveness assertion pass vacuously.
    fn process_can_run(pid: i32) -> bool {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("failed to run ps to check process state");
        let stat = String::from_utf8_lossy(&output.stdout);
        match stat.trim().chars().next() {
            Some('Z' | 'X') => false,
            Some(_) => true,
            // No stat line: ps signals "no such process" via non-zero
            // exit. A successful exit without output is a ps anomaly and
            // must fail the test rather than report the PID as gone.
            None => {
                assert!(
                    !output.status.success() && output.stderr.is_empty(),
                    "ps failed without reporting that pid {pid} is absent: status={}, stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                false
            }
        }
    }

    /// Reads the `<shell-pid> <grandchild-pid>` pair, polling briefly
    /// because the child writes it right after spawning.
    fn read_pids(path: &std::path::Path) -> Vec<i32> {
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(path) {
                let pids: Vec<i32> = text
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                if pids.len() == 2 {
                    return pids;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("pid file {} was never fully written", path.display());
    }

    /// Asserts `pid` terminates within 2.5s, well before the grandchild's
    /// scheduled marker write at 5s.
    fn assert_process_gone(pid: i32) {
        for _ in 0..125 {
            if !process_can_run(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("process {pid} survived the timeout kill");
    }

    /// Releases the survivor probe and gives any leaked grandchild time to
    /// write its marker without depending on scheduler-relative deadlines.
    fn release_marker_probe(marker: &std::path::Path) {
        std::fs::write(marker.with_extension("trigger"), b"release").expect("release marker probe");
        std::thread::sleep(Duration::from_millis(250));
    }

    #[test]
    fn run_command_timeout_kills_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let trigger = marker.with_extension("trigger");
        // The direct child records `<shell-pid> <grandchild-pid>` and
        // blocks; a surviving grandchild writes only after the test probe.
        let script = format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; : > '{}') & \
             echo $$ $! > '{}'; sleep 30",
            trigger.display(),
            marker.display(),
            pid_file.display()
        );

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        // Leave enough startup budget for a loaded CI host to spawn the
        // grandchild and persist both PIDs before the timeout kills the group.
        let err = run_command(&mut cmd, Duration::from_secs(1), "test").unwrap_err();
        assert!(matches!(err.code, ErrorCode::Timeout));

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());

        for pid in &pids {
            assert_process_gone(*pid);
        }

        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the timeout");
    }

    #[test]
    fn run_command_drain_respects_deadline_when_grandchild_holds_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let trigger = marker.with_extension("trigger");
        // The direct child exits immediately with success, but its
        // backgrounded grandchild inherits stdout and keeps it open.
        let script = format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; : > '{}') & \
             echo $$ $! > '{}'; exit 0",
            trigger.display(),
            marker.display(),
            pid_file.display()
        );

        let started = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        // This fixture also needs its PID handshake to complete before the
        // deadline; the explicit trigger keeps the survivor probe deterministic.
        let err = run_command(&mut cmd, Duration::from_secs(1), "test").unwrap_err();
        assert!(matches!(err.code, ErrorCode::Timeout));
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "drain must return at the deadline, not when the grandchild exits"
        );

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());

        // The grandchild pipe holder must be killed with the group.
        assert_process_gone(pids[1]);

        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the drain timeout");
    }

    #[test]
    fn drain_pipe_truncates_when_output_exceeds_limit() {
        let data = vec![b'x'; MAX_OUTPUT_BYTES + 1];
        let rx = drain_pipe(Some(std::io::Cursor::new(data)));
        let output = rx.recv().expect("drain thread should send output");
        assert!(output.truncated, "output should be marked truncated");
        assert_eq!(output.bytes.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn drain_pipe_keeps_small_output_intact() {
        let data = vec![b'h'; 100];
        let rx = drain_pipe(Some(std::io::Cursor::new(data.clone())));
        let output = rx.recv().expect("drain thread should send output");
        assert!(!output.truncated);
        assert_eq!(output.bytes, data);
    }

    #[test]
    fn run_command_rejects_oversized_stdout() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("yes | head -c {}", MAX_OUTPUT_BYTES + 1));
        let err = run_command(&mut cmd, Duration::from_secs(10), "test").unwrap_err();
        assert!(
            matches!(err.code, ErrorCode::OutputTooLarge),
            "expected OutputTooLarge, got {:?}: {}",
            err.code,
            err.message
        );
    }

    /// A command that emits output past the size limit and then keeps the
    /// process group alive must return `OutputTooLarge` before the deadline,
    /// not `Timeout`. Reproduces the P2 finding where the wait loop only
    /// watched `try_wait` + deadline and read `truncated` afterwards, so a
    /// grandchild holding the pipes open masked the size error as a timeout.
    #[test]
    fn run_command_output_too_large_beats_timeout_and_kills_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let trigger = marker.with_extension("trigger");
        // Emit output past the size limit, then keep the group alive via a
        // backgrounded grandchild holding stdout; `sleep 30` ensures the
        // direct child also outlives the deadline without the early kill.
        let script = format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; : > '{}') & \
             echo $$ $! > '{}'; \
             head -c {} /dev/zero; \
             sleep 30",
            trigger.display(),
            marker.display(),
            pid_file.display(),
            MAX_OUTPUT_BYTES + 1
        );

        let started = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        // 2s is well past the time `head` needs to emit the oversized output,
        // so an implementation that only checks truncation after the child
        // exits would return Timeout here instead of OutputTooLarge.
        let err = run_command(&mut cmd, Duration::from_secs(2), "test").unwrap_err();
        assert!(
            matches!(err.code, ErrorCode::OutputTooLarge),
            "expected OutputTooLarge, got {:?}: {}",
            err.code,
            err.message
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "OutputTooLarge must return before the deadline, took {:?}",
            started.elapsed()
        );

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());

        // The grandchild pipe/process holder must be killed with the group.
        for pid in &pids {
            assert_process_gone(*pid);
        }

        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the truncation kill");
    }

    /// After the direct child exits, a grandchild holds stdout open and
    /// also overflows stderr past the size limit.  The post-exit drain must
    /// detect the truncated stderr in parallel with the open stdout, not
    /// block on stdout until the deadline and return Timeout.
    #[test]
    fn run_command_stderr_too_large_after_child_exit_with_grandchild_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pids");
        // Direct child exits immediately; the backgrounded grandchild
        // inherits stdout (keeping it open via sleep) and overflows stderr.
        let script = format!(
            "(sleep 0.3; head -c {} /dev/zero >&2; sleep 30) & \
             echo $$ $! > '{}'; exit 0",
            MAX_OUTPUT_BYTES + 1,
            pid_file.display()
        );

        let started = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        // 10s is well past the time head needs to overflow stderr (~1s), but
        // well before the grandchild's sleep 30 ends.  The old sequential
        // drain blocked on stdout until the deadline and returned Timeout;
        // the parallel drain must surface OutputTooLarge immediately.
        let err = run_command(&mut cmd, Duration::from_secs(10), "test").unwrap_err();
        assert!(
            matches!(err.code, ErrorCode::OutputTooLarge),
            "expected OutputTooLarge for stderr, got {:?}: {}",
            err.code,
            err.message
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "OutputTooLarge must return well before the deadline, took {:?}",
            started.elapsed()
        );

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        for pid in &pids {
            assert_process_gone(*pid);
        }
    }
}
