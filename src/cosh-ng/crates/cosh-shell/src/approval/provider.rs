use crate::approval::broker::{
    provider_status_response, ProviderApprovalStatus, ProviderResponseInput,
};
use crate::runtime::prelude::*;

pub(super) fn provider_approval_response(
    request: &RuntimeApprovalRequest,
    ctrl_request_id: &str,
) -> ApprovalResponse {
    provider_status_response(
        ProviderResponseInput {
            request_id: ctrl_request_id,
            tool_use_id: request.tool_use_id.as_deref(),
            tool_input: request.tool_input.as_ref(),
        },
        provider_approval_status(request.status),
    )
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
