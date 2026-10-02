//! Control-protocol request routing for the persistent cosh-core service.

use std::cell::RefCell;
use std::sync::mpsc;

use crate::types::AgentEvent;

use super::super::claude::send_agent_event;
use super::super::{control_protocol, AdapterError, ApprovalChannelMessage};
use super::RunCommand;

pub(super) fn handle_control_request(
    line: &str,
    command: &RunCommand,
    pending: &RefCell<control_protocol::PendingControlProtocolToolCall>,
    event_tx: &mpsc::Sender<Result<AgentEvent, AdapterError>>,
) -> bool {
    let Some(control) = control_protocol::parse_control_request(line) else {
        return false;
    };
    match control {
        control_protocol::ControlRequest::CanUseTool {
            request_id,
            tool_name,
            tool_input,
            tool_use_id,
            hook_requires_approval,
            audit_ref,
        } => {
            let _ = pending
                .borrow_mut()
                .take_matching_control_shell(&command.run_id, &tool_use_id);
            if let Some(response) = control_protocol::analysis_continuation_shell_deny_response(
                &command.prepared.prompt,
                &request_id,
                &tool_name,
                &tool_input,
                &tool_use_id,
            ) {
                let _ = command
                    .internal_response_tx
                    .send(ApprovalChannelMessage::Response(response));
                return true;
            }
            send_agent_event(
                event_tx,
                AgentEvent::ToolPermissionRequest {
                    run_id: command.run_id.clone(),
                    request_id,
                    tool_name,
                    tool_input,
                    tool_use_id,
                    hook_requires_approval,
                    audit_ref,
                },
            );
        }
        control_protocol::ControlRequest::AskUser {
            request_id,
            question,
            options,
            allow_free_text,
            selection_mode,
        } => send_agent_event(
            event_tx,
            AgentEvent::UserQuestion {
                run_id: command.run_id.clone(),
                provider_request_id: Some(request_id),
                question,
                options,
                allow_free_text,
                selection_mode,
            },
        ),
        control_protocol::ControlRequest::AuthRequired {
            request_id,
            reason,
            error_message,
            providers,
        } => send_agent_event(
            event_tx,
            AgentEvent::AuthRequired {
                run_id: command.run_id.clone(),
                request_id,
                reason,
                error_message,
                providers,
            },
        ),
        control_protocol::ControlRequest::ShellEvidence {
            request_id,
            tool_use_id,
            action,
        } => {
            let _ = pending
                .borrow_mut()
                .take_matching_control_tool_call(&command.run_id, &tool_use_id);
            send_agent_event(
                event_tx,
                AgentEvent::ShellEvidenceRequest {
                    run_id: command.run_id.clone(),
                    request_id,
                    tool_use_id,
                    action,
                },
            );
        }
        control_protocol::ControlRequest::Initialize { .. } => {}
    }
    true
}
