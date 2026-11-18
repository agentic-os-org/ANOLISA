//! Permission and foreground authority surface for the Shell ACP client.
//!
//! The adapter never invents consent: these types carry the caller's decision
//! and the fail-closed default, kept apart from connection lifecycle.

use std::sync::Arc;

use agent_client_protocol::v1::schema::{
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SelectedPermissionOutcome,
};

use super::host_shell::{AcpHostExecuteOutcome, AcpHostExecuteRequest, AcpHostRefusal};

/// Decision for one `session/request_permission` reverse call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpPermissionDecision {
    /// Selects an option that must exist in the request.
    Select(String),
    /// Reports that the turn was cancelled before a decision was reached.
    Cancel,
}

/// Caller-supplied permission authority; the adapter never invents consent.
pub type AcpPermissionPolicy =
    Arc<dyn Fn(&RequestPermissionRequest) -> AcpPermissionDecision + Send + Sync>;

/// The Shell's foreground authority for one action: local review and the
/// single dispatch are one round trip, because the runtime's approval card,
/// PTY handoff and evidence collection already form one flow.
pub type AcpHostAuthority = Arc<
    dyn Fn(&AcpHostExecuteRequest) -> Result<AcpHostExecuteOutcome, AcpHostRefusal> + Send + Sync,
>;

pub(super) fn permission_outcome(
    request: &RequestPermissionRequest,
    decision: AcpPermissionDecision,
) -> RequestPermissionOutcome {
    match decision {
        AcpPermissionDecision::Cancel => RequestPermissionOutcome::Cancelled,
        AcpPermissionDecision::Select(option_id) => {
            let known = request
                .options
                .iter()
                .any(|option| option.option_id.0.as_ref() == option_id);
            if known {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
            } else {
                RequestPermissionOutcome::Cancelled
            }
        }
    }
}
/// Fail-closed default: pick the agent's own once-only reject option.
///
/// Remembered kinds are never chosen in either direction: selecting
/// `reject_always` would store a denial the user never decided, which is the
/// same invented policy as selecting an allow. Without a once-only reject the
/// answer is cancelled, matching the gateway's once-only permission proxy.
pub(super) fn reject_by_default(request: &RequestPermissionRequest) -> AcpPermissionDecision {
    request
        .options
        .iter()
        .find(|option| option.kind == PermissionOptionKind::RejectOnce)
        .map_or(AcpPermissionDecision::Cancel, |option| {
            AcpPermissionDecision::Select(option.option_id.0.to_string())
        })
}
