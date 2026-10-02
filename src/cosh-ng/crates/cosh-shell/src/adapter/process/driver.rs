//! Shared subprocess lifecycle for vendor stream-json adapters.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::types::AgentEvent;

use super::super::{
    commit_provider_session_if_completed, control_protocol, AdapterError, AgentRunHandle,
    ApprovalChannelMessage, ApprovalDecision, ApprovalResponse, PreparedInvocation,
    ProviderCancellationArtifactStore,
};
use super::{
    agent_event_is_provider_progress, record_cancellation_pending_session,
    run_provider_process_loop, spawn_provider_child, terminate_process_group, ProviderLineProgress,
    ProviderPromptArgMode, ProviderRunOutcome, ProviderStdinMode,
};

/// Provider-specific stream parser used by the shared process driver.
pub(crate) trait ProviderStreamParser: Send + 'static {
    /// Converts one provider output line into normalized Agent events.
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;

    /// Flushes parser state after a successful provider exit.
    fn finish(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError>;
}

/// Provider seams that must remain distinct across shared lifecycle code.
pub(crate) trait ProviderDriverSpec: Send + Sync + 'static {
    /// Parser implementation owned by the provider adapter.
    type Parser: ProviderStreamParser;

    /// Stable label used in transport errors and cancellation artifacts.
    const PROVIDER_LABEL: &'static str;
    /// Status message emitted before a plain stream-json run.
    const STREAM_START_MESSAGE: &'static str;
    /// Status message emitted before a control-protocol run.
    const CONTROL_START_MESSAGE: &'static str;
    /// Provider-specific prompt argv encoding for plain runs.
    const PLAIN_PROMPT_MODE: ProviderPromptArgMode;

    /// Creates the provider parser while preserving pending-session capture.
    fn parser(run_id: String, pending_session: Arc<Mutex<Option<String>>>) -> Self::Parser;

    /// Applies provider capability compatibility rules.
    fn map_capabilities(
        capabilities: control_protocol::ControlProtocolCapabilities,
    ) -> control_protocol::ControlProtocolCapabilities;

    /// Encodes an allow decision in the provider's native wire shape.
    fn serialize_allow(response: &ApprovalResponse) -> String;
}

fn line_progress(progressed: bool) -> ProviderLineProgress {
    if progressed {
        ProviderLineProgress::Progress
    } else {
        ProviderLineProgress::NoProgress
    }
}

fn update_completion_flags(event: &AgentEvent, completed: &mut bool, failed: &mut bool) {
    match event {
        AgentEvent::AgentCompleted { .. } => *completed = true,
        AgentEvent::AgentFailed { .. } | AgentEvent::AgentCancelled { .. } => *failed = true,
        _ => {}
    }
}

fn is_terminal_agent_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. }
    )
}

fn reduce_terminal_event(terminal_events: &mut Vec<AgentEvent>, event: AgentEvent) {
    let incoming_failure = matches!(
        event,
        AgentEvent::AgentFailed { .. } | AgentEvent::AgentCancelled { .. }
    );
    match terminal_events.first() {
        None => terminal_events.push(event),
        Some(AgentEvent::AgentCompleted { .. }) if incoming_failure => {
            terminal_events[0] = event;
        }
        Some(_) => {}
    }
}

fn send_agent_event(sender: &mpsc::Sender<Result<AgentEvent, AdapterError>>, event: AgentEvent) {
    let _ = sender.send(Ok(event));
}

/// Starts a cancellable plain stream-json provider run.
pub(crate) fn start_cancellable_provider_process<S: ProviderDriverSpec>(
    run_id: String,
    prepared: PreparedInvocation,
    session_state: Arc<Mutex<Option<String>>>,
) -> AgentRunHandle {
    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let child_pid = Arc::new(Mutex::new(None::<u32>));
    let pending_session = Arc::new(Mutex::new(None));
    let cancellation_artifacts = ProviderCancellationArtifactStore::default();

    let cancel_flag = Arc::clone(&cancelled);
    let cancel_pid = Arc::clone(&child_pid);
    let cancel = Arc::new(move || {
        cancel_flag.store(true, Ordering::SeqCst);
        if let Some(pid) = cancel_pid.lock().ok().and_then(|guard| *guard) {
            terminate_process_group(pid);
        }
    });

    let pending_session_for_thread = Arc::clone(&pending_session);
    let cancellation_artifacts_for_thread = cancellation_artifacts.clone();
    thread::spawn(move || {
        send_agent_event(
            &sender,
            AgentEvent::StatusChanged {
                run_id: run_id.clone(),
                phase: "starting".to_string(),
                message: S::STREAM_START_MESSAGE.to_string(),
            },
        );

        let mut child = match spawn_provider_child(
            &prepared,
            S::PROVIDER_LABEL,
            ProviderStdinMode::Null,
            S::PLAIN_PROMPT_MODE,
        ) {
            Ok(child) => child,
            Err(err) => {
                let _ = sender.send(Err(err));
                return;
            }
        };

        if let Ok(mut pid) = child_pid.lock() {
            *pid = Some(child.id());
        }
        if cancelled.load(Ordering::SeqCst) {
            terminate_process_group(child.id());
        }

        let mut parser = S::parser(run_id.clone(), Arc::clone(&pending_session_for_thread));
        let mut completed = false;
        let mut failed = false;
        let mut terminal_events = Vec::new();
        let outcome = run_provider_process_loop(
            run_id.clone(),
            S::PROVIDER_LABEL,
            &mut child,
            Arc::clone(&child_pid),
            Arc::clone(&cancelled),
            cancellation_artifacts_for_thread.clone(),
            &sender,
            |line| {
                let events = parser.parse_line(&line);
                let progressed = events.iter().any(agent_event_is_provider_progress);
                for event in events {
                    update_completion_flags(&event, &mut completed, &mut failed);
                    if is_terminal_agent_event(&event) {
                        reduce_terminal_event(&mut terminal_events, event);
                    } else {
                        send_agent_event(&sender, event);
                    }
                }
                Ok(line_progress(progressed))
            },
            || Ok(Vec::new()),
        );

        match &outcome {
            ProviderRunOutcome::Cancelled | ProviderRunOutcome::Failed => {
                record_cancellation_pending_session(
                    &cancellation_artifacts_for_thread,
                    S::PROVIDER_LABEL,
                    &run_id,
                    pending_session_for_thread
                        .lock()
                        .ok()
                        .and_then(|session| session.clone()),
                );
                return;
            }
            ProviderRunOutcome::Exited {
                status,
                stderr_tail,
            } => {
                if !status.success() {
                    let error = stderr_tail.trim().to_string();
                    send_agent_event(
                        &sender,
                        AgentEvent::AgentFailed {
                            run_id,
                            error,
                            error_code: None,
                            max_turns: None,
                        },
                    );
                    return;
                }
            }
        }

        let _ = parser.finish(&mut |event| {
            update_completion_flags(&event, &mut completed, &mut failed);
            if is_terminal_agent_event(&event) {
                reduce_terminal_event(&mut terminal_events, event);
            } else {
                send_agent_event(&sender, event);
            }
            Ok(())
        });
        commit_provider_session_if_completed(
            &outcome,
            completed,
            failed,
            &session_state,
            &pending_session_for_thread,
        );
        for event in terminal_events {
            send_agent_event(&sender, event);
        }
    });

    AgentRunHandle {
        receiver,
        cancel,
        approval_sender: None,
        question_answer_confirmation: None,
        auth_sender: None,
        control_capabilities: Arc::new(Mutex::new(
            control_protocol::ControlProtocolCapabilities::default(),
        )),
        pending_provider_session: Some(pending_session),
        cancellation_artifacts,
    }
}

/// Starts a cancellable provider run with the shared control protocol.
pub(crate) fn start_control_protocol_provider_process<S: ProviderDriverSpec>(
    run_id: String,
    prepared: PreparedInvocation,
    session_state: Arc<Mutex<Option<String>>>,
) -> AgentRunHandle {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel::<ApprovalChannelMessage>();
    let cancelled = Arc::new(AtomicBool::new(false));
    let writer_done = Arc::new(AtomicBool::new(false));
    let child_pid = Arc::new(Mutex::new(None::<u32>));
    let pending_session = Arc::new(Mutex::new(None));
    let cancellation_artifacts = ProviderCancellationArtifactStore::default();
    let control_capabilities = Arc::new(Mutex::new(S::map_capabilities(
        control_protocol::ControlProtocolCapabilities::default(),
    )));
    let control_capabilities_for_thread = Arc::clone(&control_capabilities);

    let cancel_flag = Arc::clone(&cancelled);
    let cancel_pid = Arc::clone(&child_pid);
    let cancel = Arc::new(move || {
        cancel_flag.store(true, Ordering::SeqCst);
        if let Some(pid) = cancel_pid.lock().ok().and_then(|guard| *guard) {
            terminate_process_group(pid);
        }
    });

    let prompt = prepared.prompt.clone();
    let prompt_for_writer = prompt.clone();
    let prompt_for_loop = prompt.clone();

    let pending_session_for_thread = Arc::clone(&pending_session);
    let cancellation_artifacts_for_thread = cancellation_artifacts.clone();
    let approval_tx_for_thread = approval_tx.clone();
    thread::spawn(move || {
        send_agent_event(
            &event_tx,
            AgentEvent::StatusChanged {
                run_id: run_id.clone(),
                phase: "starting".to_string(),
                message: S::CONTROL_START_MESSAGE.to_string(),
            },
        );

        let mut child = match spawn_provider_child(
            &prepared,
            S::PROVIDER_LABEL,
            ProviderStdinMode::Piped,
            ProviderPromptArgMode::None,
        ) {
            Ok(child) => child,
            Err(err) => {
                let _ = event_tx.send(Err(err));
                return;
            }
        };

        if let Ok(mut pid) = child_pid.lock() {
            *pid = Some(child.id());
        }
        if cancelled.load(Ordering::SeqCst) {
            terminate_process_group(child.id());
        }

        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = event_tx.send(Err(AdapterError {
                    message: "failed to capture stdin".to_string(),
                }));
                return;
            }
        };
        // stdin writer thread
        let writer_done_for_thread = Arc::clone(&writer_done);
        let writer_cancelled = Arc::clone(&cancelled);
        thread::spawn(move || {
            use std::io::Write;
            let mut writer = std::io::BufWriter::new(stdin);

            let init_msg = control_protocol::serialize_initialize("init-1");
            let _ = writeln!(writer, "{init_msg}");
            let _ = writer.flush();

            if !prompt_for_writer.is_empty() {
                let user_msg = control_protocol::serialize_user_message(&prompt_for_writer, None);
                let _ = writeln!(writer, "{user_msg}");
                let _ = writer.flush();
            }

            while !writer_done_for_thread.load(Ordering::SeqCst)
                && !writer_cancelled.load(Ordering::SeqCst)
            {
                let msg = match approval_rx.recv_timeout(Duration::from_millis(100)) {
                    // Receipts exist only for cosh-core's residual approval
                    // timeout; provider control protocols have no receipt
                    // semantic, so consume them without touching the wire.
                    Ok(ApprovalChannelMessage::Receipt { .. }) => continue,
                    Ok(ApprovalChannelMessage::Response(response)) => match &response.decision {
                        ApprovalDecision::Allow => S::serialize_allow(&response),
                        ApprovalDecision::Deny { message } => {
                            control_protocol::serialize_deny(&response.request_id, message)
                        }
                        ApprovalDecision::HostExecutedShell { result } => {
                            control_protocol::serialize_host_executed_shell_result(
                                &response.request_id,
                                result,
                            )
                        }
                        ApprovalDecision::Answer { answer } => {
                            control_protocol::serialize_answer(&response.request_id, answer)
                        }
                        ApprovalDecision::ShellEvidence { result } => {
                            control_protocol::serialize_shell_evidence_result(
                                &response.request_id,
                                result,
                            )
                        }
                    },
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if writeln!(writer, "{msg}").is_err() {
                    break;
                }
                if writer.flush().is_err() {
                    break;
                }
            }
        });

        let mut parser = S::parser(run_id.clone(), Arc::clone(&pending_session_for_thread));
        let pending_control_tool_call =
            RefCell::new(control_protocol::PendingControlProtocolToolCall::default());
        let control_capabilities_for_loop = Arc::clone(&control_capabilities_for_thread);
        let approval_tx_for_loop = approval_tx_for_thread.clone();
        let mut completed = false;
        let mut failed = false;
        let mut terminal_events = Vec::new();
        let outcome = run_provider_process_loop(
            run_id.clone(),
            S::PROVIDER_LABEL,
            &mut child,
            Arc::clone(&child_pid),
            Arc::clone(&cancelled),
            cancellation_artifacts_for_thread.clone(),
            &event_tx,
            |line| {
                if let Some(response) = control_protocol::parse_initialize_response(&line, "init-1")
                {
                    let capabilities = response.map_err(|message| AdapterError { message })?;
                    if let Ok(mut current) = control_capabilities_for_loop.lock() {
                        *current = S::map_capabilities(capabilities);
                    }
                    return Ok(ProviderLineProgress::NoProgress);
                }

                if let Some(ctrl) = control_protocol::parse_control_request(&line) {
                    match ctrl {
                        control_protocol::ControlRequest::CanUseTool {
                            request_id,
                            tool_name,
                            tool_input,
                            tool_use_id,
                            hook_requires_approval,
                            audit_ref,
                        } => {
                            let _ = pending_control_tool_call
                                .borrow_mut()
                                .take_matching_control_shell(&run_id, &tool_use_id);
                            if let Some(response) =
                                control_protocol::analysis_continuation_shell_deny_response(
                                    &prompt_for_loop,
                                    &request_id,
                                    &tool_name,
                                    &tool_input,
                                    &tool_use_id,
                                )
                            {
                                let _ = approval_tx_for_loop
                                    .send(ApprovalChannelMessage::Response(response));
                                return Ok(ProviderLineProgress::AwaitingApproval);
                            }
                            send_agent_event(
                                &event_tx,
                                AgentEvent::ToolPermissionRequest {
                                    run_id: run_id.clone(),
                                    request_id,
                                    tool_name,
                                    tool_input,
                                    tool_use_id,
                                    hook_requires_approval,
                                    audit_ref,
                                },
                            );
                            return Ok(ProviderLineProgress::AwaitingApproval);
                        }
                        control_protocol::ControlRequest::Initialize { request_id } => {
                            let _ = request_id;
                        }
                        control_protocol::ControlRequest::AskUser {
                            request_id,
                            question,
                            options,
                            allow_free_text,
                            selection_mode,
                        } => {
                            send_agent_event(
                                &event_tx,
                                AgentEvent::UserQuestion {
                                    run_id: run_id.clone(),
                                    provider_request_id: Some(request_id),
                                    question,
                                    options,
                                    allow_free_text,
                                    selection_mode,
                                },
                            );
                            return Ok(ProviderLineProgress::AwaitingApproval);
                        }
                        control_protocol::ControlRequest::AuthRequired {
                            request_id,
                            reason,
                            error_message,
                            providers,
                        } => {
                            send_agent_event(
                                &event_tx,
                                AgentEvent::AuthRequired {
                                    run_id: run_id.clone(),
                                    request_id,
                                    reason,
                                    error_message,
                                    providers,
                                },
                            );
                            return Ok(ProviderLineProgress::AwaitingApproval);
                        }
                        control_protocol::ControlRequest::ShellEvidence {
                            request_id,
                            tool_use_id,
                            action,
                        } => {
                            let _ = pending_control_tool_call
                                .borrow_mut()
                                .take_matching_control_tool_call(&run_id, &tool_use_id);
                            send_agent_event(
                                &event_tx,
                                AgentEvent::ShellEvidenceRequest {
                                    run_id: run_id.clone(),
                                    request_id,
                                    tool_use_id,
                                    action,
                                },
                            );
                            return Ok(ProviderLineProgress::AwaitingApproval);
                        }
                    }
                    return Ok(ProviderLineProgress::NoProgress);
                }

                let events = parser.parse_line(&line);
                let progressed = events.iter().any(agent_event_is_provider_progress);
                for event in events {
                    for event in pending_control_tool_call.borrow_mut().stage_or_emit(event) {
                        update_completion_flags(&event, &mut completed, &mut failed);
                        if is_terminal_agent_event(&event) {
                            writer_done.store(true, Ordering::SeqCst);
                            reduce_terminal_event(&mut terminal_events, event);
                        } else {
                            send_agent_event(&event_tx, event);
                        }
                    }
                }
                Ok(line_progress(progressed))
            },
            || {
                Ok(pending_control_tool_call
                    .borrow_mut()
                    .flush_stalled(control_protocol::PENDING_CONTROL_TOOL_CALL_GRACE))
            },
        );

        match &outcome {
            ProviderRunOutcome::Cancelled | ProviderRunOutcome::Failed => {
                writer_done.store(true, Ordering::SeqCst);
                record_cancellation_pending_session(
                    &cancellation_artifacts_for_thread,
                    S::PROVIDER_LABEL,
                    &run_id,
                    pending_session_for_thread
                        .lock()
                        .ok()
                        .and_then(|session| session.clone()),
                );
                return;
            }
            ProviderRunOutcome::Exited {
                status,
                stderr_tail,
            } => {
                if !status.success() {
                    writer_done.store(true, Ordering::SeqCst);
                    let error = stderr_tail.trim().to_string();
                    send_agent_event(
                        &event_tx,
                        AgentEvent::AgentFailed {
                            run_id,
                            error,
                            error_code: None,
                            max_turns: None,
                        },
                    );
                    return;
                }
            }
        }

        let _ = parser.finish(&mut |event| {
            for event in pending_control_tool_call.borrow_mut().stage_or_emit(event) {
                update_completion_flags(&event, &mut completed, &mut failed);
                if is_terminal_agent_event(&event) {
                    writer_done.store(true, Ordering::SeqCst);
                    reduce_terminal_event(&mut terminal_events, event);
                } else {
                    send_agent_event(&event_tx, event);
                }
            }
            Ok(())
        });
        commit_provider_session_if_completed(
            &outcome,
            completed,
            failed,
            &session_state,
            &pending_session_for_thread,
        );
        for event in terminal_events {
            send_agent_event(&event_tx, event);
        }
    });

    AgentRunHandle {
        receiver: event_rx,
        cancel,
        approval_sender: Some(approval_tx),
        question_answer_confirmation: None,
        auth_sender: None,
        control_capabilities,
        pending_provider_session: Some(pending_session),
        cancellation_artifacts,
    }
}
