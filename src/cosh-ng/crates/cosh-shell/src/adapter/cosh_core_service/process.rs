//! Process and JSONL I/O primitives for the persistent cosh-core service.

use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStdin};
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::types::CoshApprovalMode;

use super::super::claude::terminate_process;
use super::super::cosh_core::question_ingress::{
    protocol_error, CoreQuestionProtocolReason, CoshCoreQuestionGate,
};
use super::super::cosh_core_registry::registry_timeout;
use super::super::RegistryQueryError;
use super::super::{
    control_protocol, spawn_provider_child, AdapterError, ApprovalChannelMessage, ApprovalDecision,
    ApprovalResponse, AuthResponse, PreparedInvocation, ProviderPromptArgMode, ProviderStdinMode,
};
use super::command::{RegistryCommand, RunCommand};
use super::registry::{execute_registry, log_registry_transport_failure};

pub(super) struct PersistentProcess {
    pub(super) child: Child,
    pub(super) stdin: Arc<Mutex<BufWriter<ChildStdin>>>,
    pub(super) output_rx: mpsc::Receiver<Result<String, String>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    stderr_done: Arc<AtomicBool>,
    /// Set before an intentional shutdown or reset so the stdout reader does
    /// not treat the resulting EOF as a failure.
    pub(super) expected_stop: Arc<AtomicBool>,
    pub(super) initialized: bool,
    pub(super) approval_mode: CoshApprovalMode,
    pub(super) session_id: Option<String>,
    pub(super) workspace_scope: String,
    /// Resumability reported by the `system/init` this process emitted.
    ///
    /// cosh-core announces it once per process, in response to `initialize`, so
    /// a per-turn stream parser only observes it on the first turn. Later turns
    /// read it from here instead of defaulting to "unknown".
    pub(super) session_resumable: Option<bool>,
    /// Control-protocol capabilities announced by this process.
    ///
    /// Like `session_resumable`, the `initialize` response arrives once per
    /// process, so only the first turn parses it. Later turns seed their
    /// per-run capability set from here; otherwise the #1940 receipt gate
    /// would fall back to "not capable" and stop emitting `approval_receipt`
    /// from the second turn on.
    pub(super) control_capabilities: control_protocol::ControlProtocolCapabilities,
}

/// Snapshot of the process bindings that must be kept consistent when a
/// direct-kill path (Drop or forced cancellation) decides which process to
/// terminate. Holding both the expected-stop flag and the pid under one lock
/// guarantees the timer cannot pick up a flag from one process and a pid from
/// another, or miss a process that started after the cancellation was requested.
#[derive(Debug, Default)]
pub(super) struct ProcessBindings {
    /// Expected-stop flag of the currently live process.
    pub(super) expected_stop: Option<Arc<AtomicBool>>,
    /// OS pid of the currently live process.
    pub(super) child_pid: Option<u32>,
}

/// Test-only hook that slows `spawn_process` so cancellation paths can be
/// exercised while a child is starting but not yet bound to the runtime.
#[cfg(test)]
pub(super) static TEST_SPAWN_DELAY_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn test_spawn_delay() {
    let delay = TEST_SPAWN_DELAY_MS.load(Ordering::Relaxed);
    if delay > 0 {
        thread::sleep(Duration::from_millis(delay));
    }
}

#[cfg(not(test))]
fn test_spawn_delay() {}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_response_writer(
    stdin: Arc<Mutex<BufWriter<ChildStdin>>>,
    done: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    approval_rx: mpsc::Receiver<ApprovalChannelMessage>,
    auth_rx: mpsc::Receiver<AuthResponse>,
    question_gate: Arc<Mutex<CoshCoreQuestionGate>>,
    capabilities: Arc<Mutex<control_protocol::ControlProtocolCapabilities>>,
    failure_tx: mpsc::Sender<AdapterError>,
    answer_confirmation_tx: mpsc::Sender<Result<String, AdapterError>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut approval_open = true;
        let mut auth_open = true;
        while !done.load(Ordering::SeqCst) && !cancelled.load(Ordering::SeqCst) {
            if approval_open {
                match approval_rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(ApprovalChannelMessage::Receipt { request_id }) => {
                        // #1940 receipt protocol: only providers that announce
                        // `can_handle_approval_receipt` understand this line;
                        // for the rest the receipt is skipped and the core-side
                        // last-resort guard stays armed (the designed
                        // degradation for a lost receipt).
                        if !control_protocol::receipt_capable(&capabilities) {
                            continue;
                        }
                        if send_json(
                            &stdin,
                            &control_protocol::serialize_approval_receipt(&request_id),
                        )
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    Ok(ApprovalChannelMessage::Response(response)) => {
                        let message = approval_message(&response);
                        if matches!(&response.decision, ApprovalDecision::Answer { .. }) {
                            let write_result =
                                question_gate.lock().map_err(|_| ()).and_then(|mut gate| {
                                    send_json(&stdin, &message).map_err(|_| ())?;
                                    gate.answer_written(&response.request_id);
                                    Ok(())
                                });
                            if write_result.is_err() {
                                let error =
                                    protocol_error(CoreQuestionProtocolReason::AnswerWriteFailed);
                                let _ = answer_confirmation_tx.send(Err(error.clone()));
                                let _ = failure_tx.send(error);
                                break;
                            }
                            let _ = answer_confirmation_tx.send(Ok(response.request_id.clone()));
                        } else if send_json(&stdin, &message).is_err() {
                            break;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => approval_open = false,
                }
            }
            if auth_open {
                match auth_rx.try_recv() {
                    Ok(response) => {
                        let message = control_protocol::serialize_auth_response(
                            &response.request_id,
                            &response.provider_id,
                            response.provider_type.as_deref(),
                            &response.values,
                            response.persist,
                        );
                        if send_json(&stdin, &message).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => auth_open = false,
                }
            }
            if !approval_open && !auth_open {
                break;
            }
        }
    })
}

fn approval_message(response: &ApprovalResponse) -> String {
    match &response.decision {
        ApprovalDecision::Allow => control_protocol::serialize_co_allow(&response.request_id),
        ApprovalDecision::Deny { message } => {
            control_protocol::serialize_deny(&response.request_id, message)
        }
        ApprovalDecision::HostExecutedShell { result } => {
            control_protocol::serialize_host_executed_shell_result(&response.request_id, result)
        }
        ApprovalDecision::Answer { answer } => {
            control_protocol::serialize_answer(&response.request_id, answer)
        }
        ApprovalDecision::ShellEvidence { result } => {
            control_protocol::serialize_shell_evidence_result(&response.request_id, result)
        }
    }
}

fn read_stdout_lines<R: BufRead>(
    reader: R,
    output_tx: &mpsc::Sender<Result<String, String>>,
    core_pid: Option<u32>,
    expected_stop: &AtomicBool,
) {
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if output_tx.send(Ok(line)).is_err() {
                    return;
                }
            }
            Err(error) => {
                // Dual-write: tracing keeps stream errors visible for post-mortem diagnosis.
                tracing::warn!(pid = core_pid, error = %error, "failed to read cosh-core stream");
                let _ = output_tx.send(Err(format!("failed to read cosh-core stream: {error}")));
                return;
            }
        }
    }
    // Only warn for unexpected EOF; controlled shutdowns are expected.
    if expected_stop.load(Ordering::SeqCst) {
        tracing::debug!(
            pid = core_pid,
            "cosh-core output reached EOF after expected stop"
        );
    } else {
        tracing::warn!(pid = core_pid, "cosh-core output reached EOF");
    }
    let _ = output_tx.send(Err("cosh-core output reached EOF".to_string()));
}

fn spawn_stdout_reader<R: Read + Send + 'static>(
    stdout: R,
    output_tx: mpsc::Sender<Result<String, String>>,
    core_pid: Option<u32>,
    expected_stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        read_stdout_lines(BufReader::new(stdout), &output_tx, core_pid, &expected_stop);
    })
}

pub(super) fn spawn_process(
    prepared: &PreparedInvocation,
    approval_mode: CoshApprovalMode,
    expected_stop: Arc<AtomicBool>,
) -> Result<PersistentProcess, String> {
    let mut child = spawn_provider_child(
        prepared,
        "cosh-core",
        ProviderStdinMode::Piped,
        ProviderPromptArgMode::None,
    )
    .map_err(|error| error.message)?;
    // Test hook: simulate a slow spawn so cancellation paths can be verified
    // while the child exists but has not yet been published to the runtime.
    test_spawn_delay();
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to capture cosh-core stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture cosh-core stdout".to_string())?;
    let (output_tx, output_rx) = mpsc::channel();
    let core_pid = Some(child.id());
    spawn_stdout_reader(stdout, output_tx, core_pid, Arc::clone(&expected_stop));
    let stderr_tail = Arc::new(Mutex::new(Vec::new()));
    let stderr_done = Arc::new(AtomicBool::new(false));
    if let Some(mut stderr) = child.stderr.take() {
        let tail = Arc::clone(&stderr_tail);
        let done = Arc::clone(&stderr_done);
        thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => append_bounded(&tail, &chunk[..read]),
                }
            }
            done.store(true, Ordering::SeqCst);
        });
    } else {
        stderr_done.store(true, Ordering::SeqCst);
    }
    Ok(PersistentProcess {
        child,
        stdin: Arc::new(Mutex::new(BufWriter::new(stdin))),
        output_rx,
        stderr_tail,
        stderr_done,
        expected_stop,
        initialized: false,
        approval_mode,
        session_id: None,
        workspace_scope: String::new(),
        session_resumable: None,
        control_capabilities: control_protocol::ControlProtocolCapabilities::default(),
    })
}

const STDERR_TAIL_MAX_BYTES: usize = 64 * 1024;

fn append_bounded(tail: &Arc<Mutex<Vec<u8>>>, bytes: &[u8]) {
    let Ok(mut tail) = tail.lock() else {
        return;
    };
    tail.extend_from_slice(bytes);
    if tail.len() > STDERR_TAIL_MAX_BYTES {
        let overflow = tail.len() - STDERR_TAIL_MAX_BYTES;
        tail.drain(..overflow);
    }
}

pub(super) fn process_error(process: &PersistentProcess, message: &str) -> String {
    let deadline = Instant::now() + Duration::from_millis(100);
    while !process.stderr_done.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let stderr = process
        .stderr_tail
        .lock()
        .ok()
        .map(|tail| String::from_utf8_lossy(&tail).trim().to_string())
        .unwrap_or_default();
    if stderr.is_empty() {
        message.to_string()
    } else {
        format!("{message}\nstderr tail:\n{stderr}")
    }
}

// Consumes a pending deferred reload and replays it into the live core.
// Shared by the pre-turn flush and the end-of-turn check. Transport errors
// are surfaced as status events and returned so the caller can reset the
// broken process; response-level failures only emit the event.
pub(super) fn flush_pending_reload(
    process: &mut PersistentProcess,
    reload_pending: &Arc<AtomicBool>,
    run_id: &str,
    event_tx: &mpsc::Sender<Result<crate::types::AgentEvent, AdapterError>>,
) -> Option<RegistryQueryError> {
    if !reload_pending.swap(false, Ordering::SeqCst) {
        return None;
    }
    let deferred = RegistryCommand {
        request_id: format!("deferred-reload-{}", std::process::id()),
        domain: "extensions".to_string(),
        action: "reload".to_string(),
        params: Value::Null,
        response_tx: mpsc::channel().0,
    };
    let result = execute_registry(process, &deferred);
    let transport_failed = log_registry_transport_failure(&result, &deferred);
    if let Err(error) = result {
        super::super::claude::send_agent_event(
            event_tx,
            crate::types::AgentEvent::StatusChanged {
                run_id: run_id.to_string(),
                phase: "extension_reload_failed".to_string(),
                message: error.clone().into_message(),
            },
        );
        if transport_failed {
            return Some(error);
        }
    }
    None
}

pub(super) fn send_user_turn(
    process: &mut PersistentProcess,
    command: &RunCommand,
    reload_pending: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Apply mutations observed while idle. A transport failure leaves the core
    // broken, so fail the turn and let the service loop reset it.
    if let Some(error) =
        flush_pending_reload(process, reload_pending, &command.run_id, &command.event_tx)
    {
        return Err(error.into_message());
    }
    let session_id = process.session_id.clone();
    send_json(
        &process.stdin,
        &user_message_with_raw_input(
            &command.prepared.prompt,
            command.raw_user_input.as_deref(),
            session_id.as_deref(),
            &command.session_scope,
        ),
    )
}

pub(super) fn reset_process(
    process: &mut Option<PersistentProcess>,
    live: &Arc<AtomicBool>,
    active_stdin: &Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
    current_process: &Arc<Mutex<ProcessBindings>>,
) {
    if let Some(mut running) = process.take() {
        running.expected_stop.store(true, Ordering::SeqCst);
        stop_process(&mut running.child, Duration::ZERO);
    }
    live.store(false, Ordering::SeqCst);
    if let Ok(mut current) = active_stdin.lock() {
        *current = None;
    }
    if let Ok(mut current) = current_process.lock() {
        *current = ProcessBindings::default();
    }
}

pub(super) fn stop_process(child: &mut Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Mark a process as expected to stop and then terminate it.
///
/// Used by direct-kill paths (Drop, forced cancellation) so the stdout reader
/// classifies the resulting EOF as controlled rather than a turn failure.
pub(super) fn terminate_marked(expected_stop: Option<Arc<AtomicBool>>, pid: Option<u32>) {
    if let Some(stop) = expected_stop {
        stop.store(true, Ordering::SeqCst);
    }
    if let Some(pid) = pid {
        terminate_process(pid);
    }
}

pub(super) fn send_json(
    stdin: &Arc<Mutex<BufWriter<ChildStdin>>>,
    message: &str,
) -> Result<(), String> {
    let mut writer = stdin
        .lock()
        .map_err(|_| "cosh-core stdin lock poisoned".to_string())?;
    writeln!(writer, "{message}")
        .and_then(|()| writer.flush())
        .map_err(|error| format!("failed to write cosh-core request: {error}"))
}

pub(super) fn control_request(subtype: &str, request_id: &str, fields: Value) -> String {
    let mut request = serde_json::Map::new();
    request.insert("subtype".to_string(), Value::String(subtype.to_string()));
    if let Value::Object(fields) = fields {
        request.extend(fields);
    }
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
    .to_string()
}

pub(super) fn user_message_with_raw_input(
    content: &str,
    raw_user_input: Option<&str>,
    session_id: Option<&str>,
    cwd: &str,
) -> String {
    let mut message = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
        "session_id": session_id.unwrap_or("default"),
        "shell_context": {"cwd": cwd, "env": {}, "last_exit_code": 0},
    });
    if let Some(raw_user_input) = raw_user_input {
        message["message"]["raw_user_input"] = Value::String(raw_user_input.to_string());
    }
    message.to_string()
}

#[cfg(test)]
mod tests;
