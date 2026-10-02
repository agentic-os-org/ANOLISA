// Owner: shell_host handoff claim (#2142). Decides which preexec-reported
// command block belongs to a pending approved shell handoff, so closure works
// on identity instead of the marker's possibly rewritten command text.
use crate::types::{CommandOrigin, ShellCommandAuditIdentity, ShellHandoffRequest};

/// The single pending-handoff slot registered right before the handoff bytes
/// are written to the PTY, consumed by the claiming preexec marker.
#[derive(Debug, Clone)]
pub(super) struct PendingCommandOrigin {
    pub(super) command: String,
    pub(super) origin: CommandOrigin,
    pub(super) audit_identity: ShellCommandAuditIdentity,
    /// One-time claim token staged with the handoff request (#2142). A marker
    /// echoing it back claims this slot regardless of the reported command
    /// text; markers without it can only claim by exact text.
    pub(super) handoff_token: String,
}

pub(super) fn pending_origin_for_request(request: &ShellHandoffRequest) -> PendingCommandOrigin {
    PendingCommandOrigin {
        command: request.command.clone(),
        origin: command_origin_from_handoff_request(request),
        audit_identity: ShellCommandAuditIdentity {
            run_id: request.run_id.clone(),
            request_id: request.request_id.clone(),
            tool_use_id: request.tool_use_id.clone(),
            handoff_token: (!request.token.is_empty()).then(|| request.token.clone()),
        },
        handoff_token: request.token.clone(),
    }
}

/// Resolves the origin of a preexec-reported command against the pending
/// handoff slot (#2142).
///
/// Claim rules — an explicit token is exclusive:
/// 1. marker carries the staged token → claim, whatever text it reports
///    (redaction-proof);
/// 2. marker carries some other token → stale or forged claim; never adopt
///    the handoff identity, not even on identical text (identical text plus
///    a wrong token is exactly what a replayed marker for the same command
///    line looks like), and leave the slot alive;
/// 3. no token, exact text match → claim (marker scripts predating the token
///    sidecar);
/// 4. no token, no text match → ordinary user input; the slot survives so an
///    unrelated command racing ahead cannot burn it (S3).
pub(super) fn claim_pending_command_origin(
    slot: &mut Option<PendingCommandOrigin>,
    command: &str,
    marker_handoff: Option<&str>,
) -> (CommandOrigin, Option<ShellCommandAuditIdentity>) {
    let Some(pending) = slot.take() else {
        return (CommandOrigin::UserInteractive, None);
    };
    match marker_handoff {
        Some(token) if !pending.handoff_token.is_empty() && token == pending.handoff_token => {
            (pending.origin, Some(pending.audit_identity))
        }
        Some(_) => {
            *slot = Some(pending);
            (CommandOrigin::Unknown, None)
        }
        None if pending.command == command => (pending.origin, Some(pending.audit_identity)),
        None => {
            *slot = Some(pending);
            (CommandOrigin::UserInteractive, None)
        }
    }
}

pub(super) fn command_origin_from_handoff_request(request: &ShellHandoffRequest) -> CommandOrigin {
    match request.source.as_str() {
        "send_to_shell" => CommandOrigin::UserSendToShell,
        "user_analysis_action" => CommandOrigin::UserAnalysisAction,
        "approved_provider_shell_tool" => CommandOrigin::ProviderTool,
        "approved_fallback" => CommandOrigin::AgentHandoff,
        "validation" => CommandOrigin::ShellInternal,
        _ => CommandOrigin::Unknown,
    }
}
