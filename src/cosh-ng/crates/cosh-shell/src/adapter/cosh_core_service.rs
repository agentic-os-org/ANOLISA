//! Long-lived cosh-core JSONL process shared by Agent turns and registry requests.

mod command;
mod control;
mod process;
mod question;

use std::cell::RefCell;
use std::io::{BufWriter, Write};
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::types::{AgentEvent, CoshApprovalMode};

use super::claude::{
    is_terminal_agent_event, send_agent_event, terminate_process, update_completion_flags,
};
use super::cosh_core::question_ingress::CoshCoreQuestionGate;
use super::cosh_core::{
    commit_pending_session_for_scope, invalidate_resume_on_session_failure, mark_recovery_failure,
    retain_context_session, terminal_events_for_session_commit, SessionResumeAttempt,
    SessionRuntimeState,
};
use super::cosh_core_registry::{
    extension_mutation_requires_reload, registry_timeout, RegistryQueryError,
};
use super::{
    control_protocol, record_cancellation_pending_session, AdapterError, AgentRunHandle,
    ApprovalChannelMessage, ApprovalResponse, AuthResponse, ClaudeStreamParser, PreparedInvocation,
    ProviderCancellationArtifactStore,
};
use process::{
    control_request, execute_registry, flush_pending_reload, process_error, reset_process,
    send_json, send_user_turn, spawn_process, spawn_response_writer, stop_process,
    PersistentProcess,
};

const CANCEL_GRACE: Duration = Duration::from_secs(2);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const POST_TURN_PROTOCOL_GRACE: Duration = Duration::from_millis(10);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub(crate) struct PersistentCoshCoreRuntime {
    command_tx: Mutex<Option<mpsc::Sender<ServiceCommand>>>,
    live: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    cancel_pending: Arc<AtomicBool>,
    reload_pending: Arc<AtomicBool>,
    request_counter: AtomicU64,
    active_stdin: Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
    child_pid: Arc<Mutex<Option<u32>>>,
}

impl PersistentCoshCoreRuntime {
    pub(super) fn start_run(
        &self,
        run_id: String,
        prepared: PreparedInvocation,
        raw_user_input: Option<String>,
        mode: CoshApprovalMode,
        session_state: Arc<Mutex<SessionRuntimeState>>,
        session_scope: String,
        resume_attempt: SessionResumeAttempt,
    ) -> AgentRunHandle {
        let (event_tx, event_rx) = mpsc::channel();
        let (approval_tx, approval_rx) = mpsc::channel();
        let (auth_tx, auth_rx) = mpsc::channel();
        let (answer_confirmation_tx, answer_confirmation_rx) = mpsc::channel();
        let pending_session = Arc::new(Mutex::new(None));
        let cancellation_artifacts = ProviderCancellationArtifactStore::default();
        let control_capabilities = Arc::new(Mutex::new(
            control_protocol::ControlProtocolCapabilities::default(),
        ));
        let cancelled = Arc::new(AtomicBool::new(false));
        let run_done = Arc::new(AtomicBool::new(false));

        let acquired = self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        // A failed answer write cancels the owning turn and immediately starts
        // a fallback turn. Queue that one successor behind teardown; unrelated
        // concurrent starts still fail closed.
        let queued_after_cancel = !acquired
            && self
                .cancel_pending
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
        if !acquired && !queued_after_cancel {
            let _ = mark_recovery_failure(
                &session_state,
                &resume_attempt,
                "cosh-core runtime is already processing a request",
            );
            let _ = event_tx.send(Err(AdapterError {
                message: "cosh-core runtime is already processing a request".to_string(),
            }));
            return AgentRunHandle {
                receiver: event_rx,
                cancel: Arc::new(|| {}),
                approval_sender: Some(approval_tx),
                question_answer_confirmation: None,
                auth_sender: Some(auth_tx),
                control_capabilities,
                pending_provider_session: Some(pending_session),
                cancellation_artifacts,
            };
        }

        let cancel_flag = Arc::clone(&cancelled);
        let cancel_done = Arc::clone(&run_done);
        let cancel_stdin = Arc::clone(&self.active_stdin);
        let cancel_pid = Arc::clone(&self.child_pid);
        let cancel_pending = Arc::clone(&self.cancel_pending);
        let cancel = Arc::new(move || {
            cancel_pending.store(true, Ordering::SeqCst);
            cancel_flag.store(true, Ordering::SeqCst);
            if let Some(stdin) = cancel_stdin.lock().ok().and_then(|current| current.clone()) {
                if let Ok(mut writer) = stdin.lock() {
                    let message = control_request("interrupt", "interrupt-1", Value::Null);
                    let _ = writeln!(writer, "{message}");
                    let _ = writer.flush();
                }
            }
            let done = Arc::clone(&cancel_done);
            let pid = Arc::clone(&cancel_pid);
            thread::spawn(move || {
                thread::sleep(CANCEL_GRACE);
                if !done.load(Ordering::SeqCst) {
                    if let Some(pid) = pid.lock().ok().and_then(|current| *current) {
                        terminate_process(pid);
                    }
                }
            });
        });

        let command = RunCommand {
            run_id,
            prepared,
            raw_user_input,
            mode,
            session_state,
            session_scope,
            resume_attempt,
            event_tx,
            internal_response_tx: approval_tx.clone(),
            approval_rx: Some(approval_rx),
            auth_rx: Some(auth_rx),
            answer_confirmation_tx,
            pending_session: Arc::clone(&pending_session),
            cancellation_artifacts: cancellation_artifacts.clone(),
            control_capabilities: Arc::clone(&control_capabilities),
            cancelled,
            run_done,
        };
        let service_error_tx = command.event_tx.clone();
        let recovery_state = Arc::clone(&command.session_state);
        let recovery_attempt = command.resume_attempt.clone();
        match self.sender().and_then(|sender| {
            sender
                .send(ServiceCommand::Run(command))
                .map_err(|_| AdapterError {
                    message: "cosh-core runtime service stopped".to_string(),
                })
        }) {
            Ok(()) => {}
            Err(error) => {
                self.busy.store(false, Ordering::SeqCst);
                let _ = mark_recovery_failure(&recovery_state, &recovery_attempt, &error.message);
                let _ = service_error_tx.send(Err(error));
            }
        }

        AgentRunHandle {
            receiver: event_rx,
            cancel,
            approval_sender: Some(approval_tx),
            question_answer_confirmation: Some(answer_confirmation_rx),
            auth_sender: Some(auth_tx),
            control_capabilities,
            pending_provider_session: Some(pending_session),
            cancellation_artifacts,
        }
    }

    pub(crate) fn live_registry_query(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Option<Result<Value, RegistryQueryError>> {
        if !self.live.load(Ordering::SeqCst)
            || self
                .busy
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return None;
        }
        let request_id = format!(
            "live-reg-{}-{}",
            std::process::id(),
            self.request_counter.fetch_add(1, Ordering::SeqCst)
        );
        let (response_tx, response_rx) = mpsc::channel();
        let command = RegistryCommand {
            request_id,
            domain: domain.to_string(),
            action: action.to_string(),
            params,
            response_tx,
        };
        let sent = self
            .sender()
            .map_err(|error| RegistryQueryError::Transport(error.message))
            .and_then(|sender| {
                sender.send(ServiceCommand::Registry(command)).map_err(|_| {
                    RegistryQueryError::Transport("cosh-core runtime service stopped".to_string())
                })
            });
        if let Err(error) = sent {
            self.busy.store(false, Ordering::SeqCst);
            return Some(Err(error));
        }
        let timeout = registry_timeout(domain, action);
        Some(response_rx.recv_timeout(timeout).unwrap_or_else(|_| {
            Err(RegistryQueryError::Transport(
                "live registry query timed out".to_string(),
            ))
        }))
    }

    pub(crate) fn note_external_mutation(&self, domain: &str, action: &str) {
        if self.live.load(Ordering::SeqCst) && extension_mutation_requires_reload(domain, action) {
            self.reload_pending.store(true, Ordering::SeqCst);
        }
    }

    fn sender(&self) -> Result<mpsc::Sender<ServiceCommand>, AdapterError> {
        let mut current = self.command_tx.lock().map_err(|_| AdapterError {
            message: "cosh-core runtime lock poisoned".to_string(),
        })?;
        if let Some(sender) = current.as_ref() {
            return Ok(sender.clone());
        }
        let (sender, receiver) = mpsc::channel();
        let live = Arc::clone(&self.live);
        let busy = Arc::clone(&self.busy);
        let cancel_pending = Arc::clone(&self.cancel_pending);
        let reload_pending = Arc::clone(&self.reload_pending);
        let active_stdin = Arc::clone(&self.active_stdin);
        let child_pid = Arc::clone(&self.child_pid);
        thread::spawn(move || {
            service_loop(
                receiver,
                live,
                busy,
                cancel_pending,
                reload_pending,
                active_stdin,
                child_pid,
            );
        });
        *current = Some(sender.clone());
        Ok(sender)
    }
}

enum ServiceCommand {
    Run(RunCommand),
    Registry(RegistryCommand),
    Shutdown,
}

use command::{RegistryCommand, RunCommand};

fn service_loop(
    receiver: mpsc::Receiver<ServiceCommand>,
    live: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    cancel_pending: Arc<AtomicBool>,
    reload_pending: Arc<AtomicBool>,
    active_stdin: Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
    child_pid: Arc<Mutex<Option<u32>>>,
) {
    let mut process = None;
    while let Ok(command) = receiver.recv() {
        match command {
            ServiceCommand::Run(mut command) => {
                busy.store(true, Ordering::SeqCst);
                let result = run_turn(
                    &mut process,
                    &mut command,
                    &live,
                    &reload_pending,
                    &active_stdin,
                    &child_pid,
                );
                match result {
                    Ok(reset_required) => {
                        if reset_required {
                            reset_process(&mut process, &live, &active_stdin, &child_pid);
                        }
                    }
                    Err(error) => {
                        // Choke-point dual-write: every turn-level failure (spawn,
                        // protocol, stream errors) converges here; one warn covers
                        // the whole run_turn surface for post-mortem logs.
                        tracing::warn!(error = %error, "cosh-core turn failed; resetting process");
                        let _ = mark_recovery_failure(
                            &command.session_state,
                            &command.resume_attempt,
                            &error,
                        );
                        let _ = command.event_tx.send(Err(AdapterError { message: error }));
                        reset_process(&mut process, &live, &active_stdin, &child_pid);
                    }
                }
                command.run_done.store(true, Ordering::SeqCst);
                cancel_pending.store(false, Ordering::SeqCst);
                busy.store(false, Ordering::SeqCst);
            }
            ServiceCommand::Registry(command) => {
                let result = match process.as_mut() {
                    Some(process) => execute_registry(process, &command),
                    None => Err(RegistryQueryError::Transport(
                        "live cosh-core process is unavailable".to_string(),
                    )),
                };
                if matches!(&result, Err(RegistryQueryError::Transport(_))) {
                    // Transport failure means the live core stopped responding;
                    // record it before reset so the log explains the reset.
                    tracing::warn!(
                        domain = %command.domain,
                        action = %command.action,
                        "live registry transport failed; resetting cosh-core process"
                    );
                    reset_process(&mut process, &live, &active_stdin, &child_pid);
                }
                let _ = command.response_tx.send(result);
                busy.store(false, Ordering::SeqCst);
            }
            ServiceCommand::Shutdown => {
                if let Some(process) = process.as_mut() {
                    let _ = send_json(
                        &process.stdin,
                        &control_request("shutdown", "shutdown-1", Value::Null),
                    );
                    stop_process(&mut process.child, SHUTDOWN_GRACE);
                }
                break;
            }
        }
    }
    live.store(false, Ordering::SeqCst);
}

fn run_turn(
    process: &mut Option<PersistentProcess>,
    command: &mut RunCommand,
    live: &Arc<AtomicBool>,
    reload_pending: &Arc<AtomicBool>,
    active_stdin: &Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
    child_pid: &Arc<Mutex<Option<u32>>>,
) -> Result<bool, String> {
    let desired_session_id = command.resume_attempt.session_id().map(str::to_string);
    if process.as_mut().is_some_and(|process| {
        process.approval_mode != command.mode
            || process.child.try_wait().ok().flatten().is_some()
            || process.workspace_scope != command.session_scope
            || process.session_id != desired_session_id
    }) {
        reset_process(process, live, active_stdin, child_pid);
    }
    if process.is_none() {
        let mut spawned = spawn_process(&command.prepared, command.mode)?;
        spawned.session_id = desired_session_id;
        spawned.workspace_scope.clone_from(&command.session_scope);
        // Process lifecycle event: core spawn is a key diagnostic node
        // (pid pairs shell and core in logs, run registry, and export).
        tracing::info!(
            pid = spawned.child.id(),
            session_id = %spawned.session_id.as_deref().unwrap_or("default"),
            "cosh-core spawned"
        );
        crate::diagnostics::run_registry::update_core_pid(spawned.child.id());
        *process = Some(spawned);
        let running = process.as_ref().expect("process was just spawned");
        live.store(true, Ordering::SeqCst);
        reload_pending.store(false, Ordering::SeqCst);
        if let Ok(mut current) = active_stdin.lock() {
            *current = Some(Arc::clone(&running.stdin));
        }
        if let Ok(mut pid) = child_pid.lock() {
            *pid = Some(running.child.id());
        }
    }
    let process = process.as_mut().expect("process is available");
    let awaiting_initialize = !process.initialized;
    if awaiting_initialize {
        send_json(
            &process.stdin,
            &control_protocol::serialize_cosh_core_initialize("init-1"),
        )?;
    } else if process.control_capabilities.provider_initialize_seen {
        // The initialize response arrives once per process; later turns seed
        // their per-run capability set from the process record so the #1940
        // receipt gate keeps emitting `approval_receipt` after the first turn.
        if let Ok(mut current) = command.control_capabilities.lock() {
            *current = process.control_capabilities;
        }
    }
    if !awaiting_initialize {
        send_user_turn(process, command, reload_pending)?;
    }

    send_agent_event(
        &command.event_tx,
        AgentEvent::StatusChanged {
            run_id: command.run_id.clone(),
            phase: "starting".to_string(),
            message: "using persistent cosh-core runtime".to_string(),
        },
    );

    let writer_done = Arc::new(AtomicBool::new(false));
    let question_gate = Arc::new(Mutex::new(CoshCoreQuestionGate::default()));
    let (writer_failure_tx, writer_failure_rx) = mpsc::channel();
    let writer_handle = spawn_response_writer(
        Arc::clone(&process.stdin),
        Arc::clone(&writer_done),
        Arc::clone(&command.cancelled),
        command
            .approval_rx
            .take()
            .ok_or_else(|| "approval receiver is unavailable".to_string())?,
        command
            .auth_rx
            .take()
            .ok_or_else(|| "auth receiver is unavailable".to_string())?,
        Arc::clone(&question_gate),
        Arc::clone(&command.control_capabilities),
        writer_failure_tx,
        command.answer_confirmation_tx.clone(),
    );
    let mut parser = ClaudeStreamParser::new(
        command.run_id.clone(),
        Some(Arc::clone(&command.pending_session)),
    )
    .with_session_resumable(process.session_resumable);
    let pending_control_tool_call =
        RefCell::new(control_protocol::PendingControlProtocolToolCall::default());
    let mut completed = false;
    let mut failed = false;
    let mut terminal_events = Vec::new();
    let mut transport_error = None;
    let mut reset_after_turn = false;
    let mut user_turn_sent = !awaiting_initialize;

    'output: while terminal_events.is_empty() {
        let line = match process.output_rx.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(Ok(line)) => line,
            Ok(Err(_)) if command.cancelled.load(Ordering::SeqCst) => break,
            Ok(Err(error)) => {
                transport_error = Some(process_error(process, &error));
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(error) = writer_failure_rx.try_recv() {
                    transport_error = Some(error.message);
                    break;
                }
                if process
                    .child
                    .try_wait()
                    .map_err(|error| format!("failed to inspect cosh-core: {error}"))?
                    .is_some()
                {
                    if command.cancelled.load(Ordering::SeqCst) {
                        break;
                    }
                    transport_error = Some(process_error(
                        process,
                        "cosh-core exited before completing the Agent turn",
                    ));
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if command.cancelled.load(Ordering::SeqCst) {
                    break;
                }
                transport_error = Some(process_error(
                    process,
                    "cosh-core output stream disconnected",
                ));
                break;
            }
        };
        match question::handle_line(&line, &question_gate, &command.run_id, &command.event_tx) {
            Ok(question::QuestionLineOutcome::Handled) => {
                continue;
            }
            Ok(question::QuestionLineOutcome::PassThrough) => {}
            Err(error) => {
                transport_error = Some(error.message);
                break;
            }
        }
        if let Some(response) = control_protocol::parse_initialize_response(&line, "init-1") {
            let capabilities = match response {
                Ok(capabilities) => capabilities,
                Err(error) => {
                    transport_error = Some(error);
                    break;
                }
            };
            // Announced once per process: keep the durable copy on the
            // process record so later turns inherit it (mirrors
            // `session_resumable`).
            process.control_capabilities = capabilities;
            if let Ok(mut current) = command.control_capabilities.lock() {
                *current = capabilities;
            }
            if awaiting_initialize && !user_turn_sent {
                process.initialized = true;
                if let Err(error) = send_user_turn(process, command, reload_pending) {
                    transport_error = Some(error);
                    break;
                }
                user_turn_sent = true;
            }
            continue;
        }
        if control::handle_control_request(
            &line,
            command,
            &pending_control_tool_call,
            &command.event_tx,
        ) {
            continue;
        }
        for event in parser.parse_line(&line) {
            for event in pending_control_tool_call.borrow_mut().stage_or_emit(event) {
                if let Err(error) = question::observe_event(&question_gate, &event) {
                    transport_error = Some(error.message);
                    break 'output;
                }
                update_completion_flags(&event, &mut completed, &mut failed);
                if is_terminal_agent_event(&event) {
                    terminal_events.push(event);
                } else {
                    send_agent_event(&command.event_tx, event);
                }
            }
        }
        for event in pending_control_tool_call
            .borrow_mut()
            .flush_stalled(control_protocol::PENDING_CONTROL_TOOL_CALL_GRACE)
        {
            send_agent_event(&command.event_tx, event);
        }
    }

    if transport_error.is_none() && !terminal_events.is_empty() {
        match process.output_rx.recv_timeout(POST_TURN_PROTOCOL_GRACE) {
            Ok(Err(error)) if error == "cosh-core output reached EOF" => {
                reset_after_turn = true;
            }
            Ok(Err(error)) => transport_error = Some(process_error(process, &error)),
            Ok(Ok(line)) => {
                let events = parser.parse_line(&line);
                if events.iter().all(|event| {
                    matches!(
                        event,
                        AgentEvent::StatusChanged { phase, .. }
                            if phase.starts_with("compaction_recommended_v1:")
                    )
                }) && !events.is_empty()
                {
                    for event in events {
                        send_agent_event(&command.event_tx, event);
                    }
                } else {
                    transport_error = Some(
                        "cosh-core emitted output after the terminal Agent result".to_string(),
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                reset_after_turn = true;
            }
        }
    }

    writer_done.store(true, Ordering::SeqCst);
    let _ = writer_handle.join();
    if transport_error.is_none() {
        if let Ok(error) = writer_failure_rx.try_recv() {
            transport_error = Some(error.message);
        }
    }
    let had_terminal_result = !terminal_events.is_empty();
    let finish_result = parser.finish(&mut |event| {
        for event in pending_control_tool_call.borrow_mut().stage_or_emit(event) {
            question::observe_event(&question_gate, &event)?;
            update_completion_flags(&event, &mut completed, &mut failed);
            if is_terminal_agent_event(&event) {
                terminal_events.push(event);
            } else {
                send_agent_event(&command.event_tx, event);
            }
        }
        Ok(())
    });
    if let Err(error) = finish_result {
        transport_error = Some(error.message);
    }
    if transport_error.is_some() && !had_terminal_result {
        terminal_events.retain(|event| !matches!(event, AgentEvent::AgentCompleted { .. }));
        completed = false;
        failed = true;
    }
    // Only the turn that carried `initialize` sees `system/init`. The parser
    // starts with the process's cached value and writes back any value the
    // current turn announced.
    if let Some(observed) = parser.session_resumable() {
        process.session_resumable = Some(observed);
    }
    let session_resumable = parser.session_resumable().or(process.session_resumable);
    if command.cancelled.load(Ordering::SeqCst)
        && !terminal_events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. }))
    {
        terminal_events.push(AgentEvent::AgentCancelled {
            run_id: command.run_id.clone(),
            reason: "user requested cancellation".to_string(),
        });
    }
    invalidate_resume_on_session_failure(
        &command.resume_attempt,
        parser.session_error_code(),
        parser.session_error_phase(),
        &terminal_events,
        &command.session_state,
    );
    // A retained failure keeps the persisted transcript resumable, so the commit
    // and the process binding below both act on the effective state rather than
    // the raw terminal flags. Cancellation never qualifies: the user asked to
    // stop, so the fresh pending session must not be committed.
    let session_error_phase = parser.session_error_phase();
    let retain_session = !command.cancelled.load(Ordering::SeqCst)
        && retain_context_session(&terminal_events, session_error_phase, session_resumable);
    let session_completed = completed || retain_session;
    let session_failed = failed && !retain_session;
    let commit_outcome = if command.cancelled.load(Ordering::SeqCst) {
        record_cancellation_pending_session(
            &command.cancellation_artifacts,
            "cosh-core",
            &command.run_id,
            command
                .pending_session
                .lock()
                .ok()
                .and_then(|session| session.clone()),
        );
        commit_pending_session_for_scope(
            false,
            true,
            &command.session_state,
            &command.pending_session,
            &command.session_scope,
            session_resumable,
            &command.resume_attempt,
        )
    } else {
        commit_pending_session_for_scope(
            session_completed,
            session_failed,
            &command.session_state,
            &command.pending_session,
            &command.session_scope,
            session_resumable,
            &command.resume_attempt,
        )
    };
    if session_completed && !session_failed && session_resumable != Some(false) {
        process.session_id = command
            .pending_session
            .lock()
            .ok()
            .and_then(|session| session.clone());
    }
    let terminal_events =
        terminal_events_for_session_commit(&command.run_id, terminal_events, commit_outcome);

    flush_pending_reload(process, reload_pending, &command.run_id, &command.event_tx);
    for event in terminal_events {
        send_agent_event(&command.event_tx, event);
    }
    if let Some(error) = transport_error {
        return Err(error);
    }
    Ok(command.cancelled.load(Ordering::SeqCst) || reset_after_turn)
}
