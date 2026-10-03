//! Deadline- and size-bounded async child execution with whole-process-tree
//! cleanup.
//!
//! Shared by the shell tool and the hook system: `tokio::time::timeout`
//! around `output()` only cancels the future and leaks the process tree.
//! Here the child leads its own process group, the deadline covers stdin
//! writing, waiting, and output collection, and a RAII guard SIGKILLs the
//! group even when the calling future itself is cancelled. Output
//! collection is additionally capped per pipe so a runaway producer cannot
//! exhaust memory before the deadline fires.

use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

/// Maximum bytes to collect from a single stdout or stderr pipe.
///
/// Mirrors `cosh-platform::run_command`'s limit so the LLM shell tool and
/// hook execution honor the same runaway-output contract as the CLI
/// command surface (issue #2841).
pub const MAX_PIPE_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Child output collected under the per-pipe size limit.
///
/// When a truncated flag is set, the process group was killed at the size
/// limit and the pipe's bytes hold only the beginning of the stream.
#[derive(Debug)]
pub struct BoundedOutput {
    /// The child's exit status; on truncation, the status after the kill.
    pub status: ExitStatus,
    /// Collected stdout bytes, at most the size limit.
    pub stdout: Vec<u8>,
    /// Collected stderr bytes, at most the size limit.
    pub stderr: Vec<u8>,
    /// Whether stdout exceeded the size limit and was cut short.
    pub stdout_truncated: bool,
    /// Whether stderr exceeded the size limit and was cut short.
    pub stderr_truncated: bool,
}

/// Failure modes of [`output_with_timeout`].
#[derive(Debug)]
pub enum OutputError {
    /// The process could not be spawned.
    Spawn(std::io::Error),
    /// The process spawned but waiting or collecting output failed.
    Io(std::io::Error),
    /// The deadline expired; the process group was killed and reaped.
    Timeout,
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to spawn process: {e}"),
            Self::Io(e) => write!(f, "process I/O failed: {e}"),
            Self::Timeout => write!(f, "process timed out"),
        }
    }
}

/// SIGKILLs the child's process group on drop unless disarmed.
///
/// Covers cancellation of the caller's future: drop runs synchronously, and
/// tokio's orphan reaper collects the killed child afterwards.
struct ProcessGroupGuard {
    pgid: Option<u32>,
}

impl ProcessGroupGuard {
    fn new(pgid: Option<u32>) -> Self {
        Self { pgid }
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }

    /// Kills the group now and disarms; logs failures other than ESRCH.
    fn kill_now(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            if let Err(e) = cosh_platform::process::kill_process_group(pgid) {
                tracing::warn!(target: "cosh_process", pgid, "failed to kill process group: {e}");
            }
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// Runs `cmd` to completion within `timeout`, collecting at most
/// [`MAX_PIPE_OUTPUT_BYTES`] from each output pipe.
///
/// The child is spawned as its own process-group leader with stdout/stderr
/// piped; stdin is piped only when `stdin_data` is provided, otherwise null.
/// Stdin is written concurrently with output collection, and the deadline
/// covers writing, waiting for exit, and draining output, so a child (or
/// grandchild) that never reads stdin or holds the pipes open cannot stall the
/// caller past the deadline. Completion is decided by the child exiting and
/// both pipes reaching EOF; an unwritten stdin remainder is dropped rather
/// than waited for, because a child that exited without reading its input has
/// already produced whatever result it is going to produce.
///
/// When either pipe exceeds the limit, the whole process group is killed
/// immediately (before the deadline), the child is reaped, and the collected
/// head of each pipe is returned with the corresponding truncated flag set.
/// Callers serving an LLM should surface that partial output plus a hint on
/// how to retrieve the rest, not a bare error.
///
/// Cancelling the returned future SIGKILLs the process group via a drop
/// guard, falls back to `kill_on_drop` for the direct child, and aborts the
/// output reader tasks.
///
/// # Errors
///
/// - [`OutputError::Spawn`] if the process cannot be started.
/// - [`OutputError::Io`] if waiting or collecting output fails; the process
///   group is killed and the child reaped before returning.
/// - [`OutputError::Timeout`] if the deadline expires; the whole process
///   group receives SIGKILL, the direct child gets a fallback kill and is
///   explicitly reaped, and reader tasks are aborted before returning.
pub async fn output_with_timeout(
    cmd: Command,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<BoundedOutput, OutputError> {
    output_with_limit(cmd, stdin_data, timeout, MAX_PIPE_OUTPUT_BYTES).await
}

/// [`output_with_timeout`] with a caller-chosen per-pipe cap, so tests can
/// exercise the limit logic without materializing 32 MB of output.
async fn output_with_limit(
    mut cmd: Command,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<BoundedOutput, OutputError> {
    cosh_platform::process::isolate_process_group(cmd.as_std_mut());
    cmd.stdin(if stdin_data.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    })
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    // Direct-child fallback for the cancellation path, where only the
    // guard's killpg runs and could fail with a non-ESRCH error.
    .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(OutputError::Spawn)?;
    let mut guard = ProcessGroupGuard::new(child.id());

    let stdin = child.stdin.take();
    let mut stdout_task = spawn_reader(child.stdout.take(), max_output_bytes);
    let mut stderr_task = spawn_reader(child.stderr.take(), max_output_bytes);

    // Feed stdin from its own task rather than before the loop below. A
    // payload larger than the pipe buffer cannot be written to a child that
    // never reads it, and writing sequentially would block the loop before it
    // could observe a reader's truncation report: the runaway child would then
    // only be caught at the deadline and misreported as a timeout.
    let _stdin_task = match (stdin, stdin_data) {
        (Some(mut stdin), Some(data)) => Some(AbortOnDrop(tokio::spawn(async move {
            // The child may exit without reading stdin; broken pipes are fine.
            let _ = stdin.write_all(&data).await;
            let _ = stdin.shutdown().await;
        }))),
        _ => None,
    };

    let run = async {
        // Wait for exit and pipe completion concurrently. A pipe that hits
        // the size limit must kill the group right away: a runaway producer
        // would otherwise keep burning CPU and memory until the deadline.
        let mut status: Option<ExitStatus> = None;
        let mut stdout_out: Option<ReaderOutput> = None;
        let mut stderr_out: Option<ReaderOutput> = None;
        loop {
            if stdout_out.as_ref().is_some_and(|o| o.truncated)
                || stderr_out.as_ref().is_some_and(|o| o.truncated)
            {
                return Ok(RunOutcome::Truncated {
                    stdout: stdout_out,
                    stderr: stderr_out,
                });
            }
            if status.is_some() && stdout_out.is_some() && stderr_out.is_some() {
                match (status, stdout_out, stderr_out) {
                    (Some(status), Some(stdout), Some(stderr)) => {
                        return Ok(RunOutcome::Done {
                            status,
                            stdout: stdout.bytes,
                            stderr: stderr.bytes,
                        });
                    }
                    // The completeness check above guarantees the shape.
                    _ => unreachable!("completeness check passed but a result is missing"),
                }
            }
            tokio::select! {
                st = child.wait(), if status.is_none() => status = Some(st?),
                res = stdout_task.rx.recv(), if stdout_out.is_none() => {
                    stdout_out = Some(recv_reader_result(res)?);
                }
                res = stderr_task.rx.recv(), if stderr_out.is_none() => {
                    stderr_out = Some(recv_reader_result(res)?);
                }
            }
        }
    };

    // One absolute deadline covers the run phase and the post-truncation
    // drain, so the total bound holds no matter how many pipes still need
    // draining afterwards.
    let deadline = tokio::time::Instant::now() + timeout;
    let result = tokio::time::timeout_at(deadline, run).await;
    match result {
        Ok(Ok(RunOutcome::Done {
            status,
            stdout,
            stderr,
        })) => {
            guard.disarm();
            Ok(BoundedOutput {
                status,
                stdout,
                stderr,
                stdout_truncated: false,
                stderr_truncated: false,
            })
        }
        Ok(Ok(RunOutcome::Truncated {
            stdout: stdout_done,
            stderr: stderr_done,
        })) => {
            let status = kill_and_reap(&mut guard, &mut child)
                .await
                .map_err(OutputError::Io)?;
            // The overflowed pipe already reported; the other one may still
            // be reading. Bound the drain so a descendant that escapes the
            // process group (e.g. via setsid) and keeps the pipe open cannot
            // block past the original deadline (issue #2841).
            let stdout = join_or_default(&mut stdout_task, stdout_done, deadline)
                .await
                .map_err(OutputError::Io)?;
            let stderr = join_or_default(&mut stderr_task, stderr_done, deadline)
                .await
                .map_err(OutputError::Io)?;
            Ok(BoundedOutput {
                status,
                stdout: stdout.bytes,
                stderr: stderr.bytes,
                stdout_truncated: stdout.truncated,
                stderr_truncated: stderr.truncated,
            })
        }
        Ok(Err(e)) => {
            let _ = kill_and_reap(&mut guard, &mut child).await;
            Err(OutputError::Io(e))
        }
        Err(_) => {
            let _ = kill_and_reap(&mut guard, &mut child).await;
            Err(OutputError::Timeout)
        }
    }
    // Reader tasks are aborted by their drop guards on every exit path.
}

/// Terminal state of the monitored run.
enum RunOutcome {
    /// The child exited and both pipes drained within the size limit.
    Done {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// A pipe exceeded the size limit; the group still needs killing. The
    /// options carry the already-collected reader results, if any.
    Truncated {
        stdout: Option<ReaderOutput>,
        stderr: Option<ReaderOutput>,
    },
}

/// SIGKILLs the process group, then fallback-kills and reaps the child.
///
/// Returns the child's final exit status; the signal kill normally makes
/// it a signaled status with no exit code.
async fn kill_and_reap(
    guard: &mut ProcessGroupGuard,
    child: &mut Child,
) -> std::io::Result<ExitStatus> {
    guard.kill_now();
    // Fallback for the direct child; harmless if the group kill landed.
    let _ = child.start_kill();
    child.wait().await
}

/// Reader task handle that aborts on drop, so neither cancellation nor an
/// early error return leaves a detached task behind. Results arrive over
/// the channel rather than the JoinHandle: a completed JoinHandle panics
/// when polled again, while a channel with a pending message stays safely
/// pollable across select rounds that picked another branch.
struct ReaderTask {
    handle: JoinHandle<()>,
    rx: tokio::sync::mpsc::UnboundedReceiver<std::io::Result<ReaderOutput>>,
}

/// Aborts the wrapped task on drop, so a stdin writer still blocked on a child
/// that never read its input cannot outlive the call.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Output collected from one pipe plus whether it hit the size limit.
struct ReaderOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_reader<R>(reader: Option<R>, max_bytes: usize) -> ReaderTask
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
        let result = read_bounded(reader, max_bytes).await;
        // The receiver may already be gone (caller cancelled); dropping the
        // result then is fine, the abort guard owns the cleanup.
        let _ = tx.send(result);
    });
    ReaderTask { handle, rx }
}

async fn read_bounded<R>(reader: Option<R>, max_bytes: usize) -> std::io::Result<ReaderOutput>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let Some(mut r) = reader else {
        return Ok(ReaderOutput {
            bytes: buf,
            truncated: false,
        });
    };
    let mut chunk = [0u8; 8192];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(ReaderOutput {
                bytes: buf,
                truncated: false,
            });
        }
        if buf.len() + n > max_bytes {
            // Keep at most max_bytes; stopping early marks truncation
            // and lets the caller kill the still-running group.
            let take = max_bytes.saturating_sub(buf.len());
            buf.extend_from_slice(&chunk[..take]);
            return Ok(ReaderOutput {
                bytes: buf,
                truncated: true,
            });
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Converts a channel receive into a reader result. `None` means the task
/// ended without reporting, which only happens on cancellation races.
fn recv_reader_result(res: Option<std::io::Result<ReaderOutput>>) -> std::io::Result<ReaderOutput> {
    res.unwrap_or_else(|| {
        Err(std::io::Error::other(
            "output reader ended without reporting a result",
        ))
    })
}

/// Awaits a reader task's result, surfacing read errors and join failures
/// so output collection failures reach the caller's kill/reap branch as
/// I/O errors.
async fn join_reader(task: &mut ReaderTask) -> std::io::Result<ReaderOutput> {
    recv_reader_result(task.rx.recv().await)
}

/// Awaits a reader task's result up to `deadline`, returning an empty output
/// if the deadline fires. This prevents a descendant that escapes the process
/// group and keeps a pipe open from blocking the caller past the original
/// timeout.
///
/// On deadline the bytes already buffered are lost: the reader task owns its
/// buffer and only hands it over through the channel when it finishes. The
/// caller still learns the output was cut short, because the pipe that hit
/// the size limit always reports through `done` with its flag intact.
async fn join_or_default(
    task: &mut ReaderTask,
    done: Option<ReaderOutput>,
    deadline: tokio::time::Instant,
) -> std::io::Result<ReaderOutput> {
    match done {
        Some(out) => Ok(out),
        None => match tokio::time::timeout_at(deadline, join_reader(task)).await {
            Ok(res) => res,
            Err(_) => Ok(ReaderOutput {
                bytes: Vec::new(),
                truncated: false,
            }),
        },
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared fixtures for process-tree cleanup regression tests.

    use std::path::{Path, PathBuf};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    /// Serializes real process-tree fixtures inside one test binary.
    ///
    /// These tests intentionally create and SIGKILL process groups. Running
    /// several at once can delay shell reaping enough for a marker deadline
    /// to fire under CI load, even though each isolated cleanup succeeds.
    pub async fn exclusive_process_tree_test() -> tokio::sync::OwnedMutexGuard<()> {
        static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
        LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
            .lock_owned()
            .await
    }

    fn marker_trigger(marker: &Path) -> PathBuf {
        marker.with_extension("trigger")
    }

    /// Shell script that backgrounds a grandchild which writes `marker`
    /// after the test releases its probe, records `<shell-pid>
    /// <grandchild-pid>` into `pids`, then blocks far past any test timeout.
    pub fn leak_script(marker: &Path, pids: &Path) -> String {
        let trigger = marker_trigger(marker);
        format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; : > '{}') & \
             echo $$ $! > '{}'; sleep 30",
            trigger.display(),
            marker.display(),
            pids.display()
        )
    }

    /// Variant of [`leak_script`] whose direct child exits successfully at
    /// once, leaving the grandchild as the only holder of stdout/stderr.
    pub fn stdout_holder_script(marker: &Path, pids: &Path) -> String {
        let trigger = marker_trigger(marker);
        format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; : > '{}') & \
             echo $$ $! > '{}'; exit 0",
            trigger.display(),
            marker.display(),
            pids.display()
        )
    }

    /// Reads the two PIDs recorded by [`leak_script`], polling briefly
    /// because the child writes them right after spawning.
    pub fn read_pids(path: &Path) -> Vec<i32> {
        for _ in 0..100 {
            if let Ok(content) = std::fs::read_to_string(path) {
                let pids: Vec<i32> = content
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

    /// Best-effort SIGKILL of the recorded PIDs and their groups so a
    /// failed assertion does not leak processes into CI.
    pub struct PidCleanup(pub Vec<i32>);

    impl Drop for PidCleanup {
        fn drop(&mut self) {
            for pid in &self.0 {
                let _ = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("kill -9 -- -{pid} {pid} 2>/dev/null"))
                    .status();
            }
        }
    }

    /// Asserts `pid` terminates within 2.5s.
    pub fn assert_process_gone(pid: i32) {
        for _ in 0..125 {
            if !process_can_run(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("process {pid} survived the timeout kill");
    }

    /// Whether `pid` can still execute code. Zombie (Z) and dead (X)
    /// states count as terminated: SIGKILL already landed but the parent
    /// (e.g. tokio's best-effort orphan reaper) has not reaped the entry
    /// yet, and `kill -0` would still report such a PID as alive.
    ///
    /// Fails closed: an unrunnable or misbehaving `ps` panics instead of
    /// letting the liveness assertion pass vacuously.
    fn process_can_run(pid: i32) -> bool {
        let output = std::process::Command::new("ps")
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

    /// Releases the survivor probe and gives any leaked grandchild time to
    /// write its marker without depending on scheduler-relative deadlines.
    pub fn release_marker_probe(marker: &Path) {
        std::fs::write(marker_trigger(marker), b"release").expect("release marker probe");
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::test_support::*;
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[tokio::test]
    async fn collects_output_and_status() {
        let out = output_with_limit(
            sh("printf out; printf err >&2"),
            None,
            Duration::from_secs(5),
            MAX_PIPE_OUTPUT_BYTES,
        )
        .await
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert!(!out.stdout_truncated);
        assert!(!out.stderr_truncated);
    }

    #[tokio::test]
    async fn stdin_is_delivered() {
        let out = output_with_limit(
            sh("cat"),
            Some(b"ping".to_vec()),
            Duration::from_secs(5),
            MAX_PIPE_OUTPUT_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(out.stdout, b"ping");
    }

    #[tokio::test]
    async fn stdout_over_limit_returns_partial_head() {
        // The child finishes quickly, but its output crosses the limit, so
        // the collected head is capped and the truncation flag is set.
        let out = output_with_limit(
            sh("head -c 5000 /dev/zero"),
            None,
            Duration::from_secs(5),
            1024,
        )
        .await
        .unwrap();
        assert!(out.stdout_truncated);
        assert_eq!(out.stdout.len(), 1024);
        assert!(!out.stderr_truncated);
        // The exit status is a race here: `head` may finish writing into the
        // pipe buffer and exit 0 before the reader hits the limit, so the
        // status can be a genuine success. Callers must treat the truncated
        // flag, not the status, as the error signal.
    }

    #[tokio::test]
    async fn stderr_over_limit_flags_truncation() {
        let out = output_with_limit(
            sh("head -c 5000 /dev/zero >&2"),
            None,
            Duration::from_secs(5),
            1024,
        )
        .await
        .unwrap();
        assert!(out.stderr_truncated);
        assert_eq!(out.stderr.len(), 1024);
        assert!(!out.stdout_truncated);
    }

    #[tokio::test]
    async fn stdout_over_limit_kills_group_before_deadline() {
        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let trigger = marker.with_extension("trigger");
        // Emit output past the limit, then keep the group alive via a
        // backgrounded grandchild holding stdout; `sleep 30` ensures the
        // direct child also outlives the deadline without the early kill.
        let script = format!(
            "(while [ ! -e '{}' ]; do sleep 0.05; done; : > '{}') & \
             echo $$ $! > '{}'; \
             head -c 4096 /dev/zero; \
             sleep 30",
            trigger.display(),
            marker.display(),
            pid_file.display()
        );

        let started = Instant::now();
        let out = output_with_limit(sh(&script), None, Duration::from_secs(2), 1024)
            .await
            .unwrap();
        assert!(out.stdout_truncated);
        assert_eq!(out.stdout.len(), 1024);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "truncation must return before the deadline, took {:?}",
            started.elapsed()
        );

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        for pid in &pids {
            assert_process_gone(*pid);
        }
        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the truncation kill");
    }

    #[tokio::test]
    async fn timeout_kills_grandchildren() {
        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");

        let err = output_with_limit(
            sh(&leak_script(&marker, &pid_file)),
            None,
            Duration::from_millis(300),
            MAX_PIPE_OUTPUT_BYTES,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OutputError::Timeout));

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        for pid in &pids {
            assert_process_gone(*pid);
        }
        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the timeout");
    }

    #[tokio::test]
    async fn unread_stdin_respects_deadline() {
        // Larger than any pipe buffer: writing it to a child that never
        // reads stdin must still be bounded by the deadline.
        let payload = vec![b'x'; 1 << 20];
        let started = Instant::now();
        let err = output_with_limit(
            sh("sleep 30"),
            Some(payload),
            Duration::from_millis(300),
            MAX_PIPE_OUTPUT_BYTES,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OutputError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_child_ignoring_stdin_still_reports_truncation_early() {
        let _fixture_guard = exclusive_process_tree_test().await;
        // A hook payload carrying conversation history easily exceeds the pipe
        // buffer. If stdin writing were sequential, write_all would block on a
        // child that never reads it, the reader's truncation report would never
        // be observed, and the runaway child would be classified as a timeout
        // at the deadline instead of as truncated output.
        let payload = vec![b'x'; 1 << 20];
        // Returning Ok at all is the assertion: with a sequential write_all this
        // call ended in Err(Timeout), because write_all blocked on a child that
        // never reads stdin and the loop observing the reader's truncation
        // report was never reached. The outcome separates the two
        // implementations on its own, so no wall-clock bound is needed here.
        let out = output_with_limit(
            sh("head -c 4096 /dev/zero; sleep 30"),
            Some(payload),
            Duration::from_secs(5),
            1024,
        )
        .await
        .unwrap();
        assert!(out.stdout_truncated);
        assert_eq!(out.stdout.len(), 1024);
    }

    #[tokio::test]
    async fn a_finished_child_completes_while_a_descendant_holds_stdin() {
        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("bgpid");
        // The shell exits at once and both output pipes reach EOF, but a
        // backgrounded descendant keeps the stdin read end open without ever
        // reading it, so a payload larger than the pipe buffer can never finish
        // being written.
        let script = format!(
            "(sleep 30) <&0 >/dev/null 2>&1 & echo $! > '{}'; exit 0",
            pid_file.display()
        );

        let started = Instant::now();
        let out = output_with_limit(
            sh(&script),
            Some(vec![b'x'; 1 << 20]),
            Duration::from_secs(5),
            MAX_PIPE_OUTPUT_BYTES,
        )
        .await
        .unwrap();

        assert_eq!(out.status.code(), Some(0));
        // Completion is decided by the child exiting and both pipes reaching
        // EOF. Waiting for the unwritten remainder instead would report a
        // timeout for a hook that already succeeded, and would make the outcome
        // depend on the payload size: the same script with a payload that fits
        // the pipe buffer has always completed here. The bound is generous
        // because it only needs to catch waiting on the remainder, which would
        // consume the entire deadline.
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "completion waited on the unwritten stdin remainder: {:?}",
            started.elapsed()
        );

        // Reap the descendant this test deliberately leaves behind; a process
        // that outlives a successful run is not killed, exactly as a hook
        // launching a background service expects.
        let pid = std::fs::read_to_string(&pid_file)
            .expect("read backgrounded descendant PID")
            .trim()
            .to_string();
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid)
            .status();
    }

    #[tokio::test]
    async fn grandchild_holding_stdout_cannot_stall_past_deadline() {
        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");

        let started = Instant::now();
        let err = output_with_limit(
            sh(&stdout_holder_script(&marker, &pid_file)),
            None,
            Duration::from_millis(300),
            MAX_PIPE_OUTPUT_BYTES,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OutputError::Timeout));
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "output drain must return at the deadline, not when the grandchild exits"
        );

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        // The grandchild pipe holder must be killed with the group.
        assert_process_gone(pids[1]);
        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the drain timeout");
    }

    #[tokio::test]
    async fn drain_gives_up_at_deadline_when_the_pipe_never_closes() {
        // Stands in for a descendant that escaped the process group: it
        // still holds the pipe's write end, so the reader never sees EOF.
        // The drain must give up at the deadline instead of blocking the
        // caller (issue #2841). Keeping the writer alive reproduces that
        // without depending on `setsid` being installed.
        let (_writer, reader) = tokio::io::duplex(64);
        let mut task = spawn_reader(Some(reader), MAX_PIPE_OUTPUT_BYTES);

        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        let out = join_or_default(&mut task, None, deadline).await.unwrap();

        assert!(out.bytes.is_empty());
        assert!(!out.truncated);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "drain must return at the deadline, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn drain_keeps_a_reported_result_past_the_deadline() {
        // The pipe that hit the size limit reports through `done`. An
        // expired deadline must not discard its bytes or its flag, or the
        // caller would lose both the head and the truncation signal.
        let (_writer, reader) = tokio::io::duplex(64);
        let mut task = spawn_reader(Some(reader), MAX_PIPE_OUTPUT_BYTES);
        let done = ReaderOutput {
            bytes: b"head".to_vec(),
            truncated: true,
        };
        let expired = tokio::time::Instant::now() - Duration::from_secs(1);

        let out = join_or_default(&mut task, Some(done), expired)
            .await
            .unwrap();

        assert_eq!(out.bytes, b"head");
        assert!(out.truncated);
    }

    // Multi-threaded runtime: the test thread polls with blocking sleeps
    // while the cancelled future runs on another worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_caller_kills_process_tree() {
        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let script = leak_script(&marker, &pid_file);

        let handle = tokio::spawn(output_with_limit(
            sh(&script),
            None,
            Duration::from_secs(30),
            MAX_PIPE_OUTPUT_BYTES,
        ));

        // The pid file proves the child is running before we cancel.
        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        handle.abort();
        let _ = handle.await;

        for pid in &pids {
            assert_process_gone(*pid);
        }
        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived caller cancellation");
    }
}
