//! Foreground authority backed by the runtime's existing approval channel.
//!
//! The runtime already owns the approval card, the PTY handoff and the
//! evidence collection, and delivers the terminal fact as
//! `ApprovalDecision::HostExecutedShell`. This module only bridges that one
//! round trip onto the host-shell extension; it never executes anything itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{mpsc, Arc, Mutex};

use super::super::{ApprovalChannelMessage, ApprovalDecision, HostExecutedShellResult};
use crate::types::AgentEvent;

use super::host_shell::{
    AcpHostExecuteOutcome, AcpHostExecuteRequest, AcpHostInputWait, AcpHostRefusal,
    CODE_CONSTRAINT, CODE_REFUSED, CODE_UNKNOWN,
};
use super::{AcpHostAuthority, AdapterError};

/// Builds the authority for one run from its event sink and approval channel.
/// The shared cancellation flag bounds the wait without a review deadline: a
/// cancelled run never delivers a new ask, and a wait cut short by cancellation
/// reports an unknown terminal fact instead of claiming a clean stop.
pub(super) fn authority(
    run_id: String,
    events: mpsc::Sender<Result<AgentEvent, AdapterError>>,
    approvals: Arc<Mutex<mpsc::Receiver<ApprovalChannelMessage>>>,
    cancelled: Arc<AtomicBool>,
) -> AcpHostAuthority {
    Arc::new(move |request: &AcpHostExecuteRequest| {
        // Cancellation closes new dispatch first. The ask never leaves, so the
        // action provably did not run.
        if cancelled.load(Ordering::SeqCst) {
            return Err(AcpHostRefusal::new(
                CODE_REFUSED,
                "the run was cancelled before the ask was delivered; the action did not run",
            ));
        }
        let mut tool_input = serde_json::json!({ "command": request.command });
        if let Some(expected) = &request.expected_cwd {
            tool_input["expectedCwd"] = serde_json::Value::String(expected.clone());
        }
        let ask = AgentEvent::ToolPermissionRequest {
            run_id: run_id.clone(),
            request_id: request.tool_call_id.clone(),
            tool_name: "shell".to_string(),
            tool_input,
            tool_use_id: request.tool_call_id.clone(),
            hook_requires_approval: request.hook_requires_approval,
            audit_ref: None,
        };
        // The ask never reached the runtime, so the action provably did not run.
        // Unknown is reserved for losing ownership after a possible dispatch.
        if events.send(Ok(ask)).is_err() {
            return Err(AcpHostRefusal::new(
                CODE_REFUSED,
                "the runtime event channel closed before the ask was delivered; the action did not run",
            ));
        }
        let receiver = approvals.lock().map_err(|_| {
            AcpHostRefusal::new(CODE_UNKNOWN, "the approval channel is unavailable")
        })?;
        loop {
            match receiver.recv_timeout(super::POLL_SLICE) {
                Ok(ApprovalChannelMessage::Response(response))
                    if response.request_id == request.tool_call_id =>
                {
                    return decide(response.decision);
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    // A human review may stay pending for a long time, so only
                    // cancellation ends the wait; the foreground may still be
                    // running, which is unknown rather than refused.
                    if cancelled.load(Ordering::SeqCst) {
                        return Err(AcpHostRefusal::new(
                            CODE_UNKNOWN,
                            "the run was cancelled before a terminal fact was observed; the foreground action may still be running",
                        ));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AcpHostRefusal::new(
                        CODE_UNKNOWN,
                        "the approval channel closed before a terminal fact was observed",
                    ));
                }
            }
        }
    })
}

/// Turns one runtime decision into the extension's terminal result or refusal.
fn decide(decision: ApprovalDecision) -> Result<AcpHostExecuteOutcome, AcpHostRefusal> {
    match decision {
        ApprovalDecision::HostExecutedShell { result } => Ok(outcome(*result)),
        ApprovalDecision::Deny { message } => Err(AcpHostRefusal::new(CODE_REFUSED, message)),
        // A negotiated foreground action belongs to the Shell's PTY. A native
        // allow would hand execution back to the Agent and drop that guarantee.
        ApprovalDecision::Allow => Err(AcpHostRefusal::new(
            CODE_CONSTRAINT,
            "a negotiated foreground action must not receive a native allow",
        )),
        _ => Err(AcpHostRefusal::new(
            CODE_CONSTRAINT,
            "the runtime decision does not answer a foreground execute",
        )),
    }
}

/// Carries the runtime's terminal facts without inventing missing ones.
fn outcome(result: HostExecutedShellResult) -> AcpHostExecuteOutcome {
    let metadata = result.metadata;
    AcpHostExecuteOutcome {
        command: metadata.command,
        status: metadata.status,
        exit_code: Some(metadata.exit_code),
        signal: metadata.signal,
        cwd: metadata.cwd,
        end_cwd: metadata.end_cwd,
        duration_ms: metadata.duration_ms,
        output_ref: metadata.output_ref,
        redaction_status: metadata.redaction_status,
        approval_id: metadata.approval_id,
        tool_use_id: metadata.tool_use_id,
        input_wait: metadata.input_wait.map(|wait| AcpHostInputWait {
            waited_secs: wait.waited_secs,
            interrupted: wait.interrupted,
        }),
        llm_content: result.llm_content,
        llm_content_truncated: false,
    }
}

#[cfg(test)]
mod tests;
