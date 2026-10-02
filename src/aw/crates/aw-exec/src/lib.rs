//! Bounded byte transport for one hook command, independent of Agent and Provider protocols.
//! Callers retain scheduling, policy decisions and interpretation of native responses.

use std::{
    collections::BTreeMap, ffi::OsString, io, path::PathBuf, process::ExitStatus,
    sync::atomic::AtomicBool, time::Instant,
};

#[cfg(target_os = "linux")]
mod linux;

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
/// realtime bounded. A forcibly terminated caller cannot perform cleanup.
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
        linux::run(command, input, limits, deadline, cancelled)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, input, limits, deadline, cancelled);
        Err(Error::UnsupportedPlatform)
    }
}
