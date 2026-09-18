use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::broker;
use super::command_risk::CommandShape;
use super::command_risk_parser::{parse_command, SegmentConnector};
use super::readonly_pipeline::{
    error, limit_clean_text, wait_child_with_deadline, ReadonlyPipelineConfig,
    ReadonlyPipelineError, ReadonlyPipelineOutput,
};

/// Execution plan for a readonly compound or a single stderr-suppressed command.
/// The plan carries parser tokens verbatim: steps are spawned directly
/// with `std::process::Command`, so no shell parsing layer ever touches
/// the assessed text and every expansion mechanism (history, glob,
/// tilde, parameter, alias, ...) is structurally inert — the assessed
/// token sequence *is* the executed argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadonlyCompoundPlan {
    pub(crate) steps: Vec<ReadonlyCompoundStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadonlyCompoundStep {
    /// Connector between this step and the previous one; ignored on the
    /// first step, which always runs.
    pub(crate) connector: SegmentConnector,
    /// Trusted absolute path resolved at plan-build time, so the
    /// eligibility verdict and the executed binary can never diverge.
    pub(crate) program: PathBuf,
    pub(crate) argv: Vec<String>,
    /// Connect stderr to the null device rather than capturing it.
    pub(crate) suppress_stderr: bool,
}

/// Builds a readonly argv plan without overriding the caller's risk policy.
/// Assessment and execution use the same builder, so the validated argv is
/// the argv that runs. Returns `None` for shapes that still need approval.
///
/// A simple command with exclusively `2>` / `2>>` null sinks uses one step
/// with null stderr. All other simple commands keep their existing route.
/// Compound eligibility rules (design §2):
/// 1. shape is `AndOrList` or `Sequence` (all other shapes fail closed);
/// 2. no null-redirections were stripped by the parser (stripping loses
///    the user's output-suppression intent);
/// 3. at least two segments, and no empty segment was swallowed
///    (connector count must equal the gap count);
/// 4. every segment is exactly one stage (no pipeline segments);
/// 5. every token is free of `$` and backtick (the executor does not
///    expand, so expansion intent would diverge from execution);
/// 6. every segment's token sequence passes the readonly allowlist via
///    the same token-level predicate the broker uses (single source of
///    truth; no text re-splitting, so quoted arguments keep their
///    boundaries);
/// 7. every segment's command resolves to a binary in the trusted
///    system directories — eligibility and executability are one
///    verdict, so a grant can never come back 127 at run time (a name
///    only reachable through `PATH`, e.g. a rustup or Homebrew tool,
///    stays on the AskUser path instead);
/// 8. no segment is a context-observing command (`env`, `printenv`,
///    `which`, `tty`): the executor runs a controlled environment
///    with a null stdin, and reporting that context as if it were
///    the interactive shell's live state would mislead — or, for
///    `tty`, invert `&&`/`||` branches — so such requests stay on
///    the AskUser path where the handoff shows the real shell state;
/// 9. no segment would read from stdin: the executor's stdin is
///    always null, so a bare `-` operand or a default-to-stdin
///    filter without a file operand would report an instant
///    empty-input result where the interactive shell reads the
///    terminal instead.
pub(crate) fn build_readonly_compound_plan(command: &str) -> Option<ReadonlyCompoundPlan> {
    let parsed = parse_command(command);
    let suppress_stderr =
        parsed.shape == CommandShape::Simple && parsed.null_redirections.is_stderr_only();
    let segments = if suppress_stderr {
        if parsed.requires_shell_expansion {
            return None;
        }
        vec![parsed.stages]
    } else {
        if !matches!(
            parsed.shape,
            CommandShape::AndOrList | CommandShape::Sequence
        ) || !parsed.null_redirections.is_empty()
            || parsed.segments.len() < 2
            || parsed.segment_connectors.len() != parsed.segments.len() - 1
        {
            // Missing segments/connectors would change shell short-circuit semantics.
            return None;
        }
        parsed.segments
    };

    let mut steps = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        if segment.len() != 1 {
            return None;
        }
        let argv = &segment[0];
        if argv.is_empty()
            || argv.iter().any(|token| token.contains(['$', '`']))
            // ponytail: ambiguous escapes/comments stay manual until the parser models them.
            || (suppress_stderr && argv.iter().any(|token| token.contains(['\\', '\n', '\r', '#'])))
            || CONTEXT_OBSERVING_COMMANDS.contains(&argv[0].as_str())
            || segment_reads_stdin(argv)
            || !broker::configured_readonly_command(argv)
        {
            return None;
        }
        let program = resolve_trusted_executable(&argv[0])?;
        steps.push(ReadonlyCompoundStep {
            connector: if index == 0 {
                SegmentConnector::Seq
            } else {
                parsed.segment_connectors[index - 1]
            },
            program,
            argv: argv.clone(),
            suppress_stderr,
        });
    }
    Some(ReadonlyCompoundPlan { steps })
}

/// Runs a compound plan with bash list semantics: `&&` runs the next
/// step only when the previous executed step exited 0, `||` only when it
/// exited non-zero, `;`/newline always; the overall exit code is the
/// last executed step's code. Per-step stdout/stderr are concatenated in
/// execution order with no annotations, matching what a terminal would
/// have shown; each stream is bounded by the shared config budget and
/// carries a single `<truncated>` marker once its budget is exhausted.
/// Steps run with `cwd` as their working directory (the requesting
/// shell's directory, not this process's). Executables resolve from
/// the trusted system directories only — never the inherited `PATH` —
/// and a name absent from every trusted directory reports 127 like a
/// shell while list evaluation continues; a step ended by a signal
/// reports 128+signum. Output is drained from pipes concurrently with
/// a budget, so neither disk nor memory grows with the raw output
/// size. Timeouts and executor failures fail the whole run with the
/// same error contract as the readonly pipeline.
pub(crate) fn run_readonly_compound(
    plan: &ReadonlyCompoundPlan,
    config: &ReadonlyPipelineConfig,
    cwd: &Path,
) -> Result<ReadonlyPipelineOutput, ReadonlyPipelineError> {
    if plan.steps.is_empty() {
        return Err(error(
            "empty-plan",
            "readonly compound requires at least one step",
        ));
    }
    if !cwd.is_dir() {
        // A missing working directory is an executor-environment
        // failure, not a step result: reporting 127 would disguise it
        // as command-not-found.
        return Err(error(
            "executor-io",
            format!("working directory does not exist: {}", cwd.display()),
        ));
    }
    let deadline = Instant::now() + config.total_timeout;
    run_compound_steps(plan, config, cwd, deadline)
}

/// Directories a compound step's executable may resolve from: root-owned
/// system paths only. Resolving here instead of through the inherited
/// `PATH` closes the shadowing hole where a user-writable directory
/// earlier in `PATH` provides a fake `pwd`/`cat` that would run without
/// approval under an allowlisted name.
const TRUSTED_EXECUTABLE_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Environment keys passed through from the cosh process to a compound
/// step. Everything else is cleared: the step must not observe the
/// cosh process environment (provider credentials, transport state),
/// and it cannot observe the requesting shell's live environment
/// either — the executor runs a deterministic controlled environment,
/// not an emulation of the interactive shell.
const PASSTHROUGH_ENV_KEYS: &[&str] = &["HOME", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];

/// Commands whose result is a function of the execution context:
/// under the executor's controlled environment (`env_clear` + trusted
/// `PATH` + null stdin) they would answer differently from the
/// interactive shell — `env`/`printenv` report variables, `which`
/// resolves through the user's real `PATH`, and `tty` reports the
/// terminal attached to stdin (exit 0 on the interactive PTY, exit 1
/// on the executor's null stdin, inverting `&&`/`||` branches) — so
/// they never receive a compound grant and stay on the AskUser path.
const CONTEXT_OBSERVING_COMMANDS: &[&str] = &["env", "printenv", "which", "tty"];

/// Allowlisted filters that read stdin when no file operand is given
/// (`tr` always does — it never takes file operands). The executor
/// cannot reproduce the interactive stdin, so such segments stay on
/// the AskUser path.
const STDIN_DEFAULT_FILTERS: &[&str] = &["sort", "uniq", "cut", "fold", "expand", "unexpand"];

/// True when a segment would read from the executor's (null) stdin
/// instead of a file: a bare `-` operand anywhere (the readonly path
/// checks reject `-` before `--` but not after it), `tr` in any form,
/// or a stdin-default filter without a definite file operand. Operand
/// detection is conservative — a non-flag token directly following a
/// flag-like token might be that flag's value, so it does not count
/// and the segment falls back to AskUser (fails closed).
fn segment_reads_stdin(argv: &[String]) -> bool {
    if argv.iter().any(|token| token == "-") {
        return true;
    }
    if argv[0] == "tr" {
        return true;
    }
    if !STDIN_DEFAULT_FILTERS.contains(&argv[0].as_str()) {
        return false;
    }
    let has_file_operand = argv
        .iter()
        .enumerate()
        .skip(1)
        .any(|(index, token)| !token.starts_with('-') && !argv[index - 1].starts_with('-'));
    !has_file_operand
}

/// Resolves an allowlisted command name to a trusted system binary, or
/// `None` when no trusted directory provides it. Called at plan-build
/// time so eligibility and executability are one verdict. Plan argv
/// never carries `/` — the readonly rules match bare names only — so a
/// path-qualified program can never bypass this resolution.
pub(super) fn resolve_trusted_executable(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return None;
    }
    TRUSTED_EXECUTABLE_DIRS.iter().find_map(|dir| {
        let candidate = Path::new(dir).join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn run_compound_steps(
    plan: &ReadonlyCompoundPlan,
    config: &ReadonlyPipelineConfig,
    cwd: &Path,
    deadline: Instant,
) -> Result<ReadonlyPipelineOutput, ReadonlyPipelineError> {
    let mut final_exit_code = None;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut stdout_exhausted = false;
    let mut stderr_exhausted = false;

    for (index, step) in plan.steps.iter().enumerate() {
        if index > 0 {
            let previous_code = final_exit_code.unwrap_or(0);
            let should_run = match step.connector {
                SegmentConnector::Seq => true,
                SegmentConnector::And => previous_code == 0,
                SegmentConnector::Or => previous_code != 0,
            };
            if !should_run {
                continue;
            }
        }
        if Instant::now() >= deadline {
            return Err(error("compound-timeout", "readonly compound timed out"));
        }

        let mut command = Command::new(&step.program);
        command
            .args(&step.argv[1..])
            .current_dir(cwd)
            // Deterministic controlled environment: never the cosh
            // process env (credentials must not leak into auto-executed
            // output) and never a fake of the interactive shell env.
            .env_clear()
            .env("PATH", TRUSTED_EXECUTABLE_DIRS.join(":"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if step.suppress_stderr {
                Stdio::null()
            } else {
                Stdio::piped()
            });
        for key in PASSTHROUGH_ENV_KEYS {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // Each step leads its own process group so a deadline expiry
        // can reap the whole descendant tree, not just the direct
        // child.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // The binary vanished between plan build and spawn:
                // keep the shell-style 127 contract, paying the
                // synthetic error line into the stderr budget.
                append_bounded_text(
                    &mut stderr,
                    &format!("{}: command not found\n", step.argv[0]),
                    config,
                    &mut stderr_exhausted,
                );
                final_exit_code = Some(127);
                continue;
            }
            Err(err) => {
                return Err(error("executor-spawn", format!("{}: {err}", step.argv[0])));
            }
        };
        // Drain both pipes concurrently with the wait loop: bytes within
        // the remaining budget are kept, overflow is read and discarded,
        // so the child never blocks on a full pipe and neither disk nor
        // memory grows with the raw output size.
        let stdout_drain = drain_capture(
            child.stdout.take(),
            remaining_budget(&stdout, config, stdout_exhausted),
        );
        let stderr_drain = drain_capture(
            child.stderr.take(),
            remaining_budget(&stderr, config, stderr_exhausted),
        );
        let stage_deadline = Instant::now()
            + config
                .stage_timeout
                .min(deadline.saturating_duration_since(Instant::now()));
        let child_pid = child.id();
        let waited = wait_child_with_deadline(&mut child, stage_deadline, step.argv.join(" "));
        // Terminate the step's whole process group as soon as the
        // direct child is done — unconditionally (R9): a descendant
        // that redirected its output away from the pipes lets both
        // drains finish normally, so group cleanup must not be keyed
        // off a pending drain. The drains are then joined within the
        // deadline (write ends are gone, EOF arrives promptly).
        kill_process_group(child_pid);
        let (stdout_bytes, stdout_overflow) = join_capture(stdout_drain, stage_deadline);
        let (stderr_bytes, stderr_overflow) = join_capture(stderr_drain, stage_deadline);
        final_exit_code = waited?;

        append_step_output(
            &mut stdout,
            &stdout_bytes,
            stdout_overflow,
            config,
            &mut stdout_exhausted,
        );
        append_step_output(
            &mut stderr,
            &stderr_bytes,
            stderr_overflow,
            config,
            &mut stderr_exhausted,
        );
    }

    Ok(ReadonlyPipelineOutput {
        exit_code: final_exit_code,
        stdout,
        stderr,
    })
}

/// Remaining byte budget for one stream; zero once the stream already
/// carries its `<truncated>` marker so later steps drain-and-discard.
fn remaining_budget(aggregate: &str, config: &ReadonlyPipelineConfig, exhausted: bool) -> usize {
    if exhausted {
        0
    } else {
        config.output_limit_bytes.saturating_sub(aggregate.len())
    }
}

/// Live reader-thread tally: lets tests assert that every drain
/// reader is joined by the time a run returns, even when the pipe
/// write end is still held open by an escaped descendant.
#[cfg(test)]
pub(super) static LIVE_READER_THREADS: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

#[cfg(test)]
struct ReaderThreadTally;

#[cfg(test)]
impl ReaderThreadTally {
    fn new() -> Self {
        LIVE_READER_THREADS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

#[cfg(test)]
impl Drop for ReaderThreadTally {
    fn drop(&mut self) {
        LIVE_READER_THREADS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One stream's drain in progress: the reader thread appends into the
/// shared snapshot, so a deadline-bounded join can take whatever has
/// arrived without waiting for pipe EOF, and the cancel flag lets the
/// join reclaim a reader whose pipe never reaches EOF (a descendant
/// that escaped the process group can hold the write end open).
struct DrainCapture {
    handle: JoinHandle<()>,
    snapshot: Arc<Mutex<(Vec<u8>, bool)>>,
    cancel: Arc<AtomicBool>,
}

/// Reads a child pipe on a dedicated thread, keeping at most `budget`
/// bytes in the shared snapshot and discarding the rest, and records
/// whether the stream overflowed the budget. The read loop polls with
/// a short interval and re-checks the cancel flag between polls, so
/// the thread is always joinable within one poll interval plus one
/// read — it never blocks indefinitely on a pipe whose write end is
/// held open by an escaped descendant.
#[cfg(unix)]
fn drain_capture(
    stream: Option<impl Read + std::os::unix::io::AsRawFd + Send + 'static>,
    budget: usize,
) -> Option<DrainCapture> {
    let mut stream = stream?;
    let snapshot = Arc::new(Mutex::new((Vec::new(), false)));
    let cancel = Arc::new(AtomicBool::new(false));
    let writer = Arc::clone(&snapshot);
    let cancelled = Arc::clone(&cancel);
    let handle = std::thread::spawn(move || {
        #[cfg(test)]
        let _tally = ReaderThreadTally::new();
        let fd = stream.as_raw_fd();
        let mut buffer = [0u8; 8192];
        while !cancelled.load(Ordering::Relaxed) {
            let mut poll_fd = nix::libc::pollfd {
                fd,
                events: nix::libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { nix::libc::poll(&mut poll_fd, 1, 50) };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if ready == 0 {
                continue;
            }
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let Ok(mut state) = writer.lock() else {
                        break;
                    };
                    let room = budget.saturating_sub(state.0.len());
                    if read > room {
                        state.1 = true;
                    }
                    state.0.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
        }
    });
    Some(DrainCapture {
        handle,
        snapshot,
        cancel,
    })
}

/// Non-unix fallback without `poll`: a blocking read loop that
/// re-checks the cancel flag after every read, so a cancelled join
/// completes as soon as the current read returns (process teardown
/// closes the write ends and unblocks it).
#[cfg(not(unix))]
fn drain_capture(
    stream: Option<impl Read + Send + 'static>,
    budget: usize,
) -> Option<DrainCapture> {
    let mut stream = stream?;
    let snapshot = Arc::new(Mutex::new((Vec::new(), false)));
    let cancel = Arc::new(AtomicBool::new(false));
    let writer = Arc::clone(&snapshot);
    let cancelled = Arc::clone(&cancel);
    let handle = std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        while !cancelled.load(Ordering::Relaxed) {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let Ok(mut state) = writer.lock() else {
                        break;
                    };
                    let room = budget.saturating_sub(state.0.len());
                    if read > room {
                        state.1 = true;
                    }
                    state.0.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
        }
    });
    Some(DrainCapture {
        handle,
        snapshot,
        cancel,
    })
}

/// Joins a drain within the stage deadline and takes the final
/// capture. The step's process group is terminated before this join,
/// so EOF normally arrives promptly; if the pipe never reaches EOF
/// (an escaped descendant holds the write end open), the cancel flag
/// stops the poll loop and the join completes within one poll
/// interval plus one read — no reader thread is ever dropped
/// unjoined.
fn join_capture(capture: Option<DrainCapture>, deadline: Instant) -> (Vec<u8>, bool) {
    let Some(capture) = capture else {
        return Default::default();
    };
    while !capture.handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    capture.cancel.store(true, Ordering::Relaxed);
    let _ = capture.handle.join();
    capture
        .snapshot
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default()
}

/// Terminates a step's whole process group (the child was spawned as
/// the group leader) so descendants cannot outlive the deadline and
/// keep pipes, threads, or CPU alive across auto-executions. The
/// result is checked: ESRCH just means every group member already
/// exited, anything else is surfaced for diagnosis (the drain cancel
/// path still bounds the join even when the kill fails).
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let killed = unsafe { nix::libc::killpg(pid as nix::libc::pid_t, nix::libc::SIGKILL) };
    if killed != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(nix::libc::ESRCH) {
            tracing::warn!(pid, error = %err, "readonly compound process group kill failed");
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

/// Appends one step's captured bytes to the aggregate under the
/// remaining budget; once a step overflows, the `<truncated>` marker
/// terminates the aggregate and later steps are skipped for that
/// stream.
fn append_step_output(
    aggregate: &mut String,
    bytes: &[u8],
    overflowed: bool,
    config: &ReadonlyPipelineConfig,
    exhausted: &mut bool,
) {
    if *exhausted {
        return;
    }
    let remaining_bytes = config.output_limit_bytes.saturating_sub(aggregate.len());
    let remaining_lines = config
        .output_limit_lines
        .saturating_sub(aggregate.lines().count());
    let chunk = limit_clean_text(bytes, overflowed, remaining_bytes, remaining_lines);
    if chunk.ends_with("<truncated>") {
        *exhausted = true;
    }
    aggregate.push_str(&chunk);
}

/// Pays a synthetic executor-generated line (e.g. the 127 error) into
/// the same stream budget as real step output.
fn append_bounded_text(
    aggregate: &mut String,
    text: &str,
    config: &ReadonlyPipelineConfig,
    exhausted: &mut bool,
) {
    append_step_output(aggregate, text.as_bytes(), false, config, exhausted);
}
