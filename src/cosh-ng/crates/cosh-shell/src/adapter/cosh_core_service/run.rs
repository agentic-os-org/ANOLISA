//! Agent turn lifecycle for the persistent cosh-core service.

use std::cell::RefCell;
use std::io::BufWriter;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::types::AgentEvent;

use super::super::claude::{is_terminal_agent_event, send_agent_event, update_completion_flags};
use super::super::cosh_core::question_ingress::CoshCoreQuestionGate;
use super::super::cosh_core::{
    commit_pending_session_for_scope, invalidate_resume_on_session_failure, retain_context_session,
    terminal_events_for_session_commit, SessionRuntimeState,
};
use super::super::{
    control_protocol, record_cancellation_pending_session, AdapterError, ClaudeStreamParser,
};
use super::command::RunCommand;
use super::process::{
    control_request, flush_pending_reload, process_error, reset_process, send_json, send_user_turn,
    spawn_process, spawn_response_writer, stop_process, PersistentProcess, ProcessBindings,
};
use super::{control, question};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const POST_TURN_PROTOCOL_GRACE: Duration = Duration::from_millis(10);

pub(super) fn run_turn(
    process: &mut Option<PersistentProcess>,
    command: &mut RunCommand,
    live: &Arc<AtomicBool>,
    reload_pending: &Arc<AtomicBool>,
    current_process: &Arc<Mutex<ProcessBindings>>,
    active_stdin: &Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
) -> Result<bool, String> {
    let desired_session_id = command.resume_attempt.session_id().map(str::to_string);
    if process.as_mut().is_some_and(|process| {
        process.approval_mode != command.mode
            || process.child.try_wait().ok().flatten().is_some()
            || process.workspace_scope != command.session_scope
            || process.session_id != desired_session_id
    }) {
        reset_process(process, live, active_stdin, current_process);
    }
    let mut cancelled_after_spawn = false;
    if process.is_none() {
        // Reserve a binding before the potentially slow spawn so the
        // cancellation timer has an expected-stop flag to mark even while no
        // child pid exists yet. The same flag is adopted by the stdout reader
        // and by the direct-kill path after spawn completes.
        let expected_stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut current) = current_process.lock() {
            current.expected_stop = Some(Arc::clone(&expected_stop));
            current.child_pid = None;
        }
        let mut spawned =
            spawn_process(&command.prepared, command.mode, Arc::clone(&expected_stop))?;
        spawned.session_id = desired_session_id;
        spawned.workspace_scope.clone_from(&command.session_scope);
        // Process lifecycle event: core spawn is a key diagnostic node
        // (pid pairs shell and core in logs, run registry, and export).
        tracing::info!(
            pid = spawned.child.id(),
            session_id = %spawned.session_id.as_deref().unwrap_or("default"),
            "cosh-core spawned"
        );
        *process = Some(spawned);
        let running = process.as_mut().expect("process was just spawned");
        live.store(true, Ordering::SeqCst);
        reload_pending.store(false, Ordering::SeqCst);
        if let Ok(mut current) = active_stdin.lock() {
            *current = Some(Arc::clone(&running.stdin));
        }
        if let Ok(mut current) = current_process.lock() {
            current.expected_stop = Some(Arc::clone(&running.expected_stop));
            current.child_pid = Some(running.child.id());
        }
        cancelled_after_spawn =
            command.cancelled.load(Ordering::SeqCst) || expected_stop.load(Ordering::SeqCst);
        if cancelled_after_spawn {
            // The cancel grace period may have already expired while spawn was
            // in progress. Mark the flag and stop the child before it processes
            // a turn so it does not keep running after a run the user asked to
            // abort, and so the stdout reader treats the resulting EOF as
            // controlled.
            running.expected_stop.store(true, Ordering::SeqCst);
            stop_process(&mut running.child, Duration::ZERO);
        }
    }
    let process = process.as_mut().expect("process is available");
    let awaiting_initialize = !process.initialized && !cancelled_after_spawn;
    if awaiting_initialize {
        send_json(
            &process.stdin,
            &control_protocol::serialize_cosh_core_initialize("init-1"),
        )?;
    } else if process.control_capabilities.provider_initialize_seen && !cancelled_after_spawn {
        // The initialize response arrives once per process; later turns seed
        // their per-run capability set from the process record so the #1940
        // receipt gate keeps emitting `approval_receipt` after the first turn.
        if let Ok(mut current) = command.control_capabilities.lock() {
            *current = process.control_capabilities;
        }
    }
    if !awaiting_initialize && !cancelled_after_spawn {
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
    // Set when the provider died because the runtime tore it down (adapter
    // Drop) rather than crashing; finalization resolves that to a
    // cancellation instead of a turn failure.
    let mut teardown_break = false;
    let mut reset_after_turn = false;
    let mut user_turn_sent = !awaiting_initialize;

    'output: while terminal_events.is_empty() {
        let line = match process.output_rx.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(Ok(line)) => line,
            Ok(Err(_)) if command.cancelled.load(Ordering::SeqCst) => break,
            Ok(Err(_)) if process.expected_stop.load(Ordering::SeqCst) => {
                // Runtime teardown killed the provider mid-turn; the reader
                // already classified this EOF as controlled. Route it away
                // from the transport-error path so routine teardown does not
                // surface as a turn failure.
                teardown_break = true;
                break;
            }
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
                    if process.expected_stop.load(Ordering::SeqCst) {
                        // Teardown can win the race against the poll timeout
                        // before the reader's EOF lands; classify the dead
                        // provider exactly like the EOF branch above.
                        teardown_break = true;
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
    let turn_aborted = command.cancelled.load(Ordering::SeqCst) || teardown_break;
    // An aborted turn resolves to AgentCancelled below, so the parser's
    // synthetic fallback completion must not run: with a question still in
    // flight the gate would reject it as premature completion and routine
    // teardown would surface as a turn failure. Genuine premature results
    // were already rejected when observed in the output loop.
    if !turn_aborted {
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
    if command.cancelled.load(Ordering::SeqCst) || teardown_break {
        // An aborted run — user cancellation or runtime teardown — must
        // report cancellation, not success. Strip any AgentCompleted —
        // whether it is the parser's synthetic fallback or a genuine result
        // that raced with the abort — while preserving a real AgentFailed so
        // structured session failures are not lost.
        terminal_events.retain(|event| !matches!(event, AgentEvent::AgentCompleted { .. }));
        if !terminal_events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. }))
        {
            terminal_events.push(AgentEvent::AgentCancelled {
                run_id: command.run_id.clone(),
                reason: if command.cancelled.load(Ordering::SeqCst) {
                    "user requested cancellation".to_string()
                } else {
                    "cosh-core runtime shut down during turn".to_string()
                },
            });
        }
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
    let retain_session = !(command.cancelled.load(Ordering::SeqCst) || teardown_break)
        && retain_context_session(&terminal_events, session_error_phase, session_resumable);
    let session_completed = completed || retain_session;
    let session_failed = failed && !retain_session;
    let commit_outcome = if command.cancelled.load(Ordering::SeqCst) || teardown_break {
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

    let reload_error =
        flush_pending_reload(process, reload_pending, &command.run_id, &command.event_tx);
    if reload_error.is_some() {
        // Already logged; reset the broken process after terminal events.
        reset_after_turn = true;
    }
    for event in terminal_events {
        send_agent_event(&command.event_tx, event);
    }
    if let Some(error) = transport_error {
        return Err(error);
    }
    Ok(command.cancelled.load(Ordering::SeqCst) || teardown_break || reset_after_turn)
}
