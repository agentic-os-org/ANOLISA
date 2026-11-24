//! Bounded byte transport for one hook command, independent of Agent and Provider protocols.
//! Callers retain scheduling, policy decisions and interpretation of native responses.

pub mod uds;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::PathBuf,
    process::ExitStatus,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod package_lock;
mod packages;
#[cfg(target_os = "linux")]
pub use package_lock::PackageLock;

pub use packages::{LeaseError, PackageLeases};

/// Explicit process context; arguments are literal and the environment is not inherited.
///
/// Use an explicit shell executable and arguments for shell syntax. Program lookup
/// follows `std::process::Command`; callers can supply an absolute executable path.
#[derive(Clone, Debug)]
pub struct CommandSpec {
    /// Executable to launch with the caller's privileges.
    pub program: PathBuf,
    /// Literal arguments, without an implicit shell or template expansion.
    pub args: Vec<OsString>,
    /// Working directory of the command.
    pub cwd: PathBuf,
    /// Complete child environment, including any PATH or credentials it needs.
    pub environment: BTreeMap<OsString, OsString>,
}

/// Independent byte ceilings; zero permits only an empty stream.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum input accepted before spawning a process.
    pub input_bytes: usize,
    /// Maximum captured stdout; exceeding it fails without returning partial output.
    pub stdout_bytes: usize,
    /// Maximum captured stderr; exceeding it fails without returning partial output.
    pub stderr_bytes: usize,
}

/// Output channel whose byte ceiling was exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Command response bytes.
    Stdout,
    /// Command diagnostic bytes.
    Stderr,
    /// Dedicated command result, separate from diagnostics.
    Result,
}

/// Native result, available only after verifying owned process-group cleanup.
#[derive(Debug)]
pub struct Output {
    /// Native status, preserving nonzero exits and signal termination separately.
    pub status: ExitStatus,
    /// Exact stdout bytes; no JSON interpretation, decoding or newline conversion.
    pub stdout: Vec<u8>,
    /// Exact stderr bytes, separate from the response and not automatically logged.
    pub stderr: Vec<u8>,
    /// Bytes written to the stdin pipe, not proof that the command consumed them.
    /// A command may close stdin early and still produce a valid native result.
    pub input_bytes_written: usize,
}

/// Transport or cleanup failure, distinct from a command's native exit status.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The caller cancelled before completion.
    #[error("command execution cancelled")]
    Cancelled,
    /// The absolute execution deadline elapsed.
    #[error("command execution deadline exceeded")]
    DeadlineExceeded,
    /// Input exceeded the caller's limit before any process was started.
    #[error("command input has {actual} bytes, exceeding limit {limit}")]
    InputLimit {
        /// Allowed input bytes.
        limit: usize,
        /// Supplied input bytes.
        actual: usize,
    },
    /// A captured stream exceeded its limit; the owned group was stopped.
    #[error("command {stream:?} exceeded byte limit {limit}")]
    OutputLimit {
        /// Offending output channel.
        stream: Stream,
        /// Allowed bytes for that channel.
        limit: usize,
    },
    /// Process creation or pipe operations failed.
    #[error("command {operation} failed: {source}")]
    Io {
        /// Operation that failed, without command arguments or payload data.
        operation: &'static str,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// Cleanup could not be verified; this takes precedence over execution results.
    #[error("command group {pid} cleanup could not be verified: {source}")]
    Cleanup {
        /// Original process-group leader, for diagnostics only. Its identity may
        /// already have been released; callers must not signal this numeric PID.
        pid: u32,
        /// Cleanup error or exhausted verification budget.
        #[source]
        source: io::Error,
    },
    /// Execution is implemented only on Linux.
    #[error("bounded command execution requires Linux")]
    UnsupportedPlatform,
}

const CHILD_TERMINATE_GRACE: Duration = Duration::from_millis(250);
const CHILD_CLEANUP_GRACE: Duration = Duration::from_secs(1);

/// Maximum cooperative termination and group verification budget.
pub const COOPERATIVE_CHILD_CLEANUP_GRACE: Duration =
    CHILD_TERMINATE_GRACE.saturating_add(CHILD_CLEANUP_GRACE);

/// Checks the shared execution limit before doing work or accepting a result.
///
/// # Errors
/// Returns cancellation first, then deadline expiry.
pub fn check_active(deadline: Instant, cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Error::DeadlineExceeded)
    } else {
        Ok(())
    }
}

/// Run one command with nonblocking pipes and a caller-owned absolute deadline.
///
/// The deadline covers spawning and all three pipes; cleanup has a separate,
/// fixed one-second budget. Cancellation is polled during execution. Every
/// spawned command gets its own process group. Successful results require the
/// group to be killed and checked for live members before its leader is reaped.
/// Failed cleanup reports an error and Drop attempts a nonblocking final reap.
/// Output requires EOF on both streams; an inherited pipe held by a descendant
/// can therefore exhaust the deadline even after the leader exits.
///
/// This synchronous call creates no helper threads or global signal handlers.
/// Callers may invoke it concurrently or sequentially to preserve native hook
/// scheduling. They must leave child reaping to this function (no competing
/// wait or automatic SIGCHLD reaping). This is process hygiene, not a sandbox:
/// descendants can escape the group, and kernel-blocked calls are not hard
/// realtime bounded. Linux kills the immediate child if its owning thread dies;
/// a forcibly terminated caller still cannot verify or clean up descendants.
/// Privilege-changing executables can clear that parent-death signal.
///
/// # Errors
/// Returns typed errors for cancellation, deadline, stream limits, spawn/pipe
/// failures, unverifiable cleanup and unsupported platforms. Nonzero native
/// exits are successful transport results and remain for the host to interpret.
pub fn run(
    command: &CommandSpec,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Output, Error> {
    #[cfg(target_os = "linux")]
    {
        linux::run(
            command,
            input,
            limits,
            deadline,
            cancelled,
            CleanupMode::Immediate,
            None,
        )
        .map(|(output, _)| output)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, input, limits, deadline, cancelled);
        Err(Error::UnsupportedPlatform)
    }
}

/// Run a cooperative wrapper, notifying it before forced cleanup on transport failure.
///
/// Shares [`run`]'s byte, deadline and process-group guarantees. On failure, sends
/// SIGTERM and allows up to 250 ms for the wrapper to cancel its nested commands,
/// then performs the same forced group cleanup with its separate one-second budget.
/// The wrapper must install a SIGTERM handler before spawning nested commands and
/// use [`run`] for those commands so their cancellation does not add another grace.
/// Successful exchanges keep immediate cleanup. Uncooperative wrappers and escaped
/// descendants retain [`run`]'s limitations; this does not create a sandbox.
///
/// # Errors
/// Returns the same errors as [`run`], including unverifiable termination/cleanup.
pub fn run_cooperative(
    command: &CommandSpec,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Output, Error> {
    #[cfg(target_os = "linux")]
    {
        linux::run(
            command,
            input,
            limits,
            deadline,
            cancelled,
            CleanupMode::Cooperative,
            None,
        )
        .map(|(output, _)| output)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, input, limits, deadline, cancelled);
        Err(Error::UnsupportedPlatform)
    }
}

/// Process-group cleanup after a transport failure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CleanupMode {
    /// Stop the group immediately, as in [`run`].
    Immediate,
    /// Allow the wrapper to cancel its children first, as in [`run_cooperative`].
    Cooperative,
}

/// Capture a dedicated result FIFO alongside stdin, stdout and stderr.
///
/// The caller supplies a fresh path in its private temporary directory, passes
/// that path to the writer, and removes the directory after the call. The FIFO
/// is created before spawn. The result and stdout each get `limits.stdout_bytes`.
///
/// # Errors
/// Returns the same errors as [`run`], including [`Stream::Result`] overflow.
pub fn run_with_result_pipe(
    command: &CommandSpec,
    input: &[u8],
    limits: Limits,
    deadline: Instant,
    cancelled: &AtomicBool,
    cleanup: CleanupMode,
    path: &std::path::Path,
) -> Result<(Output, Vec<u8>), Error> {
    #[cfg(target_os = "linux")]
    {
        linux::run(
            command,
            input,
            limits,
            deadline,
            cancelled,
            cleanup,
            Some(path),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, input, limits, deadline, cancelled, cleanup, path);
        Err(Error::UnsupportedPlatform)
    }
}

/// Run an Agent with inherited terminal streams and exclusive process-group ownership.
///
/// The session lasts until the child exits or the caller supplies a termination
/// signal. Pending signals are forwarded to the owned group; termination then
/// has a two-second grace period. The foreground terminal is restored and owned
/// descendants are stopped before returning the original child exit status.
/// The caller must serialize foreground sessions and own signal-handler setup.
/// This is foreground execution, not a shell job-control implementation.
///
/// # Errors
/// Rejects spawn, terminal-transfer, child-wait and bounded cleanup failures.
pub fn run_foreground(
    command: &CommandSpec,
    signal: &std::sync::atomic::AtomicI32,
) -> Result<ExitStatus, Error> {
    run_foreground_observed(command, signal, |_| {})
}

/// Run a foreground Agent and report its owned leader PID after terminal handoff.
///
/// `started` runs once on the caller's thread after successful spawn and terminal
/// transfer, before waiting for exit. The PID is also the owned process-group ID.
/// The callback must return promptly and must not reap the child; the executor
/// retains exclusive cleanup ownership. Spawn/transfer failure does not call it.
/// See [`run_foreground`] for signal, terminal and lifetime guarantees.
///
/// # Errors
/// Returns the same execution and cleanup errors as [`run_foreground`].
pub fn run_foreground_observed(
    command: &CommandSpec,
    signal: &std::sync::atomic::AtomicI32,
    started: impl FnOnce(u32),
) -> Result<ExitStatus, Error> {
    run_foreground_with_grace(
        command,
        signal,
        &AtomicBool::new(false),
        Duration::from_secs(2),
        started,
    )
}

/// Run a foreground Agent with a bounded cooperative shutdown grace.
///
/// The first forwarded `signal` starts `shutdown_grace`; later signals never
/// extend it. Setting `failed` requests SIGTERM with at most two seconds left,
/// independently of the configured grace. Failure can shorten but never extend
/// an existing deadline. The caller retains ownership of both signal sources.
/// Leader exit still triggers immediate cleanup of owned descendants.
/// See [`run_foreground_observed`] for terminal and callback guarantees.
///
/// # Errors
/// Rejects grace outside 1 ms..=120 s before spawning, and returns the execution
/// and cleanup errors described in [`run_foreground`].
pub fn run_foreground_with_grace(
    command: &CommandSpec,
    signal: &std::sync::atomic::AtomicI32,
    failed: &AtomicBool,
    shutdown_grace: Duration,
    started: impl FnOnce(u32),
) -> Result<ExitStatus, Error> {
    run_foreground_mode(command, signal, failed, shutdown_grace, false, started)
}

/// Run a worker that owns the graceful shutdown of its subprocesses.
///
/// External signals reach only the unreaped worker PID so its SDK transports
/// remain available during cleanup. Internal failure still signals the group;
/// deadline expiry and worker exit still force cleanup of the whole group.
/// Terminal-generated signals continue to follow the kernel foreground group.
/// All other guarantees match [`run_foreground_with_grace`].
///
/// # Errors
/// Returns the same validation and cleanup errors as [`run_foreground_with_grace`].
pub fn run_foreground_worker_with_grace(
    command: &CommandSpec,
    signal: &std::sync::atomic::AtomicI32,
    failed: &AtomicBool,
    shutdown_grace: Duration,
    started: impl FnOnce(u32),
) -> Result<ExitStatus, Error> {
    run_foreground_mode(command, signal, failed, shutdown_grace, true, started)
}

fn run_foreground_mode(
    command: &CommandSpec,
    signal: &std::sync::atomic::AtomicI32,
    failed: &AtomicBool,
    shutdown_grace: Duration,
    leader_signals: bool,
    started: impl FnOnce(u32),
) -> Result<ExitStatus, Error> {
    if shutdown_grace < Duration::from_millis(1) || shutdown_grace > Duration::from_secs(120) {
        return Err(Error::Io {
            operation: "foreground shutdown grace",
            source: io::Error::new(io::ErrorKind::InvalidInput, "must be within 1..120000 ms"),
        });
    }
    #[cfg(target_os = "linux")]
    {
        linux::foreground::run(
            command,
            signal,
            failed,
            shutdown_grace,
            leader_signals,
            started,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            command,
            signal,
            failed,
            shutdown_grace,
            leader_signals,
            started,
        );
        Err(Error::UnsupportedPlatform)
    }
}
