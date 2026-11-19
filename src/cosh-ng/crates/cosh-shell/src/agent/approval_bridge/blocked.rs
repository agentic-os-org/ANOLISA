//! Blocked auto-approval terminal closeout (#10316), split from the approval
//! bridge host to keep it under the production-file ceiling.

use super::AutoApprovalFlow;
use crate::approval::provider::provider_approval_response;
use crate::runtime::prelude::*;

/// Deliver the provider deny verdict (with the actionable rewrite hint when
/// validation failed), settle the approval ledger, render the Blocked receipt,
/// and stop the owning run when the verdict cannot be delivered so the turn
/// never hangs. Shared by the pre-recorded validation/audit block and the
/// host-execution authorization failure.
pub(crate) fn finish_blocked_auto_request<W: Write>(
    state: &mut InlineState,
    request: &mut RuntimeApprovalRequest,
    output: &mut W,
) -> std::io::Result<AutoApprovalFlow> {
    let mut delivered = true;
    if let Some(ctrl_request_id) = request.request_id.as_deref() {
        delivered = false;
        if let Some(active_run) = state.agent_run.active.as_ref() {
            let response = provider_approval_response(request, ctrl_request_id);
            if active_run.handle.respond_approval(response).is_ok() {
                state
                    .control
                    .approval_ledger_mut()
                    .mark_responded(&request.run_id, ctrl_request_id);
                delivered = true;
            }
        }
    }
    render_approval_resolution(
        state,
        request,
        MessageId::ApprovalResolutionBlockedTitle,
        output,
    )?;
    if !delivered {
        stop_active_agent_run_without_rendering(state, output)?;
        return Ok(AutoApprovalFlow::Handled);
    }
    // The verdict was delivered (or no provider reply is owed) and the run
    // continues: keep processing the rest of the batch so later approvals are
    // not orphaned into the generic unhomed-control drain (#10316 review).
    Ok(AutoApprovalFlow::Continue)
}
