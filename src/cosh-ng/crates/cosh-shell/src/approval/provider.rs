use crate::approval::approved_tool::request_is_executable_bash_tool;
use crate::approval::broker::{
    provider_deny_response, provider_status_response, ProviderApprovalStatus, ProviderResponseInput,
};
use crate::approval::handoff::{
    shell_handoff_command_from_request, shell_handoff_validation_error,
};
use crate::runtime::prelude::*;

pub(super) fn provider_approval_response(
    request: &RuntimeApprovalRequest,
    ctrl_request_id: &str,
) -> ApprovalResponse {
    let input = ProviderResponseInput {
        request_id: ctrl_request_id,
        tool_use_id: request.tool_use_id.as_deref(),
        tool_input: request.tool_input.as_ref(),
    };
    let status = provider_approval_status(request.status);
    if status == ProviderApprovalStatus::Blocked {
        if let Some(message) = actionable_handoff_block_message(request) {
            return provider_deny_response(input, message);
        }
    }
    provider_status_response(input, status)
}

/// A blocked Bash request whose command cannot be handed to the foreground
/// shell carries a model-facing reason plus a rewrite hint, so the agent can
/// self-correct on the first retry instead of re-sending the same shape.
/// Only the message changes; the block decision and fail-closed gate stay put.
fn actionable_handoff_block_message(request: &RuntimeApprovalRequest) -> Option<String> {
    if !request_is_executable_bash_tool(request) {
        return None;
    }
    let command = shell_handoff_command_from_request(request).ok()?;
    let error = shell_handoff_validation_error(&command)?;
    handoff_block_hint(&error)
}

fn handoff_block_hint(error: &str) -> Option<String> {
    let hint = match error {
        "shell handoff command contains an unsupported line break" => {
            "the command has a line break the foreground shell handoff cannot carry (a bare \
             newline, a newline inside double quotes, a carriage return, or a backslash \
             line-continuation). Rewrite it as a single line (join steps with ';' or '&&'), or put \
             multi-line code inside a single-quoted argument such as python3 -c '...'. Only line \
             feeds inside a complete single-quoted argument are allowed."
        }
        "shell handoff command contains blocked control character" => {
            "the command has a control character the handoff cannot carry. Remove it (tabs are \
             allowed) and retry as a single line."
        }
        "shell handoff command contains NUL byte" => {
            "the command contains a NUL byte. Remove it and retry."
        }
        "empty shell handoff command" => "the command is empty. Provide a command to run.",
        _ => return None,
    };
    Some(format!(
        "cosh-shell blocked this Bash tool request before execution: {hint} The command was not executed."
    ))
}

pub(super) fn provider_approval_status(status: ApprovalRequestStatus) -> ProviderApprovalStatus {
    match status {
        ApprovalRequestStatus::Approved => ProviderApprovalStatus::Approved,
        ApprovalRequestStatus::Blocked => ProviderApprovalStatus::Blocked,
        ApprovalRequestStatus::Denied => ProviderApprovalStatus::Denied,
        ApprovalRequestStatus::Cancelled => ProviderApprovalStatus::Cancelled,
        ApprovalRequestStatus::Pending => ProviderApprovalStatus::Pending,
    }
}

pub(crate) fn mark_provider_approval_resolved(state: &mut InlineState) {
    let i18n = state.i18n();
    if let Some(active_run) = state.agent_run.active.as_mut() {
        active_run.current_phase = i18n.t(MessageId::AgentStatusTool).to_string();
        active_run.current_message = i18n
            .t(MessageId::AgentStatusRunningApprovedProviderTool)
            .to_string();
        active_run.last_activity_at = std::time::Instant::now();
    }
}
