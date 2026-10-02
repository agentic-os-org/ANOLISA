use crate::approval::approved_tool::request_is_executable_bash_tool;
use crate::approval::broker::{
    approval_execution_metadata, classify_approval_outcome, ApprovalExecutionMetadata,
    ApprovalOutcome, ApprovalOutcomeInput,
};
use crate::approval::handoff::{
    fallback_bash_execution_path, shell_handoff_command_from_request, trust_key_from_command,
    ApprovedBashExecutionPath,
};
use crate::approval::journal::{approval_audit_input, approval_journal_entry};
use crate::approval::provider::provider_approval_status;
use crate::approval::requests::reconcile_staged_unresolved_entry;
use crate::runtime::prelude::*;

pub(crate) struct AppliedApprovalDecision {
    pub(crate) request: RuntimeApprovalRequest,
    pub(crate) title: MessageId,
    pub(crate) run_approved_tool: bool,
}

pub(crate) fn apply_approval_decision(
    state: &mut InlineState,
    request_index: usize,
    kind: ApprovalCommandKind,
) -> Option<AppliedApprovalDecision> {
    let mut trust_key = None;
    let turn_extension =
        state.approvals.requests[request_index].kind == ApprovalRequestKind::TurnExtension;
    if turn_extension
        && !matches!(
            kind,
            ApprovalCommandKind::Approve | ApprovalCommandKind::Deny | ApprovalCommandKind::Cancel
        )
    {
        return None;
    }
    let (status, title) = match kind {
        ApprovalCommandKind::Approve => {
            let (status, title) =
                approval_status_for_allowed_request(&state.approvals.requests[request_index]);
            if turn_extension {
                (status, MessageId::ApprovalResolutionContinuingTitle)
            } else {
                (status, title)
            }
        }
        ApprovalCommandKind::ApproveTurn => {
            let (status, _) =
                approval_status_for_allowed_request(&state.approvals.requests[request_index]);
            (status, MessageId::ApprovalResolutionTurnApprovedTitle)
        }
        ApprovalCommandKind::AlwaysTrust => {
            let (status, _) =
                approval_status_for_allowed_request(&state.approvals.requests[request_index]);
            // Defense in depth for high-risk requests (issue #2064): the
            // panel never offers AlwaysTrust for them, but a stale or
            // replayed CardAlwaysTrust event must still not mint a session
            // trust key — downgrade to a one-shot approval instead.
            if state.approvals.requests[request_index].risk == "high" {
                (status, MessageId::ApprovalResolutionApprovedTitle)
            } else {
                trust_key =
                    trust_key_from_command(&state.approvals.requests[request_index].preview);
                (status, MessageId::ApprovalResolutionTrustedTitle)
            }
        }
        ApprovalCommandKind::Deny => {
            let title = if turn_extension {
                MessageId::ApprovalResolutionStoppedTitle
            } else {
                MessageId::ApprovalResolutionDeniedTitle
            };
            (ApprovalRequestStatus::Denied, title)
        }
        ApprovalCommandKind::Cancel => {
            let title = if turn_extension {
                MessageId::ApprovalResolutionStoppedTitle
            } else {
                MessageId::ApprovalResolutionCancelledTitle
            };
            (ApprovalRequestStatus::Cancelled, title)
        }
        ApprovalCommandKind::Details => return None,
        ApprovalCommandKind::SendToShell => return None,
    };
    let grant_turn_consent = kind == ApprovalCommandKind::ApproveTurn;
    let actor = if grant_turn_consent {
        "user_batch"
    } else {
        "user"
    };

    finalize_approval_decision(
        state,
        request_index,
        status,
        title,
        trust_key,
        grant_turn_consent,
        actor,
    )
}

/// Batch-consent sweep resolution: same pipeline as a user decision, but
/// journalled as `batch_consent` and never (re-)granting turn consent.
pub(crate) fn apply_batch_consent_decision(
    state: &mut InlineState,
    request_index: usize,
) -> Option<AppliedApprovalDecision> {
    let (status, _) = approval_status_for_allowed_request(&state.approvals.requests[request_index]);
    finalize_approval_decision(
        state,
        request_index,
        status,
        MessageId::ApprovalResolutionTurnApprovedTitle,
        None,
        false,
        "batch_consent",
    )
}

/// Turn-scope batch consent covers pending, non-hook, non-high-risk
/// executable bash tool requests from the consented run (issue #1773).
///
/// Contract: `run_id` must be a run the user has explicitly consented to
/// (`ApprovalTrustState::run_batch_consent`, or the run of the just-approved
/// `ApproveTurn` request). The predicate deliberately does not read the
/// live consent state: the user-path sweep must keep covering the
/// consented run even when delivering an entry stops the run (which
/// clears the consent). Callers must never pass an unconsented run id.
pub(crate) fn batch_consent_covers_request(request: &RuntimeApprovalRequest, run_id: &str) -> bool {
    request.status == ApprovalRequestStatus::Pending
        && request_is_executable_bash_tool(request)
        && !request.hook_requires_approval
        && request.risk != "high"
        && request.run_id == run_id
}

fn finalize_approval_decision(
    state: &mut InlineState,
    request_index: usize,
    status: ApprovalRequestStatus,
    mut title: MessageId,
    trust_key: Option<String>,
    grant_turn_consent: bool,
    actor: &'static str,
) -> Option<AppliedApprovalDecision> {
    state.approvals.requests[request_index].status = status;
    let outcome = approval_outcome_for_request(state, &state.approvals.requests[request_index]);
    let metadata = approval_execution_metadata(
        outcome,
        provider_approval_status(status),
        request_is_executable_bash_tool(&state.approvals.requests[request_index]),
    );
    apply_approval_execution_metadata(&mut state.approvals.requests[request_index], metadata);
    let mut request = state.approvals.requests[request_index].clone();
    let audit_result = state
        .audit
        .as_mut()
        .map(|audit| audit.record_approval_resolved(approval_audit_input(&request)))
        .transpose();
    if audit_result.is_err() && request.status == ApprovalRequestStatus::Approved {
        request.status = ApprovalRequestStatus::Blocked;
        request.execution_path = Some("blocked_audit_required");
        state.approvals.requests[request_index] = request.clone();
    }
    if request.status == ApprovalRequestStatus::Approved
        && outcome == ApprovalOutcome::ForegroundShellHandoff
    {
        let host_audit_result = state
            .audit
            .as_mut()
            .map(|audit| {
                audit.authorize_host_execution(
                    approval_audit_input(&request),
                    "shell_foreground_handoff",
                )
            })
            .transpose();
        if host_audit_result.is_err() {
            request.status = ApprovalRequestStatus::Blocked;
            request.execution_path = Some("blocked_audit_required");
            state.approvals.requests[request_index] = request.clone();
        }
    }
    // Keep the receipt title aligned with the final status, including
    // validation blocks that occur before the audit checks above.
    if request.status == ApprovalRequestStatus::Blocked {
        title = MessageId::ApprovalResolutionBlockedTitle;
    }
    if request.status == ApprovalRequestStatus::Approved {
        if let Some(key) = trust_key {
            state.control.trust.trust_session_command(key);
        }
        if grant_turn_consent {
            state
                .control
                .trust
                .grant_run_batch_consent(request.run_id.clone());
        }
    }
    // A late verdict on a grace-released staged call converts the
    // provisional staged_unresolved entry rather than adding a second,
    // contradictory terminal record for the same tool_use_id (#2156).
    let reconciled = request.tool_use_id.as_deref().is_some_and(|tool_id| {
        reconcile_staged_unresolved_entry(
            state,
            &request.run_id,
            tool_id,
            request.status,
            actor,
            "staged_resolved_late_verdict",
        )
    });
    if !reconciled {
        state
            .approvals
            .journal
            .push(approval_journal_entry(&request, actor));
    }
    let run_approved_tool = request.status == ApprovalRequestStatus::Approved
        && request_is_executable_bash_tool(&request);

    Some(AppliedApprovalDecision {
        request,
        title,
        run_approved_tool,
    })
}

fn apply_approval_execution_metadata(
    request: &mut RuntimeApprovalRequest,
    metadata: ApprovalExecutionMetadata,
) {
    request.execution_path = if request.kind == ApprovalRequestKind::TurnExtension {
        match request.status {
            ApprovalRequestStatus::Approved => Some("provider_session_continuation"),
            ApprovalRequestStatus::Denied | ApprovalRequestStatus::Cancelled => {
                Some("not_executed_stopped")
            }
            _ => metadata.execution_path,
        }
    } else {
        metadata.execution_path
    };
    request.redaction_status = metadata.redaction_status;
}

fn approval_status_for_allowed_request(
    request: &RuntimeApprovalRequest,
) -> (ApprovalRequestStatus, MessageId) {
    if request_is_executable_bash_tool(request) {
        let command = match shell_handoff_command_from_request(request) {
            Ok(command) => command,
            Err(_) => {
                return (
                    ApprovalRequestStatus::Blocked,
                    MessageId::ApprovalResolutionBlockedTitle,
                )
            }
        };
        if fallback_bash_execution_path(&command) == ApprovedBashExecutionPath::Blocked {
            return (
                ApprovalRequestStatus::Blocked,
                MessageId::ApprovalResolutionBlockedTitle,
            );
        }
    }

    (
        ApprovalRequestStatus::Approved,
        MessageId::ApprovalResolutionApprovedTitle,
    )
}

pub(crate) fn active_provider_supports_host_executed_shell(state: &InlineState) -> bool {
    state.agent_run.active.as_ref().is_some_and(|run| {
        run.handle
            .control_capabilities()
            .can_handle_host_executed_shell_tool_result
    })
}

pub(crate) fn request_can_receive_host_executed_result(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    request_is_executable_bash_tool(request)
        && request.provider_shell_request_kind.is_control_permission()
        && request.request_id.is_some()
        && request.tool_use_id.is_some()
        && state
            .agent_run
            .active
            .as_ref()
            .is_some_and(|run| run.request.id == request.run_id)
        && active_provider_supports_host_executed_shell(state)
}

pub(crate) fn approval_outcome_for_request(
    _state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> ApprovalOutcome {
    classify_approval_outcome(ApprovalOutcomeInput {
        approved: request.status == ApprovalRequestStatus::Approved,
        shell_tool: request_is_executable_bash_tool(request),
        provider_request: request.provider_shell_request_kind.is_control_permission(),
    })
}

pub(crate) fn should_send_approval_resolution_to_agent(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    request.kind != ApprovalRequestKind::TurnExtension
        && matches!(
            request.status,
            ApprovalRequestStatus::Denied | ApprovalRequestStatus::Cancelled
        )
        && !state
            .approvals
            .requests
            .iter()
            .any(|request| request.status == ApprovalRequestStatus::Pending)
}

pub(crate) fn approval_resolution_agent_request(request: &RuntimeApprovalRequest) -> AgentRequest {
    let decision = match request.status {
        ApprovalRequestStatus::Denied => "denied by user",
        ApprovalRequestStatus::Cancelled => "cancelled by user",
        ApprovalRequestStatus::Blocked => "blocked by cosh-shell",
        ApprovalRequestStatus::Pending => "pending",
        ApprovalRequestStatus::Approved => "approved",
    };
    let block_id = format!("approval-resolution-{}", request.id);
    let user_input = format!(
        "Approval result for request {id}\n\
         Tool: {subject}\n\
         Command: {command}\n\
         Decision: {decision}\n\
         Status: not_executed\n\
         No command ran.\n\
         Continue the same Agent session using this approval result. Do not claim the command executed. Provide a safe next step or ask for another approval if more evidence is required.",
        id = request.id,
        subject = request.subject,
        command = request.preview,
        decision = decision,
    );

    let mut agent_request = AgentRequest {
        id: format!("agent-request-{block_id}"),
        session_id: request.session_id.clone(),
        command_block: CommandBlock {
            id: block_id,
            session_id: request.session_id.clone(),
            command: user_input.clone(),
            origin: Default::default(),
            cwd: request.cwd.clone(),
            end_cwd: request.cwd.clone(),
            started_at_ms: 0,
            ended_at_ms: 0,
            duration_ms: 0,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some(user_input),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    crate::types::set_request_context_binding(
        &mut agent_request,
        AgentContextBinding::ControlProtocolEvidence,
    );
    agent_request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_without_active_host_executed_provider_is_not_deliverable() {
        let state = InlineState::default();
        let request = shell_permission_request(Some("ctrl-1"), Some("toolu-1"));

        assert!(!request_can_receive_host_executed_result(&state, &request));
        assert_eq!(
            approval_outcome_for_request(&state, &request),
            ApprovalOutcome::ForegroundShellHandoff
        );
    }

    #[test]
    fn request_missing_control_ids_is_not_host_executed_deliverable() {
        let state = InlineState::default();

        for request in [
            shell_permission_request(None, Some("toolu-1")),
            shell_permission_request(Some("ctrl-1"), None),
        ] {
            assert!(!request_can_receive_host_executed_result(&state, &request));
        }
    }

    #[test]
    fn streamed_tool_fallback_with_ids_is_not_provider_control_owned() {
        let state = InlineState::default();
        let mut request = shell_permission_request(Some("ctrl-1"), Some("toolu-1"));
        request.provider_shell_request_kind = ProviderShellRequestKind::StreamedToolCallFallback;

        assert!(!request_can_receive_host_executed_result(&state, &request));
        assert_eq!(
            approval_outcome_for_request(&state, &request),
            ApprovalOutcome::ForegroundShellHandoff
        );
    }

    #[test]
    fn approval_resolution_request_marks_command_not_executed() {
        for (status, decision) in [
            (ApprovalRequestStatus::Denied, "denied by user"),
            (ApprovalRequestStatus::Cancelled, "cancelled by user"),
            (ApprovalRequestStatus::Blocked, "blocked by cosh-shell"),
        ] {
            let mut request = shell_permission_request(Some("ctrl-1"), Some("toolu-1"));
            request.status = status;

            let agent_request = approval_resolution_agent_request(&request);
            let input = agent_request.user_input.expect("approval result input");

            assert!(input.contains(&format!("Decision: {decision}")), "{input}");
            assert!(input.contains("Status: not_executed"), "{input}");
            assert!(input.contains("No command ran."), "{input}");
            assert_eq!(agent_request.command_block.output.terminal_output_ref, None);
            assert_eq!(agent_request.command_block.output.terminal_output_bytes, 0);
        }
    }

    fn shell_permission_request(
        request_id: Option<&str>,
        tool_use_id: Option<&str>,
    ) -> RuntimeApprovalRequest {
        RuntimeApprovalRequest {
            id: "req-1".to_string(),
            audit_ref: None,
            run_id: "run-1".to_string(),
            origin: AgentRunOrigin::Standard,
            session_id: "sess-1".to_string(),
            cwd: "/tmp".to_string(),
            source: "control-protocol",
            provider_shell_request_kind: if request_id.is_some() && tool_use_id.is_some() {
                ProviderShellRequestKind::ControlPermission
            } else {
                ProviderShellRequestKind::StreamedToolCallFallback
            },
            kind: ApprovalRequestKind::Tool,
            subject: "run_shell_command".to_string(),
            preview: "$ echo ok".to_string(),
            risk: "medium",
            request_id: request_id.map(str::to_string),
            tool_use_id: tool_use_id.map(str::to_string),
            tool_input: Some(serde_json::json!({ "command": "echo ok" })),
            original_user_request: None,
            status: ApprovalRequestStatus::Approved,
            execution_path: Some("provider_control_protocol"),
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            hook_requires_approval: false,
            hook_warnings: Vec::new(),
        }
    }
}
