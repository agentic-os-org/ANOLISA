use crate::approval::broker::{provider_deny_response, ProviderResponseInput};
use crate::runtime::prelude::*;

/// What refusing a governed request means for the calling batch.
///
/// Owner of the refusal-delivery contract shared by the shell-request
/// policy and plan mode (#1776). Living in `approval/` keeps both
/// `agent/` rendering passes and `approval/plan_mode.rs` on the same
/// one-way `agent -> approval` dependency edge (the D16 turn-extension
/// exception stays the only registered reverse edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellRequestPolicyHandling {
    Continue,
    Refused,
    AwaitingTerminalSweep,
}

/// Refuses one tool request on policy grounds (shell policy refusals and
/// plan-mode denials, #1776): answer the owning provider when there is a
/// control request to answer, then record the refusal.
///
/// A successfully delivered control refusal gets a terminal home so the tail
/// pass cannot resurface it. If delivery is unavailable, the request remains
/// unhomed for the active or detached terminal sweep. Streamed fallbacks have
/// no provider response and are recorded as shell-local refusals.
pub(crate) fn deny_policy_refused_request(
    state: &mut InlineState,
    request: &RuntimeApprovalRequest,
    reason: &str,
) -> ShellRequestPolicyHandling {
    let Some(request_id) = request.request_id.as_deref() else {
        record_policy_refused_request(state, request.clone());
        return ShellRequestPolicyHandling::Refused;
    };
    let Some(active_run) = state.agent_run.active.as_ref() else {
        return ShellRequestPolicyHandling::AwaitingTerminalSweep;
    };
    let response = provider_deny_response(
        ProviderResponseInput {
            request_id,
            tool_use_id: request.tool_use_id.as_deref(),
            tool_input: request.tool_input.as_ref(),
        },
        reason.to_string(),
    );
    if active_run.handle.respond_approval(response).is_err() {
        return ShellRequestPolicyHandling::AwaitingTerminalSweep;
    }
    record_policy_refused_request(state, request.clone());
    ShellRequestPolicyHandling::Refused
}
