use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::types::{AgentEvent, AgentRequest};

use super::claude::{is_terminal_agent_event, line_progress, update_completion_flags};
use super::cosh_core::{
    commit_pending_session_for_scope, invalidate_resume_on_session_failure, retain_context_session,
    terminal_events_for_session_commit, SessionResumeAttempt, SessionRuntimeState,
};
use super::{
    agent_event_is_provider_progress, control_protocol, run_provider_process_loop,
    spawn_provider_child, AdapterError, ClaudeStreamParser, PreparedInvocation,
    ProviderCancellationArtifactStore, ProviderPromptArgMode, ProviderRunOutcome,
    ProviderStdinMode,
};

mod input;

pub(super) fn run_sync_cosh_core_process(
    request: &AgentRequest,
    prepared: &PreparedInvocation,
    session_state: &Arc<Mutex<SessionRuntimeState>>,
    session_scope: &str,
    resume_attempt: &SessionResumeAttempt,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<(), AdapterError> {
    let pending_session = Arc::new(Mutex::new(None));
    let mut observed_resumability = None;
    let mut recovery_finalized = false;
    let result = (|| {
        sink(AgentEvent::StatusChanged {
            run_id: request.id.clone(),
            phase: "starting".to_string(),
            message: "starting cosh-core headless backend".to_string(),
        })?;
        let sync_child =
            input::spawn_sync_cosh_core_child(prepared, request.user_input.as_deref())?;
        let (mut child, writer_failure, writer) = sync_child.into_parts();
        let child_pid = Arc::new(Mutex::new(Some(child.id())));
        let mut parser =
            ClaudeStreamParser::new(request.id.clone(), Some(Arc::clone(&pending_session)));
        let mut completed = false;
        let mut failed = false;
        let mut terminal_events = Vec::new();
        let (process_tx, process_rx) = mpsc::channel();
        let outcome = run_provider_process_loop(
            request.id.clone(),
            "cosh-core",
            &mut child,
            child_pid,
            Arc::new(AtomicBool::new(false)),
            ProviderCancellationArtifactStore::default(),
            &process_tx,
            |line| {
                input::check_writer_failure(&writer_failure)?;
                let events = parser.parse_line(&line);
                observed_resumability = parser.session_resumable();
                let progressed = events.iter().any(agent_event_is_provider_progress);
                for event in events {
                    update_completion_flags(&event, &mut completed, &mut failed);
                    if is_terminal_agent_event(&event) {
                        terminal_events.push(event);
                    } else {
                        sink(event)?;
                    }
                }
                Ok(line_progress(progressed))
            },
            || {
                input::check_writer_failure(&writer_failure)?;
                Ok(Vec::new())
            },
        );
        let (process_events, mut transport_error) = drain_process_events(&process_rx);
        transport_error = transport_error.or(writer.finish());
        let transport_failed =
            matches!(outcome, ProviderRunOutcome::Failed) || transport_error.is_some();
        let exit_failure = match outcome {
            ProviderRunOutcome::Cancelled => {
                let _ = commit_pending_session_for_scope(
                    false,
                    true,
                    session_state,
                    &pending_session,
                    session_scope,
                    observed_resumability,
                    resume_attempt,
                );
                recovery_finalized = true;
                for event in process_events {
                    sink(event)?;
                }
                if let Some(error) = transport_error {
                    return Err(error);
                }
                return Ok(());
            }
            ProviderRunOutcome::Failed => None,
            ProviderRunOutcome::Exited {
                status,
                stderr_tail,
            } if !status.success() => Some(exit_failure_message(&status, &stderr_tail)),
            ProviderRunOutcome::Exited { .. } => None,
        };

        let had_terminal_result = !terminal_events.is_empty();
        let finish_result = parser.finish(&mut |event| {
            update_completion_flags(&event, &mut completed, &mut failed);
            if is_terminal_agent_event(&event) {
                terminal_events.push(event);
                Ok(())
            } else {
                sink(event)
            }
        });
        suppress_synthetic_completion_after_transport_failure(
            transport_failed,
            had_terminal_result,
            &mut completed,
            &mut failed,
            &mut terminal_events,
        );
        replace_synthetic_completion_for_nonzero_exit(
            &request.id,
            exit_failure,
            had_terminal_result,
            &mut completed,
            &mut failed,
            &mut terminal_events,
        );
        observed_resumability = parser.session_resumable();
        invalidate_resume_on_session_failure(
            resume_attempt,
            parser.session_error_code(),
            parser.session_error_phase(),
            &terminal_events,
            session_state,
        );
        let retain_session = retain_context_session(
            &terminal_events,
            parser.session_error_phase(),
            observed_resumability,
        );
        let commit_outcome = commit_pending_session_for_scope(
            completed || retain_session,
            failed && !retain_session,
            session_state,
            &pending_session,
            session_scope,
            observed_resumability,
            resume_attempt,
        );
        recovery_finalized = true;
        for event in
            terminal_events_for_session_commit(&request.id, terminal_events, commit_outcome)
        {
            sink(event)?;
        }
        for event in process_events {
            sink(event)?;
        }
        if let Some(error) = transport_error {
            return Err(error);
        }
        finish_result?;
        Ok(())
    })();

    if result.is_err() && !recovery_finalized {
        let _ = commit_pending_session_for_scope(
            false,
            true,
            session_state,
            &pending_session,
            session_scope,
            observed_resumability,
            resume_attempt,
        );
    }
    result
}

pub(super) fn exit_failure_message(status: &std::process::ExitStatus, stderr_tail: &str) -> String {
    let stderr = stderr_tail.trim();
    if stderr.is_empty() {
        format!("cosh-core exited with status {status}")
    } else {
        stderr.to_string()
    }
}

pub(super) fn replace_synthetic_completion_for_nonzero_exit(
    run_id: &str,
    exit_failure: Option<String>,
    had_terminal_result: bool,
    completed: &mut bool,
    failed: &mut bool,
    terminal_events: &mut Vec<AgentEvent>,
) {
    let Some(error) = exit_failure.filter(|_| !had_terminal_result) else {
        return;
    };
    // Parser finish synthesizes completion for legacy zero-result providers. A nonzero
    // process exit is authoritative only when no structured terminal result was parsed.
    terminal_events.clear();
    *completed = false;
    *failed = true;
    terminal_events.push(AgentEvent::AgentFailed {
        run_id: run_id.to_string(),
        error,
        error_code: None,
        max_turns: None,
    });
}

pub(super) fn suppress_synthetic_completion_after_transport_failure(
    transport_failed: bool,
    had_terminal_result: bool,
    completed: &mut bool,
    failed: &mut bool,
    terminal_events: &mut Vec<AgentEvent>,
) {
    if !transport_failed || had_terminal_result {
        return;
    }
    terminal_events.clear();
    *completed = false;
    *failed = true;
}

pub(super) fn drain_process_events(
    receiver: &mpsc::Receiver<Result<AgentEvent, AdapterError>>,
) -> (Vec<AgentEvent>, Option<AdapterError>) {
    let mut events = Vec::new();
    let mut first_error = None;
    for event in receiver.try_iter() {
        match event {
            Ok(event) => events.push(event),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    (events, first_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosh_core_driver_deduplicates_late_shell_evidence_snapshot_result() {
        let mut pending = control_protocol::PendingControlProtocolToolCall::default();

        assert!(pending
            .stage_or_emit(AgentEvent::ToolCall {
                run_id: "run-cosh-core".to_string(),
                tool_id: Some("toolu-evidence".to_string()),
                name: "cosh_shell_evidence".to_string(),
                input: r#"{"action":"list_commands"}"#.to_string(),
            })
            .is_empty());
        assert_eq!(
            pending
                .flush_stalled(control_protocol::PENDING_CONTROL_TOOL_CALL_GRACE)
                .len(),
            0
        );

        let released = pending.flush_stalled(Duration::from_millis(0));
        assert_eq!(released.len(), 1);
        assert!(!pending.take_matching_control_tool_call("run-cosh-core", "toolu-evidence"));
        assert!(pending
            .stage_or_emit(AgentEvent::ToolCompleted {
                run_id: "run-cosh-core".to_string(),
                tool_id: "toolu-evidence".to_string(),
                status: "success".to_string(),
            })
            .is_empty());
    }
}
