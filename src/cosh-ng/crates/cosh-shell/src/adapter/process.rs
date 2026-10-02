use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use crate::types::{AgentEvent, PROVIDER_TIMEOUT_ERROR_CODE};

use super::{
    AdapterError, PreparedInvocation, ProviderCancellationArtifact,
    ProviderCancellationArtifactKind, ProviderCancellationArtifactStore,
};

mod driver;
mod watchdog;

pub(crate) use driver::{
    start_cancellable_provider_process, start_control_protocol_provider_process,
    ProviderDriverSpec, ProviderStreamParser,
};
use watchdog::{
    AgentProcessTimeout, AgentProcessTimeoutKind, AgentProcessTimeouts, AgentProcessWatchdog,
};

#[derive(Debug)]
enum ProviderIoEvent {
    Line(String),
    Closed,
    ReadError(String),
}

#[derive(Debug)]
pub(crate) enum ProviderRunOutcome {
    Exited {
        status: ExitStatus,
        stderr_tail: String,
    },
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderLineProgress {
    NoProgress,
    Progress,
    AwaitingApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderStdinMode {
    Null,
    Piped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderPromptArgMode {
    None,
    TrailingArgIfNonEmpty,
    QwenPromptFlag,
}

pub(crate) fn spawn_provider_child(
    prepared: &PreparedInvocation,
    provider_label: &'static str,
    stdin_mode: ProviderStdinMode,
    prompt_mode: ProviderPromptArgMode,
) -> Result<Child, AdapterError> {
    const MAX_SPAWN_ATTEMPTS: usize = 3;

    for attempt in 0..MAX_SPAWN_ATTEMPTS {
        let mut command = Command::new(&prepared.program);
        command.args(&prepared.args);
        match prompt_mode {
            ProviderPromptArgMode::None => {}
            ProviderPromptArgMode::TrailingArgIfNonEmpty => {
                if !prepared.prompt.is_empty() {
                    command.arg(&prepared.prompt);
                }
            }
            ProviderPromptArgMode::QwenPromptFlag => {
                command.arg("--prompt").arg(&prepared.prompt);
            }
        }
        match stdin_mode {
            ProviderStdinMode::Null => {
                command.stdin(Stdio::null());
            }
            ProviderStdinMode::Piped => {
                command.stdin(Stdio::piped());
            }
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        match command.spawn() {
            Err(error)
                if spawn_error_is_text_file_busy(&error) && attempt + 1 < MAX_SPAWN_ATTEMPTS =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            result => {
                return result.map_err(|err| AdapterError {
                    message: format!("failed to run {provider_label}: {err}"),
                });
            }
        }
    }

    // The last attempt cannot enter the retry branch.
    unreachable!("the bounded provider spawn loop always returns on its final attempt")
}

fn spawn_error_is_text_file_busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Bounded, thread-safe capture of a child's trailing stderr bytes.
///
/// Shared with the background compactor so every subprocess owner reuses the
/// same tail-retention semantics instead of growing stderr without bound.
#[derive(Clone, Debug)]
pub(crate) struct StderrTail {
    inner: Arc<Mutex<Vec<u8>>>,
    limit: usize,
}

impl StderrTail {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            limit,
        }
    }

    pub(crate) fn push(&self, bytes: &[u8]) {
        let Ok(mut tail) = self.inner.lock() else {
            return;
        };
        tail.extend_from_slice(bytes);
        if tail.len() > self.limit {
            let excess = tail.len() - self.limit;
            tail.drain(0..excess);
        }
    }

    pub(crate) fn snapshot(&self) -> String {
        self.inner
            .lock()
            .map(|tail| String::from_utf8_lossy(&tail).to_string())
            .unwrap_or_default()
    }

    /// Spawns a detached drain thread that owns `stderr` until EOF.
    ///
    /// Draining continuously (instead of reading after exit) keeps the pipe
    /// from filling up and deadlocking a chatty child, while retention stays
    /// bounded to the tail limit.
    pub(crate) fn drain_in_background(&self, mut stderr: impl Read + Send + 'static) {
        let tail = self.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => tail.push(&buffer[..read]),
                }
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_provider_process_loop(
    run_id: String,
    provider_label: &'static str,
    child: &mut Child,
    child_pid: Arc<Mutex<Option<u32>>>,
    cancelled: Arc<AtomicBool>,
    cancellation_artifacts: ProviderCancellationArtifactStore,
    sender: &mpsc::Sender<Result<AgentEvent, AdapterError>>,
    mut on_stdout_line: impl FnMut(String) -> Result<ProviderLineProgress, AdapterError>,
    mut on_idle: impl FnMut() -> Result<Vec<AgentEvent>, AdapterError>,
) -> ProviderRunOutcome {
    let timeouts = AgentProcessTimeouts::from_env();
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_and_reap_process(child);
            clear_child_pid(&child_pid);
            let _ = sender.send(Err(AdapterError {
                message: format!("failed to capture {provider_label} stdout"),
            }));
            return ProviderRunOutcome::Failed;
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_and_reap_process(child);
            clear_child_pid(&child_pid);
            let _ = sender.send(Err(AdapterError {
                message: format!("failed to capture {provider_label} stderr"),
            }));
            return ProviderRunOutcome::Failed;
        }
    };

    let (stdout_rx, stdout_reader) = spawn_stdout_reader(stdout, provider_label);
    let (stderr_tail, stderr_reader) = spawn_stderr_tail_reader(stderr, timeouts.stderr_tail_bytes);
    let mut watchdog = AgentProcessWatchdog::new(timeouts, Instant::now());
    let mut cancel_started_at = None::<Instant>;
    let mut cancel_killed = false;

    loop {
        let now = Instant::now();
        if cancelled.load(Ordering::SeqCst) {
            if let Some(started_at) = cancel_started_at {
                if !cancel_killed
                    && now.saturating_duration_since(started_at) >= timeouts.cancel_grace
                {
                    kill_process_group(child.id());
                    cancel_killed = true;
                }
            } else {
                terminate_process_group(child.id());
                cancel_started_at = Some(now);
            }
        } else if let Some(timeout) = watchdog.timeout(now) {
            terminate_and_reap_process(child);
            clear_child_pid(&child_pid);
            join_provider_readers(stdout_reader, stderr_reader);
            let _ = sender.send(Ok(AgentEvent::AgentFailed {
                run_id,
                error: timeout_failure_message(provider_label, timeout, &stderr_tail.snapshot()),
                error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                max_turns: None,
            }));
            return ProviderRunOutcome::Failed;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                terminate_and_reap_process(child);
                clear_child_pid(&child_pid);
                if cancelled.load(Ordering::SeqCst) {
                    drain_cancellation_stdout_artifacts(
                        &stdout_rx,
                        &cancellation_artifacts,
                        provider_label,
                        &run_id,
                    );
                    record_cancellation_stderr_tail(
                        &cancellation_artifacts,
                        provider_label,
                        &run_id,
                        &stderr_tail.snapshot(),
                    );
                    join_provider_readers(stdout_reader, stderr_reader);
                    let _ = sender.send(Ok(AgentEvent::AgentCancelled {
                        run_id,
                        reason: "user requested cancellation".to_string(),
                    }));
                    return ProviderRunOutcome::Cancelled;
                }
                if let Err(err) =
                    drain_pending_stdout_lines(&stdout_rx, &mut on_stdout_line, sender)
                {
                    join_provider_readers(stdout_reader, stderr_reader);
                    let _ = sender.send(Err(err));
                    return ProviderRunOutcome::Failed;
                }
                join_provider_readers(stdout_reader, stderr_reader);
                return ProviderRunOutcome::Exited {
                    status,
                    stderr_tail: stderr_tail.snapshot(),
                };
            }
            Ok(None) => {}
            Err(err) => {
                terminate_and_reap_process(child);
                clear_child_pid(&child_pid);
                join_provider_readers(stdout_reader, stderr_reader);
                let _ = sender.send(Err(AdapterError {
                    message: format!("failed to poll {provider_label}: {err}"),
                }));
                return ProviderRunOutcome::Failed;
            }
        }

        match stdout_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ProviderIoEvent::Line(line)) => match on_stdout_line(line.clone()) {
                Ok(ProviderLineProgress::Progress) => {
                    if cancelled.load(Ordering::SeqCst) {
                        record_cancellation_stdout_line(
                            &cancellation_artifacts,
                            provider_label,
                            &run_id,
                            &line,
                        );
                    }
                    watchdog.record_stdout(Instant::now());
                }
                Ok(ProviderLineProgress::AwaitingApproval) => {
                    if cancelled.load(Ordering::SeqCst) {
                        record_cancellation_stdout_line(
                            &cancellation_artifacts,
                            provider_label,
                            &run_id,
                            &line,
                        );
                    }
                    watchdog.record_approval_wait(Instant::now());
                }
                Ok(ProviderLineProgress::NoProgress) => {
                    if cancelled.load(Ordering::SeqCst) {
                        record_cancellation_stdout_line(
                            &cancellation_artifacts,
                            provider_label,
                            &run_id,
                            &line,
                        );
                    }
                }
                Err(err) => {
                    let _ = sender.send(Err(err));
                    terminate_and_reap_process(child);
                    clear_child_pid(&child_pid);
                    join_provider_readers(stdout_reader, stderr_reader);
                    return ProviderRunOutcome::Failed;
                }
            },
            Ok(ProviderIoEvent::Closed) => {}
            Ok(ProviderIoEvent::ReadError(message)) => {
                let _ = sender.send(Err(AdapterError { message }));
                terminate_and_reap_process(child);
                clear_child_pid(&child_pid);
                join_provider_readers(stdout_reader, stderr_reader);
                return ProviderRunOutcome::Failed;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match on_idle() {
                Ok(events) => {
                    for event in events {
                        let _ = sender.send(Ok(event));
                    }
                }
                Err(err) => {
                    let _ = sender.send(Err(err));
                    terminate_and_reap_process(child);
                    clear_child_pid(&child_pid);
                    join_provider_readers(stdout_reader, stderr_reader);
                    return ProviderRunOutcome::Failed;
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
}

pub(crate) fn record_cancellation_pending_session(
    store: &ProviderCancellationArtifactStore,
    provider_label: &'static str,
    run_id: &str,
    pending_session_id: Option<String>,
) {
    let Some(text) = pending_session_id else {
        return;
    };
    store.push(ProviderCancellationArtifact {
        provider: provider_label,
        run_id: run_id.to_string(),
        kind: ProviderCancellationArtifactKind::PendingSession,
        text,
    });
}

fn record_cancellation_stdout_line(
    store: &ProviderCancellationArtifactStore,
    provider_label: &'static str,
    run_id: &str,
    line: &str,
) {
    store.push(ProviderCancellationArtifact {
        provider: provider_label,
        run_id: run_id.to_string(),
        kind: ProviderCancellationArtifactKind::StdoutLine,
        text: line.to_string(),
    });
}

fn drain_pending_stdout_lines(
    stdout_rx: &mpsc::Receiver<ProviderIoEvent>,
    on_stdout_line: &mut impl FnMut(String) -> Result<ProviderLineProgress, AdapterError>,
    sender: &mpsc::Sender<Result<AgentEvent, AdapterError>>,
) -> Result<(), AdapterError> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        let remaining = deadline - now;
        let wait = remaining.min(Duration::from_millis(50));
        match stdout_rx.recv_timeout(wait) {
            Ok(ProviderIoEvent::Line(line)) => {
                on_stdout_line(line)?;
            }
            Ok(ProviderIoEvent::Closed) => return Ok(()),
            Ok(ProviderIoEvent::ReadError(message)) => {
                let _ = sender.send(Err(AdapterError {
                    message: message.clone(),
                }));
                return Err(AdapterError { message });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn drain_cancellation_stdout_artifacts(
    stdout_rx: &mpsc::Receiver<ProviderIoEvent>,
    store: &ProviderCancellationArtifactStore,
    provider_label: &'static str,
    run_id: &str,
) {
    const MAX_DRAINED_LINES: usize = 16;
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut drained = 0;
    while drained < MAX_DRAINED_LINES && Instant::now() < deadline {
        match stdout_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ProviderIoEvent::Line(line)) => {
                record_cancellation_stdout_line(store, provider_label, run_id, &line);
                drained += 1;
            }
            Ok(ProviderIoEvent::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(ProviderIoEvent::ReadError(message)) => {
                record_cancellation_stdout_line(store, provider_label, run_id, &message);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn record_cancellation_stderr_tail(
    store: &ProviderCancellationArtifactStore,
    provider_label: &'static str,
    run_id: &str,
    stderr_tail: &str,
) {
    store.push(ProviderCancellationArtifact {
        provider: provider_label,
        run_id: run_id.to_string(),
        kind: ProviderCancellationArtifactKind::StderrTail,
        text: stderr_tail.to_string(),
    });
}

pub(crate) fn agent_event_is_provider_progress(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::StatusChanged { phase, .. } => matches!(
            phase.as_str(),
            "thinking" | "requesting" | "tool" | "question" | "streaming"
        ),
        AgentEvent::TextDelta { text, .. } => !text.trim().is_empty(),
        AgentEvent::Recommendation { .. }
        | AgentEvent::ToolCall { .. }
        | AgentEvent::UserQuestion { .. }
        | AgentEvent::Action { .. }
        | AgentEvent::ToolPermissionRequest { .. }
        | AgentEvent::ToolOutputDelta { .. }
        | AgentEvent::ToolCompleted { .. }
        | AgentEvent::ToolHookVerdict { .. }
        | AgentEvent::AgentCompleted { .. }
        | AgentEvent::AgentFailed { .. }
        | AgentEvent::AgentCancelled { .. }
        | AgentEvent::AuthRequired { .. }
        | AgentEvent::ShellEvidenceRequest { .. }
        | AgentEvent::HookNotification { .. } => true,
    }
}

fn spawn_stdout_reader(
    stdout: impl Read + Send + 'static,
    provider_label: &'static str,
) -> (mpsc::Receiver<ProviderIoEvent>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if tx.send(ProviderIoEvent::Line(line)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(ProviderIoEvent::ReadError(format!(
                        "failed to read {provider_label} stream: {err}"
                    )));
                    return;
                }
            }
        }
        let _ = tx.send(ProviderIoEvent::Closed);
    });
    (rx, reader)
}

fn spawn_stderr_tail_reader(
    stderr: impl Read + Send + 'static,
    limit: usize,
) -> (StderrTail, thread::JoinHandle<()>) {
    let tail = StderrTail::new(limit);
    let reader_tail = tail.clone();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0_u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => reader_tail.push(&buffer[..n]),
                Err(err) => {
                    reader_tail.push(format!("\n[stderr read error: {err}]\n").as_bytes());
                    break;
                }
            }
        }
    });
    (tail, reader)
}

fn join_provider_readers(
    stdout_reader: thread::JoinHandle<()>,
    stderr_reader: thread::JoinHandle<()>,
) {
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
}

fn timeout_failure_message(
    provider_label: &str,
    timeout: AgentProcessTimeout,
    stderr_tail: &str,
) -> String {
    let reason = match timeout.kind {
        AgentProcessTimeoutKind::Start => {
            format!("No provider response within {}s", timeout.limit.as_secs())
        }
        AgentProcessTimeoutKind::Idle => {
            format!("No provider progress within {}s", timeout.limit.as_secs())
        }
        AgentProcessTimeoutKind::ApprovalWait => {
            format!(
                "No user approval response within {}s",
                timeout.limit.as_secs()
            )
        }
        AgentProcessTimeoutKind::Hard => {
            format!("Agent exceeded {}s limit", timeout.limit.as_secs())
        }
    };
    let mut message = format!(
        "Agent timed out: {reason}\nadapter: {provider_label}\nelapsed: {}s\nlast activity: {}s ago",
        timeout.elapsed.as_secs(),
        timeout.last_activity_age.as_secs()
    );
    let trimmed_tail = stderr_tail.trim();
    if !trimmed_tail.is_empty() {
        message.push_str("\nstderr tail:\n");
        message.push_str(trimmed_tail);
    }
    message
}

pub(crate) fn terminate_process_group(pid: u32) {
    signal_process_group(pid, libc::SIGTERM);
}

pub(crate) fn terminate_and_reap_process(child: &mut Child) {
    let pid = child.id();
    if matches!(child.try_wait(), Ok(Some(_))) {
        kill_process_group_members(pid);
        return;
    }

    terminate_process_group(pid);
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => {
                kill_process_group_members(pid);
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }

    kill_process_group(pid);
    let _ = child.wait();
}

fn kill_process_group(pid: u32) {
    signal_process_group(pid, libc::SIGKILL);
}

fn kill_process_group_members(pid: u32) {
    let pid = pid as i32;
    unsafe {
        // The leader has already been reaped, so its positive PID may have been reused.
        let _ = libc::kill(-pid, libc::SIGKILL);
    }
}

fn signal_process_group(pid: u32, signal: i32) {
    let pid = pid as i32;
    unsafe {
        let _ = libc::kill(-pid, signal);
        let _ = libc::kill(pid, signal);
    }
}

fn clear_child_pid(child_pid: &Arc<Mutex<Option<u32>>>) {
    if let Ok(mut pid) = child_pid.lock() {
        *pid = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_only_tail_bytes() {
        let tail = StderrTail::new(5);
        tail.push(b"abc");
        tail.push(b"defgh");
        assert_eq!(tail.snapshot(), "defgh");
    }

    #[test]
    fn initialized_status_is_not_provider_progress() {
        assert!(!agent_event_is_provider_progress(
            &AgentEvent::StatusChanged {
                run_id: "run".to_string(),
                phase: "initialized".to_string(),
                message: "co initialized qwen3.6-flash".to_string(),
            }
        ));
        assert!(agent_event_is_provider_progress(
            &AgentEvent::StatusChanged {
                run_id: "run".to_string(),
                phase: "thinking".to_string(),
                message: "thinking".to_string(),
            }
        ));
        assert!(agent_event_is_provider_progress(&AgentEvent::TextDelta {
            run_id: "run".to_string(),
            text: "hello".to_string(),
        }));
    }
}
