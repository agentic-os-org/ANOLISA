use std::collections::VecDeque;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
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
const DEFAULT_RAW_READ_LIMIT_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_MAX_LINE_BYTES: usize = 4096;
const DEFAULT_MAX_RECORDS: usize = 200;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HealthLineFilter {
    pub(crate) max_records: usize,
    pub(crate) max_line_bytes: usize,
    pub(crate) retained_bytes: usize,
    pub(crate) raw_read_limit_bytes: u64,
    pub(crate) newest_first: bool,
}

impl Default for HealthLineFilter {
    fn default() -> Self {
        Self {
            max_records: DEFAULT_MAX_RECORDS,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            retained_bytes: DEFAULT_COMMAND_OUTPUT_LIMIT,
            raw_read_limit_bytes: DEFAULT_RAW_READ_LIMIT_BYTES,
            newest_first: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HealthFilteredLines {
    pub(crate) lines: Vec<String>,
    pub(crate) raw_bytes_read: u64,
    pub(crate) lines_scanned: u64,
    pub(crate) truncated_lines: u64,
    pub(crate) complete: bool,
}

/// Unrelated log volume consumes the read budget, never the retained evidence budget.
pub(crate) struct HealthLineFilterState {
    filter: HealthLineFilter,
    retained: VecDeque<String>,
    retained_bytes: usize,
    pending: Vec<u8>,
    pending_truncated: bool,
    raw_bytes_read: u64,
    lines_scanned: u64,
    truncated_lines: u64,
    exhausted: bool,
    eof: bool,
}

impl HealthLineFilterState {
    pub(crate) fn new(filter: HealthLineFilter) -> Self {
        Self {
            filter,
            retained: VecDeque::new(),
            retained_bytes: 0,
            pending: Vec::new(),
            pending_truncated: false,
            raw_bytes_read: 0,
            lines_scanned: 0,
            truncated_lines: 0,
            exhausted: false,
            eof: false,
        }
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(crate) fn mark_eof(&mut self, matches: &impl Fn(&str) -> bool) {
        self.eof = true;
        let trailing = std::mem::take(&mut self.pending);
        if !trailing.is_empty() {
            let truncated = std::mem::replace(&mut self.pending_truncated, false);
            self.offer_line(&trailing, truncated, matches);
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8], matches: &impl Fn(&str) -> bool) {
        if self.exhausted {
            return;
        }
        let allowance = self
            .filter
            .raw_read_limit_bytes
            .saturating_sub(self.raw_bytes_read);
        let accepted = (chunk.len() as u64).min(allowance) as usize;
        self.raw_bytes_read += accepted as u64;
        if accepted < chunk.len() {
            self.exhausted = true;
        }
        let mut start = 0;
        while start < accepted {
            match chunk[start..accepted]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                Some(offset) => {
                    self.pending
                        .extend_from_slice(&chunk[start..start + offset]);
                    let line = std::mem::take(&mut self.pending);
                    let truncated = std::mem::replace(&mut self.pending_truncated, false);
                    self.offer_line(&line, truncated, matches);
                    start += offset + 1;
                }
                None => {
                    self.pending.extend_from_slice(&chunk[start..accepted]);
                    break;
                }
            }
        }
        if self.pending.len() > self.filter.max_line_bytes {
            self.pending.truncate(self.filter.max_line_bytes);
            if !self.pending_truncated {
                self.pending_truncated = true;
                self.truncated_lines += 1;
            }
        }
    }

    fn offer_line(
        &mut self,
        line: &[u8],
        already_truncated: bool,
        matches: &impl Fn(&str) -> bool,
    ) {
        self.lines_scanned += 1;
        let mut text = String::from_utf8_lossy(line).into_owned();
        if truncate_at_char_boundary(&mut text, self.filter.max_line_bytes) && !already_truncated {
            self.truncated_lines += 1;
        }
        if !matches(&text) {
            return;
        }
        if self.filter.newest_first
            && (self.retained.len() >= self.filter.max_records
                || self.retained_bytes + text.len() > self.filter.retained_bytes)
        {
            // A newest-first stream has already delivered the most recent evidence.
            self.exhausted = true;
            return;
        }
        while self.retained.len() >= self.filter.max_records
            || self.retained_bytes + text.len() > self.filter.retained_bytes
        {
            match self.retained.pop_front() {
                Some(dropped) => self.retained_bytes -= dropped.len(),
                None => break,
            }
        }
        if text.len() <= self.filter.retained_bytes {
            self.retained_bytes += text.len();
            self.retained.push_back(text);
        }
    }

    pub(crate) fn finish(self) -> HealthFilteredLines {
        HealthFilteredLines {
            lines: self.retained.into_iter().collect(),
            raw_bytes_read: self.raw_bytes_read,
            lines_scanned: self.lines_scanned,
            truncated_lines: self.truncated_lines,
            complete: self.eof && !self.exhausted,
        }
    }
}

fn truncate_at_char_boundary(text: &mut String, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HealthFilteredCommandOutput {
    pub(crate) filtered: HealthFilteredLines,
    pub(crate) stderr: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) elapsed_ms: u128,
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
        stop_worker_after_overrun();
    }
    result
}

struct SpawnedHealthCommand {
    child: CommandChild<'static>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    started: Instant,
    deadline: Instant,
    work_deadline: Instant,
}

fn spawn_health_command(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
) -> Result<SpawnedHealthCommand, HealthCommandError> {
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
    let child = permit.register(raw_child, scope, deadline);
    let stdout = stdout.ok_or(HealthCommandError::Io)?;
    let stderr = stderr.ok_or(HealthCommandError::Io)?;
    set_nonblocking(stdout.as_raw_fd()).map_err(|_| HealthCommandError::Io)?;
    set_nonblocking(stderr.as_raw_fd()).map_err(|_| HealthCommandError::Io)?;
    Ok(SpawnedHealthCommand {
        child,
        stdout,
        stderr,
        started,
        deadline,
        work_deadline,
    })
}

fn stop_worker_after_overrun() {
    if let Some(job) = WORKER_JOB.get() {
        job.stopped.store(true, Ordering::Release);
    }
}

fn run_health_command_inner(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
) -> Result<HealthCommandOutput, HealthCommandError> {
    let mut spawned = spawn_health_command(program, args, config)?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    loop {
        read_available(
            &mut spawned.stdout,
            &mut stdout_bytes,
            config.output_limit_bytes,
            spawned.work_deadline,
        )?;
        read_available(
            &mut spawned.stderr,
            &mut stderr_bytes,
            config.output_limit_bytes,
            spawned.work_deadline,
        )?;
        let exited = spawned
            .child
            .has_exited()
            .map_err(|_| HealthCommandError::Io)?;
        if exited {
            let status = spawned
                .child
                .cleanup()
                .map_err(|_| HealthCommandError::Io)?
                .ok_or(HealthCommandError::Timeout)?;
            read_available(
                &mut spawned.stdout,
                &mut stdout_bytes,
                config.output_limit_bytes,
                spawned.deadline,
            )?;
            read_available(
                &mut spawned.stderr,
                &mut stderr_bytes,
                config.output_limit_bytes,
                spawned.deadline,
            )?;
            return Ok(HealthCommandOutput {
                exit_code: status.code(),
                stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                stdout_truncated: false,
                stderr_truncated: false,
                elapsed_ms: spawned.started.elapsed().as_millis(),
            });
        }
        let remaining = spawned
            .work_deadline
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HealthCommandError::Timeout);
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(remaining));
    }
}

pub(crate) fn run_health_command_filtered(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
    filter: HealthLineFilter,
    matches: impl Fn(&str) -> bool,
) -> Result<HealthFilteredCommandOutput, HealthCommandError> {
    if worker_command_stopped() {
        return Err(HealthCommandError::Timeout);
    }
    let result = run_health_command_filtered_inner(program, args, config, filter, &matches);
    if matches!(
        result,
        Err(HealthCommandError::Timeout | HealthCommandError::Io)
    ) {
        stop_worker_after_overrun();
    }
    result
}

fn run_health_command_filtered_inner(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
    filter: HealthLineFilter,
    matches: &impl Fn(&str) -> bool,
) -> Result<HealthFilteredCommandOutput, HealthCommandError> {
    let mut spawned = spawn_health_command(program, args, config)?;
    let mut state = HealthLineFilterState::new(filter);
    let mut stderr_bytes = Vec::new();
    let mut buffer = [0; 4096];
    let mut stdout_open = true;
    let mut exit_code = None;
    loop {
        while stdout_open && !state.exhausted() && Instant::now() < spawned.work_deadline {
            match read_chunk(&mut spawned.stdout, &mut buffer)? {
                StreamChunk::Bytes(count) => state.push(&buffer[..count], matches),
                StreamChunk::Closed => {
                    stdout_open = false;
                    state.mark_eof(matches);
                }
                StreamChunk::Pending => break,
            }
        }
        drain_stderr(
            &mut spawned.stderr,
            &mut buffer,
            &mut stderr_bytes,
            config.output_limit_bytes,
        )?;
        if state.exhausted() {
            break;
        }
        if spawned
            .child
            .has_exited()
            .map_err(|_| HealthCommandError::Io)?
        {
            exit_code = spawned
                .child
                .cleanup()
                .map_err(|_| HealthCommandError::Io)?
                .and_then(|status| status.code());
            while stdout_open && !state.exhausted() && Instant::now() < spawned.deadline {
                match read_chunk(&mut spawned.stdout, &mut buffer)? {
                    StreamChunk::Bytes(count) => state.push(&buffer[..count], matches),
                    StreamChunk::Closed => {
                        stdout_open = false;
                        state.mark_eof(matches);
                    }
                    StreamChunk::Pending => break,
                }
            }
            drain_stderr(
                &mut spawned.stderr,
                &mut buffer,
                &mut stderr_bytes,
                config.output_limit_bytes,
            )?;
            break;
        }
        let remaining = spawned
            .work_deadline
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // A single check budget cut must not cancel the remaining worker checks.
            break;
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(remaining));
    }
    Ok(HealthFilteredCommandOutput {
        filtered: state.finish(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        exit_code,
        elapsed_ms: spawned.started.elapsed().as_millis(),
    })
}

enum StreamChunk {
    Bytes(usize),
    Pending,
    Closed,
}

fn read_chunk(
    stream: &mut impl Read,
    buffer: &mut [u8],
) -> Result<StreamChunk, HealthCommandError> {
    loop {
        match stream.read(buffer) {
            Ok(0) => return Ok(StreamChunk::Closed),
            Ok(count) => return Ok(StreamChunk::Bytes(count)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(StreamChunk::Pending)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(HealthCommandError::Io),
        }
    }
}

fn drain_stderr(
    stream: &mut impl Read,
    buffer: &mut [u8],
    sink: &mut Vec<u8>,
    limit: usize,
) -> Result<(), HealthCommandError> {
    while sink.len() < limit {
        match read_chunk(stream, buffer)? {
            StreamChunk::Bytes(count) => {
                let capacity = limit - sink.len();
                sink.extend_from_slice(&buffer[..count.min(capacity)]);
            }
            _ => return Ok(()),
        }
    }
    Ok(())
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
