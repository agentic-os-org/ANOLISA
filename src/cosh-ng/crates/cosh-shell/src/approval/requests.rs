use crate::approval::journal::{approval_audit_input, approval_journal_entry};
use crate::runtime::prelude::*;
use crate::tools::display::{presentation_for_tool, ToolPresentation};

pub(crate) fn record_approval_requests(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    run_request: Option<&AgentRequest>,
    origin: AgentRunOrigin,
    ignore_tool_calls: bool,
) -> Vec<String> {
    let mut ids = Vec::new();
    let session_id = run_request
        .map(|request| request.session_id.clone())
        .unwrap_or_else(|| "unknown-session".to_string());
    let cwd = run_request
        .map(|request| request.command_block.cwd.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let original_user_request = original_user_request(run_request);
    for event in governed_events {
        let request = approval_request_from_event(
            state,
            event,
            &session_id,
            &cwd,
            original_user_request.as_deref(),
            origin,
            ignore_tool_calls,
        );

        if let Some(mut request) = request {
            if state
                .approvals
                .requests
                .iter()
                .any(|existing| same_approval_request_identity(existing, &request))
            {
                continue;
            }
            // Associate pending hook notifications by tool_use_id
            if let Some(ref tool_use_id) = request.tool_use_id {
                if let Some(active_run) = state.agent_run.active.as_mut() {
                    let mut warnings = Vec::new();
                    active_run.pending_hook_notifications.retain(|n| {
                        if n.tool_use_id.as_deref() == Some(tool_use_id) {
                            warnings.push(crate::runtime::approval_state::HookWarning {
                                hook_name: n.hook_name.clone(),
                                message: n.message.clone(),
                                decision: n.decision.clone(),
                            });
                            false
                        } else {
                            true
                        }
                    });
                    request.hook_warnings = warnings;
                }
            }
            let core_owned = state
                .agent_run
                .active
                .as_ref()
                .is_some_and(|run| run.provider_name == "cosh-core")
                && request.provider_shell_request_kind.is_control_permission();
            if !core_owned {
                if let Some(audit) = state.audit.as_mut() {
                    request.audit_ref =
                        audit.record_approval_requested(approval_audit_input(&request));
                }
            }
            ids.push(request.id.clone());
            state.approvals.requests.push(request);
        }
    }
    ids
}

fn same_approval_request_identity(
    existing: &RuntimeApprovalRequest,
    request: &RuntimeApprovalRequest,
) -> bool {
    if existing.run_id != request.run_id {
        return false;
    }
    // The control-protocol request id is the primary identity: a follow-up
    // approval (e.g. a sandbox-bypass retry after post_tool_use_failure)
    // legitimately reuses the tool_use_id of the failed tool call, so the
    // tool_use_id alone must never collapse two distinct requests (#1920).
    // parse_control_request rejects can_use_tool payloads without a
    // request_id, so a request with request_id=None is always a locally
    // recorded fallback/host entry, never a control-protocol approval.
    match (&existing.request_id, &request.request_id) {
        (Some(existing_id), Some(request_id)) => return existing_id == request_id,
        (Some(_), None) | (None, Some(_)) => return false,
        (None, None) => {}
    }
    if let (Some(existing_id), Some(request_id)) = (&existing.tool_use_id, &request.tool_use_id) {
        return existing_id == request_id;
    }
    existing.kind == request.kind
        && existing.subject == request.subject
        && existing.preview == request.preview
}

pub(crate) fn approval_request_from_governed_event(
    state: &InlineState,
    event: &GovernedEvent,
    run_request: Option<&AgentRequest>,
    origin: AgentRunOrigin,
    ignore_tool_calls: bool,
) -> Option<RuntimeApprovalRequest> {
    let session_id = run_request
        .map(|request| request.session_id.clone())
        .unwrap_or_else(|| "unknown-session".to_string());
    let cwd = run_request
        .map(|request| request.command_block.cwd.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let original_user_request = original_user_request(run_request);
    approval_request_from_event(
        state,
        event,
        &session_id,
        &cwd,
        original_user_request.as_deref(),
        origin,
        ignore_tool_calls,
    )
}

fn approval_request_from_event(
    state: &InlineState,
    event: &GovernedEvent,
    session_id: &str,
    cwd: &str,
    original_user_request: Option<&str>,
    origin: AgentRunOrigin,
    ignore_tool_calls: bool,
) -> Option<RuntimeApprovalRequest> {
    if event.policy_decision != GovernancePolicyDecision::NeedsUserApproval {
        return None;
    }

    match &event.event {
        AgentEvent::ToolCall {
            run_id,
            tool_id,
            name,
            input,
        } => {
            if ignore_tool_calls {
                return None;
            }
            let presentation = presentation_for_tool(name, input);
            let (label, preview) = approval_tool_label_preview(&presentation);
            if is_readonly_builtin_tool_name(&label) {
                return None;
            }
            let assessment = shell_tool_assessment_from_preview(&label, &preview);
            let risk = assessment
                .as_ref()
                .map(|assessment| assessment.impact.legacy_risk())
                .unwrap_or("medium");
            Some(RuntimeApprovalRequest {
                id: next_approval_id(state),
                audit_ref: None,
                run_id: run_id.clone(),
                origin,
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
                source: "agent",
                provider_shell_request_kind: ProviderShellRequestKind::StreamedToolCallFallback,
                kind: ApprovalRequestKind::Tool,
                subject: label,
                preview,
                risk,
                request_id: None,
                tool_use_id: tool_id.clone(),
                tool_input: None,
                original_user_request: original_user_request.map(ToString::to_string),
                status: ApprovalRequestStatus::Pending,
                execution_path: None,
                command_block_id: None,
                redaction_status: None,
                assessment: assessment.as_ref().map(runtime_assessment_summary),
                hook_requires_approval: false,
                hook_warnings: Vec::new(),
            })
        }
        AgentEvent::Action { run_id, command } => {
            let assessment = shell_command_assessment(command);
            Some(RuntimeApprovalRequest {
                id: next_approval_id(state),
                audit_ref: None,
                run_id: run_id.clone(),
                origin,
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
                source: "agent",
                provider_shell_request_kind: ProviderShellRequestKind::LocalApproval,
                kind: ApprovalRequestKind::ShellCommand,
                subject: "shell command".to_string(),
                preview: command.clone(),
                risk: assessment.impact.legacy_risk(),
                request_id: None,
                tool_use_id: None,
                tool_input: None,
                original_user_request: original_user_request.map(ToString::to_string),
                status: ApprovalRequestStatus::Pending,
                execution_path: None,
                command_block_id: None,
                redaction_status: None,
                assessment: Some(runtime_assessment_summary(&assessment)),
                hook_requires_approval: false,
                hook_warnings: Vec::new(),
            })
        }
        AgentEvent::ToolPermissionRequest {
            run_id,
            request_id,
            tool_name,
            tool_input,
            tool_use_id,
            hook_requires_approval,
            audit_ref,
        } => {
            // #1940: a control approval whose run id does not match the
            // active run was already denied at the registration door
            // (agent/poll.rs). Never let it resurface downstream — as a
            // pending card the user could approve, or as an auto-approval —
            // because either path would send a second, contradictory
            // response for a request the shell already terminated.
            // When no run is active the ownership cannot be disproven here:
            // a sandbox-bypass follow-up legitimately arrives after the
            // fallback execution cleared the active run, so the request
            // passes through and the respond path falls back to the
            // owner-unavailable recovery if the owner is truly gone.
            let foreign_to_active_run = state
                .agent_run
                .active
                .as_ref()
                .is_some_and(|run| &run.request.id != run_id);
            if foreign_to_active_run {
                return None;
            }
            let input_str = serde_json::to_string(tool_input).unwrap_or_default();
            let presentation = presentation_for_tool(tool_name, &input_str);
            let (label, preview) = approval_tool_label_preview(&presentation);
            let assessment = provider_tool_permission_assessment(tool_name, tool_input);
            let risk = assessment
                .as_ref()
                .map(|assessment| assessment.impact.legacy_risk())
                .unwrap_or("medium");
            let tool_use_id = non_empty_tool_use_id(tool_use_id);
            Some(RuntimeApprovalRequest {
                id: next_approval_id(state),
                audit_ref: audit_ref.clone(),
                run_id: run_id.clone(),
                origin,
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
                source: "control-protocol",
                provider_shell_request_kind: ProviderShellRequestKind::ControlPermission,
                kind: ApprovalRequestKind::Tool,
                subject: label,
                preview,
                risk,
                request_id: Some(request_id.clone()),
                tool_use_id,
                tool_input: Some(tool_input.clone()),
                original_user_request: original_user_request.map(ToString::to_string),
                status: ApprovalRequestStatus::Pending,
                execution_path: Some("provider_control_protocol"),
                command_block_id: None,
                redaction_status: None,
                assessment: assessment.as_ref().map(runtime_assessment_summary),
                hook_requires_approval: *hook_requires_approval,
                hook_warnings: Vec::new(),
            })
        }
        _ => None,
    }
}

fn non_empty_tool_use_id(tool_use_id: &str) -> Option<String> {
    (!tool_use_id.trim().is_empty()).then(|| tool_use_id.to_string())
}

fn approval_tool_label_preview(presentation: &ToolPresentation) -> (String, String) {
    (
        presentation.canonical_name.clone(),
        presentation.preview.clone(),
    )
}

fn original_user_request(run_request: Option<&AgentRequest>) -> Option<String> {
    let request = run_request?;
    request
        .user_input
        .as_ref()
        .filter(|input| !input.trim().is_empty())
        .cloned()
        .or_else(|| {
            (!request.command_block.command.trim().is_empty())
                .then(|| request.command_block.command.clone())
        })
}

pub(crate) fn record_auto_approved_request(
    state: &mut InlineState,
    mut request: RuntimeApprovalRequest,
) -> RuntimeApprovalRequest {
    request.status = ApprovalRequestStatus::Approved;
    if request.execution_path.is_none() && request.request_id.is_some() {
        request.execution_path = Some("provider_control_protocol");
    }
    let core_owned = state
        .agent_run
        .active
        .as_ref()
        .is_some_and(|run| run.provider_name == "cosh-core")
        && request.provider_shell_request_kind.is_control_permission();
    if !core_owned {
        if let Some(audit) = state.audit.as_mut() {
            request.audit_ref = audit.record_approval_requested(approval_audit_input(&request));
            if audit
                .record_approval_resolved(approval_audit_input(&request))
                .is_err()
            {
                request.status = ApprovalRequestStatus::Blocked;
                request.execution_path = Some("blocked_audit_required");
            }
        }
    }
    state.approvals.requests.push(request.clone());
    // A late control-channel verdict converts the provisional
    // staged_unresolved entry into the final approval instead of doubling
    // the journal with a contradictory Blocked+Approved pair (#2156).
    let reconciled = request.tool_use_id.as_deref().is_some_and(|tool_id| {
        reconcile_staged_unresolved_entry(
            state,
            &request.run_id,
            tool_id,
            ApprovalRequestStatus::Approved,
            "agent-auto",
            "staged_resolved_late_verdict",
        )
    });
    if !reconciled {
        state
            .approvals
            .journal
            .push(approval_journal_entry(&request, "agent-auto"));
    }
    request
}

/// Records a shell request the shell-request policy refused (#2639).
///
/// The refusal is already on the wire (or had no provider to reach), so the
/// entry exists to give the request a terminal home: the tail record pass
/// dedups on it instead of surfacing a card for a refused command, and
/// `mark_responded` settles the #1940 lifecycle ledger so the batch drain
/// cannot deny the same control request a second time.
pub(crate) fn record_policy_refused_request(
    state: &mut InlineState,
    mut request: RuntimeApprovalRequest,
) {
    request.status = ApprovalRequestStatus::Denied;
    request.execution_path = Some(if request.request_id.is_some() {
        "not_executed_shell_request_policy"
    } else {
        // Streamed fallbacks have no provider lifecycle entry. This local
        // home only prevents the tail pass from resurfacing a refused card.
        "not_executed_shell_request_policy_local"
    });
    if let Some(request_id) = request.request_id.as_deref() {
        state
            .control
            .approval_ledger_mut()
            .mark_responded(&request.run_id, request_id);
    }
    state
        .approvals
        .journal
        .push(approval_journal_entry(&request, "cosh-shell"));
    state.approvals.requests.push(request);
}

pub(crate) fn record_deferred_fallback_request(
    state: &mut InlineState,
    mut request: RuntimeApprovalRequest,
) -> RuntimeApprovalRequest {
    request.status = ApprovalRequestStatus::Blocked;
    if request.execution_path.is_none() {
        request.execution_path = Some("deferred_no_foreground_injection");
    }
    state.approvals.requests.push(request.clone());
    state
        .approvals
        .journal
        .push(approval_journal_entry(&request, "cosh-shell"));
    request
}

/// Journals a grace-released cosh-core ToolCall whose core verdict never
/// became visible. M3 (#2067): such a call must not execute and must not be
/// auto-approved — the journal entry is the audit trail, keyed
/// `staged_unresolved` so a protocol desync stays distinguishable from a
/// user or policy decision.
pub(crate) fn record_staged_unresolved_request(
    state: &mut InlineState,
    mut request: RuntimeApprovalRequest,
) {
    // The journal source is `cosh-shell` because the protocol desync is
    // detected shell-side, without any core verdict on the wire.
    request.status = ApprovalRequestStatus::Blocked;
    request.execution_path = Some("staged_unresolved");
    state
        .approvals
        .journal
        .push(approval_journal_entry(&request, "cosh-shell"));
}

/// A staged ToolCall whose hook verdict arrived inside the staging window
/// as Block: the core released the provider-native error result, so the
/// call never executed. Journal the rejection — never an auto-approval —
/// so the audit trail reflects the hook verdict instead of claiming an
/// approved provider-native execution (#2156). The journal source is
/// `cosh-core` because the block verdict originates from the core's hook
/// system; the shell only records it.
pub(crate) fn record_hook_blocked_staged_request(
    state: &mut InlineState,
    mut request: RuntimeApprovalRequest,
) {
    request.status = ApprovalRequestStatus::Blocked;
    request.execution_path = Some("hook_block");
    state
        .approvals
        .journal
        .push(approval_journal_entry(&request, "cosh-core"));
}

/// A `staged_unresolved` journal entry is provisional, not terminal
/// (#2156): the grace timer fired before the core's verdict, so when the
/// late verdict arrives — an approval resolution through the control
/// channel, or a block-marked provider-native result — the provisional
/// entry converts in place to the final state. Each tool_use_id ends with
/// exactly one terminal journal entry consistent with what actually
/// happened to the command.
pub(crate) fn reconcile_staged_unresolved_entry(
    state: &mut InlineState,
    run_id: &str,
    tool_use_id: &str,
    decision: ApprovalRequestStatus,
    actor: &'static str,
    execution_path: &'static str,
) -> bool {
    let Some(entry) = state.approvals.journal.iter_mut().find(|entry| {
        entry.run_id == run_id
            && entry.tool_use_id.as_deref() == Some(tool_use_id)
            && entry.execution_path == Some("staged_unresolved")
    }) else {
        return false;
    };
    entry.decision = decision;
    entry.actor = actor;
    entry.execution_path = Some(execution_path);
    true
}

pub(crate) fn refresh_shell_request_assessment(
    request: &mut RuntimeApprovalRequest,
    policy: AssessmentPolicy,
) -> Option<CommandAssessment> {
    if !is_shell_tool_name(&request.subject) {
        return None;
    }
    let command = request
        .preview
        .strip_prefix("$ ")
        .unwrap_or(&request.preview)
        .trim();
    let assessment = if command.is_empty() {
        blocked_shell_binding_assessment(policy.source, command, "unsafe-binding")
    } else {
        assess_shell_command(command, policy)
    };
    request.risk = assessment.impact.legacy_risk();
    request.assessment = Some(runtime_assessment_summary(&assessment));
    Some(assessment)
}

fn next_approval_id(state: &InlineState) -> String {
    state.approvals.next_request_id()
}

fn shell_tool_assessment_from_preview(subject: &str, preview: &str) -> Option<CommandAssessment> {
    if !is_shell_tool_name(subject) {
        return None;
    }
    let command = preview.strip_prefix("$ ").unwrap_or(preview).trim();
    if command.is_empty() {
        return Some(blocked_shell_binding_assessment(
            AssessmentSource::ProviderShellTool,
            command,
            "unsafe-binding",
        ));
    }
    Some(shell_command_assessment(command))
}

fn provider_tool_permission_assessment(
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Option<CommandAssessment> {
    if !is_shell_tool_name(tool_name) {
        return None;
    }
    let command = tool_input
        .get("command")
        .or_else(|| tool_input.get("cmd"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim();
    if command.is_empty() {
        return Some(blocked_shell_binding_assessment(
            AssessmentSource::ProviderShellTool,
            command,
            "unsafe-binding",
        ));
    }
    Some(shell_command_assessment(command))
}

fn shell_command_assessment(command: &str) -> CommandAssessment {
    assess_shell_command(
        command,
        AssessmentPolicy::ask(AssessmentSource::ProviderShellTool),
    )
}

pub(super) fn runtime_assessment_summary(
    assessment: &CommandAssessment,
) -> RuntimeCommandAssessmentSummary {
    RuntimeCommandAssessmentSummary {
        impact: assessment.impact.legacy_risk(),
        execution: execution_label(assessment.execution),
        confidence: confidence_label(assessment.confidence),
        primary_reason: assessment.primary_reason(),
        reason_trace: assessment.reason_trace(),
        auto_allow: assessment.auto_allow.map(AutoAllowEvidence::reason_code),
        output_stability: output_stability_label(assessment.output_stability),
        output_exposure: output_exposure_label(assessment.output_exposure),
    }
}

fn execution_label(decision: ExecutionDecision) -> &'static str {
    match decision {
        ExecutionDecision::AutoAllow => "auto-allow",
        ExecutionDecision::AskUser => "ask-user",
        ExecutionDecision::Block => "block",
        ExecutionDecision::ForegroundHandoffRequired => "foreground-handoff-required",
    }
}

fn confidence_label(confidence: AssessmentConfidence) -> &'static str {
    match confidence {
        AssessmentConfidence::High => "high",
        AssessmentConfidence::Medium => "medium",
        AssessmentConfidence::Low => "low",
    }
}

fn output_stability_label(stability: CommandRiskOutputStability) -> &'static str {
    match stability {
        CommandRiskOutputStability::StableSnapshot => "stable-snapshot",
        CommandRiskOutputStability::PotentiallyLarge => "potentially-large",
        CommandRiskOutputStability::Streaming => "streaming",
        CommandRiskOutputStability::UnstableInteractive => "unstable-interactive",
    }
}

fn output_exposure_label(exposure: OutputExposure) -> &'static str {
    match exposure {
        OutputExposure::Normal => "normal",
        OutputExposure::MayContainCommandLine => "may-contain-command-line",
        OutputExposure::MayContainEnvironment => "may-contain-environment",
        OutputExposure::MayContainSecrets => "may-contain-secrets",
    }
}
