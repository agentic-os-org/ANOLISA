use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use super::super::model::HealthUnavailableReason;
use super::super::runtime::job::JobDeadline;
use super::super::runtime::process::{
    child_has_exited, ensure_waitable_children, set_nonblocking, CleanupScope, CommandChild,
    ReapOwner, COMMAND_CLEANUP_BUDGET, COMMAND_POLL_INTERVAL,
};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_millis(750);
const DEFAULT_COMMAND_OUTPUT_LIMIT: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HealthCommandConfig {
    pub(crate) timeout: Duration,
    pub(crate) output_limit_bytes: usize,
}

impl Default for HealthCommandConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_COMMAND_TIMEOUT,
            output_limit_bytes: DEFAULT_COMMAND_OUTPUT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HealthCommandOutput {
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
    pub(crate) elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthCommandError {
    CommandMissing,
    PermissionDenied,
    Timeout,
    OutputLimit,
    Io,
}

impl HealthCommandError {
    pub(crate) fn unavailable_reason(&self) -> HealthUnavailableReason {
        match self {
            Self::CommandMissing => HealthUnavailableReason::CommandMissing,
            Self::PermissionDenied => HealthUnavailableReason::PermissionDenied,
            Self::Timeout => HealthUnavailableReason::Timeout,
            Self::OutputLimit | Self::Io => HealthUnavailableReason::ParseError,
        }
    }
}

struct WorkerCommandJob {
    deadline: JobDeadline,
    check_expires_ns: AtomicU64,
    stopped: AtomicBool,
}
static WORKER_JOB: OnceLock<WorkerCommandJob> = OnceLock::new();

pub(in crate::diagnostics::health) fn enter_worker_job(
    deadline: JobDeadline,
) -> Result<(), HealthCommandError> {
    let pid = unsafe { libc::getpid() };
    if unsafe { libc::getsid(0) } != pid || unsafe { libc::getpgrp() } != pid {
        return Err(HealthCommandError::Io);
    }
    WORKER_JOB
        .set(WorkerCommandJob {
            deadline,
            check_expires_ns: AtomicU64::new(deadline.expires_ns),
            stopped: AtomicBool::new(false),
        })
        .map_err(|_| HealthCommandError::Io)
}

pub(in crate::diagnostics::health) fn begin_worker_check(
    budget: Duration,
) -> Result<(), HealthCommandError> {
    let job = WORKER_JOB.get().ok_or(HealthCommandError::Io)?;
    let check = JobDeadline::after(budget).map_err(|_| HealthCommandError::Io)?;
    job.check_expires_ns.store(
        check.expires_ns.min(job.deadline.expires_ns),
        Ordering::Release,
    );
    Ok(())
}

pub(in crate::diagnostics::health) fn worker_command_stopped() -> bool {
    WORKER_JOB
        .get()
        .is_some_and(|job| job.stopped.load(Ordering::Acquire))
}

pub(in crate::diagnostics::health) fn stop_worker_job() {
    if let Some(job) = WORKER_JOB.get() {
        job.stopped.store(true, Ordering::Release);
    }
}

pub(crate) fn run_health_command(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
) -> Result<HealthCommandOutput, HealthCommandError> {
    if worker_command_stopped() {
        return Err(HealthCommandError::Timeout);
    }
    let result = run_health_command_inner(program, args, config);
    if matches!(
        result,
        Err(HealthCommandError::Timeout | HealthCommandError::Io)
    ) {
        if let Some(job) = WORKER_JOB.get() {
            job.stopped.store(true, Ordering::Release);
        }
    }
    result
}

fn run_health_command_inner(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
) -> Result<HealthCommandOutput, HealthCommandError> {
    let started = Instant::now();
    let deadline = started
        .checked_add(config.timeout)
        .ok_or(HealthCommandError::Io)?;
    let deadline = if let Some(job) = WORKER_JOB.get() {
        let check = JobDeadline {
            started_ns: job.deadline.started_ns,
            expires_ns: job
                .check_expires_ns
                .load(Ordering::Acquire)
                .min(job.deadline.expires_ns),
        };
        deadline.min(check.instant().map_err(|_| HealthCommandError::Io)?)
    } else {
        deadline
    };
    let work_deadline = deadline - COMMAND_CLEANUP_BUDGET.min(config.timeout / 4);
    check_deadline(work_deadline)?;
    let owner = ReapOwner::global().map_err(|_| HealthCommandError::Io)?;
    let permit = owner.reserve().ok_or(HealthCommandError::Io)?;
    ensure_waitable_children().map_err(|_| HealthCommandError::Io)?;
    check_deadline(work_deadline)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if WORKER_JOB.get().is_none() {
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut raw_child = command.spawn().map_err(classify_spawn_error)?;
    let stdout = raw_child.stdout.take();
    let stderr = raw_child.stderr.take();
    let scope = if WORKER_JOB.get().is_some() {
        CleanupScope::DirectChild
    } else {
        CleanupScope::OwnedGroup
    };
    let mut child = permit.register(raw_child, scope, deadline);
    let mut stdout = stdout.ok_or(HealthCommandError::Io)?;
    let mut stderr = stderr.ok_or(HealthCommandError::Io)?;
    set_nonblocking(stdout.as_raw_fd()).map_err(|_| HealthCommandError::Io)?;
    set_nonblocking(stderr.as_raw_fd()).map_err(|_| HealthCommandError::Io)?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    loop {
        read_available(
            &mut stdout,
            &mut stdout_bytes,
            config.output_limit_bytes,
            work_deadline,
        )?;
        read_available(
            &mut stderr,
            &mut stderr_bytes,
            config.output_limit_bytes,
            work_deadline,
        )?;
        let exited = child.has_exited().map_err(|_| HealthCommandError::Io)?;
        if exited {
            let status = child
                .cleanup()
                .map_err(|_| HealthCommandError::Io)?
                .ok_or(HealthCommandError::Timeout)?;
            read_available(
                &mut stdout,
                &mut stdout_bytes,
                config.output_limit_bytes,
                deadline,
            )?;
            read_available(
                &mut stderr,
                &mut stderr_bytes,
                config.output_limit_bytes,
                deadline,
            )?;
            return Ok(HealthCommandOutput {
                exit_code: status.code(),
                stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                stdout_truncated: false,
                stderr_truncated: false,
                elapsed_ms: started.elapsed().as_millis(),
            });
        }
        let remaining = work_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HealthCommandError::Timeout);
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(remaining));
    }
}

fn classify_spawn_error(error: io::Error) -> HealthCommandError {
    match error.kind() {
        io::ErrorKind::NotFound => HealthCommandError::CommandMissing,
        io::ErrorKind::PermissionDenied => HealthCommandError::PermissionDenied,
        _ => HealthCommandError::Io,
    }
}

fn check_deadline(deadline: Instant) -> Result<(), HealthCommandError> {
    if Instant::now() >= deadline {
        Err(HealthCommandError::Timeout)
    } else {
        Ok(())
    }
}

fn read_available(
    stream: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    deadline: Instant,
) -> Result<(), HealthCommandError> {
    let mut buffer = [0; 4096];
    loop {
        check_deadline(deadline)?;
        let capacity = limit.saturating_sub(bytes.len());
        let read_limit = capacity.saturating_add(1).min(buffer.len());
        match stream.read(&mut buffer[..read_limit]) {
            Ok(0) => break,
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count.min(capacity)]);
                if count > capacity {
                    return Err(HealthCommandError::OutputLimit);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(HealthCommandError::Io),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
