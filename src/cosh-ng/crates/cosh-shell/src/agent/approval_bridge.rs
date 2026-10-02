use crate::agent::continuation::run_request_is_analysis_only_continuation;
use crate::agent::run::stop_active_agent_run_without_rendering;
use crate::approval::broker::{
    approval_execution_metadata, classify_approval_outcome, provider_allow_response,
    provider_deny_response, ApprovalExecutionMetadata, ApprovalOutcome, ApprovalOutcomeInput,
    ProviderApprovalStatus, ProviderResponseInput,
};
use crate::approval::handoff::shell_handoff_command_from_request;
use crate::approval::journal::approval_audit_input;
use crate::approval::provider::mark_provider_approval_resolved;
use crate::approval::resolution::request_can_receive_host_executed_result;
use crate::runtime::evidence_delivery::record_readonly_compound_completion;
use crate::runtime::prelude::*;
use crate::tools::command_risk::{RiskImpact, SideEffectClass};
use crate::tools::known_provider_tool;
use crate::tools::readonly_compound::{build_readonly_compound_plan, run_readonly_compound};
use crate::tools::{ReadonlyPipelineConfig, ReadonlyPipelineError, ReadonlyPipelineOutput};

pub(crate) fn render_trusted_tool<W: Write>(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    run_request: Option<&AgentRequest>,
    origin: AgentRunOrigin,
    output: &mut W,
    adapter: &AdapterInstance,
) -> std::io::Result<bool> {
    if state.approval_mode != CoshApprovalMode::Trust {
        return Ok(false);
    }

    let mut blocked_approval_ids = Vec::new();
    for event in governed_events {
        let provider_tool_call_fallback = adapter.capabilities().control_protocol
            && matches!(event.event, AgentEvent::ToolCall { .. });
        let Some(mut request) = approval_request_from_governed_event(
            state,
            event,
            run_request,
            origin,
            adapter.capabilities().control_protocol && !provider_tool_call_fallback,
        ) else {
            continue;
        };
        // Hook ask decisions must never be auto-approved
        if request.hook_requires_approval {
            continue;
        }
        // Trust may bypass approval only for identities in the explicit
        // provider catalog. Unknown provider tools stay pending for an
        // explicit user decision instead of inheriting Trust implicitly.
        if event_tool_name(event).is_some_and(|name| known_provider_tool(name).is_none()) {
            continue;
        }
        if provider_tool_call_fallback && !request_is_executable_bash_tool(&request) {
            continue;
        }
        if provider_tool_call_fallback {
            request.source = "provider-tool-call";
        }
        if provider_tool_call_fallback
            && request_is_executable_bash_tool(&request)
            && provider_native_shell_covered_by_foreground(state, &request)
        {
            mark_provider_native_shell_request_transcript_seen(state, &request);
            continue;
        }
        if provider_tool_call_fallback
            && request_is_executable_bash_tool(&request)
            && provider_native_shell_result_is_hook_block(state, &request)
        {
            // The core verdict arrived inside the staging window as a block:
            // preserve the rejection instead of replaying an approval.
            record_hook_blocked_staged_request(state, request);
            continue;
        }
        if provider_tool_call_fallback
            && request_is_executable_bash_tool(&request)
            && provider_native_shell_result_already_visible(state, &request)
        {
            render_completed_provider_native_shell_request(state, request, output)?;
            continue;
        }
        if provider_tool_call_fallback && active_run_is_cosh_core(state) {
            // M3 (#2067): a grace-released bare ToolCall has no core-visible
            // verdict; executing it here is what bypassed hook blocks. Journal
            // the desync and leave execution to the core-owned channels.
            record_staged_unresolved_request(state, request);
            continue;
        }
        if !provider_tool_call_fallback && defer_fallback_bash_tool(state, request.clone(), output)?
        {
            render_approval_requests(state, &blocked_approval_ids, output)?;
            return Ok(true);
        }
        match handle_shell_request_policy(state, run_request, &request) {
            ShellRequestPolicyHandling::Continue => {}
            ShellRequestPolicyHandling::Refused => continue,
            ShellRequestPolicyHandling::AwaitingTerminalSweep => {
                render_approval_requests(state, &blocked_approval_ids, output)?;
                return Ok(true);
            }
        }
        if trust_mode_blocks_shell_request(&mut request, AssessmentSource::ProviderShellTool) {
            blocked_approval_ids.extend(record_approval_requests(
                state,
                std::slice::from_ref(event),
                run_request,
                origin,
                false,
            ));
            continue;
        }
        let mut request = record_auto_approved_request(state, request);
        if apply_auto_approved_request_outcome(
            state,
            &mut request,
            MessageId::ApprovalResolutionAutoApprovedTitle,
            output,
        )? == AutoApprovalFlow::Handled
        {
            render_approval_requests(state, &blocked_approval_ids, output)?;
            return Ok(true);
        }
    }

    render_approval_requests(state, &blocked_approval_ids, output)?;
    Ok(false)
}

fn event_tool_name(event: &GovernedEvent) -> Option<&str> {
    match &event.event {
        AgentEvent::ToolCall { name, .. } => Some(name),
        AgentEvent::ToolPermissionRequest { tool_name, .. } => Some(tool_name),
        _ => None,
    }
}

/// Only the irrecoverable verdicts stall the auto-approval paths: a
/// confirmed SystemControl assessment, or an unresolvable launcher chain
/// that may hide one. Neither Trust mode nor a session trust key may
/// approve those (#2064). Other High verdicts (shell-syntax risk,
/// ordinary privilege escalation) keep each mode's existing contract, so
/// Trust mode still auto-runs its supported flows.
fn assessment_requires_interactive_approval(assessment: &CommandAssessment) -> bool {
    if assessment.execution == ExecutionDecision::Block {
        return true;
    }
    if assessment.impact != RiskImpact::High {
        return false;
    }
    assessment
        .side_effects
        .contains(&SideEffectClass::SystemControl)
        || assessment.reasons.contains(&"unresolvable-launcher-chain")
}

/// M3 is scoped to the cosh-core driver: claude/qwen also report
/// `control_protocol`, but they have no core-side verdict channel, so their
/// grace-release fallback remains the only trust surface (I4/R4).
fn active_run_is_cosh_core(state: &InlineState) -> bool {
    state
        .agent_run
        .active
        .as_ref()
        .is_some_and(|run| run.provider_name == crate::adapter::COSH_CORE_PROVIDER_NAME)
}

fn trust_mode_blocks_shell_request(
    request: &mut RuntimeApprovalRequest,
    source: AssessmentSource,
) -> bool {
    refresh_shell_request_assessment(request, AssessmentPolicy::ask(source))
        .is_some_and(|assessment| assessment_requires_interactive_approval(&assessment))
}

fn shell_command_requires_interactive_approval(request: &mut RuntimeApprovalRequest) -> bool {
    refresh_shell_request_assessment(
        request,
        AssessmentPolicy::ask(AssessmentSource::ProviderShellTool),
    )
    .is_some_and(|assessment| assessment_requires_interactive_approval(&assessment))
}

pub(crate) fn render_auto_approved_tool<W: Write>(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    run_request: Option<&AgentRequest>,
    origin: AgentRunOrigin,
    output: &mut W,
    adapter: &AdapterInstance,
) -> std::io::Result<bool> {
    if state.approval_mode != CoshApprovalMode::Auto {
        return Ok(false);
    }

    let mut blocked_approval_ids = Vec::new();
    for event in governed_events {
        let provider_tool_call_fallback = adapter.capabilities().control_protocol
            && matches!(event.event, AgentEvent::ToolCall { .. });
        let Some(mut request) = approval_request_from_governed_event(
            state,
            event,
            run_request,
            origin,
            adapter.capabilities().control_protocol && !provider_tool_call_fallback,
        ) else {
            continue;
        };
        // Hook ask decisions must never be auto-approved
        if request.hook_requires_approval {
            continue;
        }
        if provider_tool_call_fallback && !request_is_executable_bash_tool(&request) {
            continue;
        }
        if provider_tool_call_fallback {
            request.source = "provider-tool-call";
        }
        if provider_tool_call_fallback
            && request_is_executable_bash_tool(&request)
            && provider_native_shell_covered_by_foreground(state, &request)
        {
            mark_provider_native_shell_request_transcript_seen(state, &request);
            continue;
        }
        if provider_tool_call_fallback
            && request_is_executable_bash_tool(&request)
            && provider_native_shell_result_is_hook_block(state, &request)
        {
            record_hook_blocked_staged_request(state, request);
            continue;
        }
        if provider_tool_call_fallback
            && request_is_executable_bash_tool(&request)
            && provider_native_shell_result_already_visible(state, &request)
        {
            render_completed_provider_native_shell_request(state, request, output)?;
            continue;
        }
        if provider_tool_call_fallback && active_run_is_cosh_core(state) {
            // M3 (#2067): a grace-released bare ToolCall has no core-visible
            // verdict; executing it here is what bypassed hook blocks. Journal
            // the desync and leave execution to the core-owned channels.
            record_staged_unresolved_request(state, request);
            continue;
        }
        if request_is_readonly_builtin_tool(&request) {
            let mut request = record_auto_approved_request(state, request);
            if apply_auto_approved_request_outcome(
                state,
                &mut request,
                MessageId::ApprovalResolutionAutoApprovedTitle,
                output,
            )? == AutoApprovalFlow::Handled
            {
                return Ok(true);
            }
            continue;
        }
        match handle_shell_request_policy(state, run_request, &request) {
            ShellRequestPolicyHandling::Continue => {}
            ShellRequestPolicyHandling::Refused => continue,
            ShellRequestPolicyHandling::AwaitingTerminalSweep => {
                render_approval_requests(state, &blocked_approval_ids, output)?;
                return Ok(true);
            }
        }

        let raw_cmd = request
            .preview
            .strip_prefix("$ ")
            .unwrap_or(&request.preview);

        let trust_key_match = request_is_executable_bash_tool(&request)
            && command_matches_trust_key(raw_cmd, state.control.trust.session_trusted_commands());
        if trust_key_match && shell_command_requires_interactive_approval(&mut request) {
            // A trust key can never override the high-risk gate: config
            // may preload `reboot` via `trusted_commands`, and a key
            // minted before this guard would otherwise replay (#2064).
            // Leave a pending card; the tail record pass ignores tool
            // calls under control-protocol adapters, so record here.
            blocked_approval_ids.extend(record_approval_requests(
                state,
                std::slice::from_ref(event),
                run_request,
                origin,
                false,
            ));
            continue;
        }
        if trust_key_match {
            if defer_fallback_bash_tool(state, request.clone(), output)? {
                return Ok(true);
            }
            let mut request = record_auto_approved_request(state, request);
            if apply_auto_approved_request_outcome(
                state,
                &mut request,
                MessageId::ApprovalResolutionTrustedTitle,
                output,
            )? == AutoApprovalFlow::Handled
            {
                return Ok(true);
            }
            continue;
        }

        if !request_is_executable_bash_tool(&request) {
            continue;
        }

        let auto_policy = AutoExecutionPolicy::current_runtime();
        let Some(assessment) = refresh_shell_request_assessment(
            &mut request,
            auto_policy.assessment_policy(AssessmentSource::ProviderShellTool),
        ) else {
            continue;
        };
        // Issue #1882: a fully readonly compound runs through the
        // dedicated argv executor instead of a shell handoff, so the
        // executed argv never passes through a shell parsing layer.
        match auto_policy.route(&assessment) {
            AutoExecutionRoute::DirectReadonlyBroker => {}
            AutoExecutionRoute::CompoundReadonlyExecutor => {
                if run_auto_approved_readonly_compound(state, request, output)?
                    == AutoApprovalFlow::Handled
                {
                    return Ok(true);
                }
                continue;
            }
            _ => continue,
        }

        if request_is_executable_bash_tool(&request)
            && request.request_id.is_none()
            && !provider_tool_call_fallback
        {
            if defer_fallback_bash_tool(state, request, output)? {
                return Ok(true);
            }
            continue;
        }

        let mut request = record_auto_approved_request(state, request);
        if apply_auto_approved_request_outcome(
            state,
            &mut request,
            MessageId::ApprovalResolutionAutoApprovedTitle,
            output,
        )? == AutoApprovalFlow::Handled
        {
            return Ok(true);
        }
    }

    render_approval_requests(state, &blocked_approval_ids, output)?;
    Ok(false)
}

/// The core's machine-readable blocked verdict: the M2 hook-block release
/// marks the provider-native result with `cosh_hook_verdict: "blocked"` on
/// the wire, and the adapter surfaces it as a ToolHookVerdict event that
/// sets this flag (#2156). Keying on the flag covers every fail-closed
/// morphology (block/deny/reject, hook failure, message-less blocks) and —
/// unlike result text — cannot be forged by the command's own output.
fn provider_native_shell_result_is_hook_block(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    if request.provider_shell_request_kind.is_control_permission() {
        return false;
    }
    let Some(tool_id) = request.tool_use_id.as_deref() else {
        return false;
    };
    state
        .control
        .provider_hook_result_is_blocked(&request.run_id, tool_id)
}

fn provider_native_shell_result_already_visible(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    !request.provider_shell_request_kind.is_control_permission()
        && request.tool_use_id.as_deref().is_some_and(|tool_id| {
            state
                .control
                .provider_shell_transcript_output_seen(&request.run_id, tool_id)
        })
}

fn provider_native_shell_covered_by_foreground(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    !request.provider_shell_request_kind.is_control_permission()
        && request_is_executable_bash_tool(request)
        && state
            .control
            .provider_foreground_shell_command_seen(shell_command_from_request_preview(request))
}

fn mark_provider_native_shell_request_transcript_seen(
    state: &mut InlineState,
    request: &RuntimeApprovalRequest,
) {
    if let Some(tool_id) = request.tool_use_id.as_deref() {
        state
            .control
            .mark_provider_shell_transcript_seen(&request.run_id, tool_id);
    }
}

fn shell_command_from_request_preview(request: &RuntimeApprovalRequest) -> &str {
    request
        .preview
        .strip_prefix("$ ")
        .unwrap_or(request.preview.as_str())
}

/// Receipt title for a provider replay of an already-executed shell tool
/// (issue #2064): echo how the request was actually resolved instead of
/// hard-coding "Auto-approved" — a manual Allow reads Approved, a
/// turn-scope consent reads the turn title, and only genuinely automatic
/// approvals (or no matching journal record: fail-safe) keep Auto-approved.
pub(super) fn completed_provider_native_shell_title(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> MessageId {
    let resolved = state.approvals.journal.iter().rev().find(|entry| {
        entry.run_id == request.run_id
            && entry.decision == ApprovalRequestStatus::Approved
            && match (&entry.tool_use_id, &request.tool_use_id) {
                (Some(journal_id), Some(request_id)) => journal_id == request_id,
                _ => entry.preview == request.preview,
            }
    });
    match resolved.map(|entry| entry.actor) {
        Some("user") => MessageId::ApprovalResolutionApprovedTitle,
        Some("user_batch") | Some("batch_consent") => {
            MessageId::ApprovalResolutionTurnApprovedTitle
        }
        _ => MessageId::ApprovalResolutionAutoApprovedTitle,
    }
}

fn render_completed_provider_native_shell_request<W: Write>(
    state: &mut InlineState,
    request: RuntimeApprovalRequest,
    output: &mut W,
) -> std::io::Result<()> {
    // Resolve the title before recording: recording journals this replay
    // as agent-auto, which would shadow the user's original decision.
    let title = completed_provider_native_shell_title(state, &request);
    let mut request = record_auto_approved_request(state, request);
    mark_provider_native_shell_execution(state, &mut request);
    render_approval_resolution(state, &request, title, output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoApprovalFlow {
    Continue,
    Handled,
}

fn approval_outcome_for_auto_request(request: &RuntimeApprovalRequest) -> ApprovalOutcome {
    classify_approval_outcome(ApprovalOutcomeInput {
        approved: request.status == ApprovalRequestStatus::Approved,
        shell_tool: request_is_executable_bash_tool(request),
        provider_request: request.provider_shell_request_kind.is_control_permission(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellRequestPolicyDecision {
    Continue,
    DenyAnalysisOnly,
    DenyDuplicateHostExecuted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellRequestPolicyHandling {
    Continue,
    Refused,
    AwaitingTerminalSweep,
}

/// Applies the shell-request policy gate to one event.
///
/// A delivered refusal is event-local, so callers keep processing the batch.
/// Without a live owner, callers stop before the tail record pass so the
/// detached terminal sweep retains responsibility for the response.
fn handle_shell_request_policy(
    state: &mut InlineState,
    run_request: Option<&AgentRequest>,
    request: &RuntimeApprovalRequest,
) -> ShellRequestPolicyHandling {
    let reason = match shell_request_policy_decision(state, run_request, request) {
        ShellRequestPolicyDecision::Continue => return ShellRequestPolicyHandling::Continue,
        ShellRequestPolicyDecision::DenyAnalysisOnly => {
            crate::adapter::ANALYSIS_ONLY_SHELL_DENY_MESSAGE
        }
        ShellRequestPolicyDecision::DenyDuplicateHostExecuted => {
            DUPLICATE_HOST_EXECUTED_SHELL_DENY_MESSAGE
        }
    };
    deny_policy_refused_shell_request(state, request, reason)
}

/// Refusal reason for a shell tool call the host already executed once and
/// whose result was delivered through the control protocol.
const DUPLICATE_HOST_EXECUTED_SHELL_DENY_MESSAGE: &str = "Duplicate shell tool request was already completed via host-executed shell result; no second foreground execution was run.";

pub(super) fn shell_request_policy_decision(
    state: &InlineState,
    run_request: Option<&AgentRequest>,
    request: &RuntimeApprovalRequest,
) -> ShellRequestPolicyDecision {
    if !request_is_executable_bash_tool(request) {
        return ShellRequestPolicyDecision::Continue;
    }
    if run_request_is_analysis_only_continuation(run_request) {
        return ShellRequestPolicyDecision::DenyAnalysisOnly;
    }
    if duplicate_host_executed_shell_result_delivered(state, request) {
        return ShellRequestPolicyDecision::DenyDuplicateHostExecuted;
    }
    if request.provider_shell_request_kind.is_control_permission() {
        return ShellRequestPolicyDecision::Continue;
    }
    if state.evidence.has_open_provider_shell_evidence()
        || state.agent_run.host_executed_shell_result_delivered
        || state
            .control
            .provider_shell_handoff_run_seen(&request.run_id)
        || run_already_approved_shell_tool(state, &request.run_id)
    {
        return ShellRequestPolicyDecision::DenyAnalysisOnly;
    }
    ShellRequestPolicyDecision::Continue
}

fn duplicate_host_executed_shell_result_delivered(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    if !request_is_executable_bash_tool(request)
        || !request.provider_shell_request_kind.is_control_permission()
    {
        return false;
    }
    let Some(request_id) = request.request_id.as_deref() else {
        return false;
    };
    state.control.provider_host_executed_shell_result_delivered(
        &request.run_id,
        request_id,
        request.tool_use_id.as_deref(),
    )
}

fn run_already_approved_shell_tool(state: &InlineState, run_id: &str) -> bool {
    state.approvals.requests.iter().any(|request| {
        request.run_id == run_id
            && request.status == ApprovalRequestStatus::Approved
            && request_is_executable_bash_tool(request)
    })
}

fn apply_auto_approved_request_outcome<W: Write>(
    state: &mut InlineState,
    request: &mut RuntimeApprovalRequest,
    title: MessageId,
    output: &mut W,
) -> std::io::Result<AutoApprovalFlow> {
    if request.status != ApprovalRequestStatus::Approved {
        render_approval_resolution(
            state,
            request,
            MessageId::ApprovalResolutionBlockedTitle,
            output,
        )?;
        return Ok(AutoApprovalFlow::Handled);
    }
    render_hook_warning_notices(state, request, output)?;
    let outcome = approval_outcome_for_auto_request(request);
    if outcome == ApprovalOutcome::ForegroundShellHandoff {
        let authorized = state
            .audit
            .as_mut()
            .map(|audit| {
                audit.authorize_host_execution(
                    approval_audit_input(request),
                    "shell_foreground_handoff",
                )
            })
            .transpose();
        if authorized.is_err() {
            request.status = ApprovalRequestStatus::Blocked;
            request.execution_path = Some("blocked_audit_required");
            render_approval_resolution(
                state,
                request,
                MessageId::ApprovalResolutionBlockedTitle,
                output,
            )?;
            return Ok(AutoApprovalFlow::Handled);
        }
    }
    if outcome == ApprovalOutcome::ProviderNativeShellFallback {
        mark_provider_native_shell_execution(state, request);
    }
    render_approval_resolution(state, request, title, output)?;

    match outcome {
        ApprovalOutcome::ProviderNativeShellFallback => {
            respond_provider_native_shell_fallback(state, request);
            Ok(AutoApprovalFlow::Continue)
        }
        ApprovalOutcome::ProviderApprovalResponse => {
            respond_auto_approval_to_provider(state, request);
            Ok(AutoApprovalFlow::Continue)
        }
        ApprovalOutcome::LocalOnly => Ok(AutoApprovalFlow::Continue),
        ApprovalOutcome::ForegroundShellHandoff => {
            mark_provider_approval_resolved(state);
            queue_approved_shell_handoff(state, request);
            if !request_can_receive_host_executed_result(state, request) {
                stop_active_agent_run_without_rendering(state, output)?;
            }
            Ok(AutoApprovalFlow::Handled)
        }
    }
}

// DR-6: When hooks had something to say but the tool is auto-approved,
// render an independent hook notice panel before the tool call header
// so that the user is aware of the hook's intervention.
fn render_hook_warning_notices<W: Write>(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
    output: &mut W,
) -> std::io::Result<()> {
    if request.hook_warnings.is_empty() {
        return Ok(());
    }
    let mut body: Vec<String> = Vec::new();
    for w in &request.hook_warnings {
        let icon = hook_warning_icon(w.decision.as_deref());
        body.push(format!("\u{2502} {icon} {}", w.hook_name));
        for msg_line in w.message.lines() {
            body.push(format!("\u{2502}   {msg_line}"));
        }
    }
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: "Hook",
            body,
            footer: None,
        },
    )
}

/// Issue #1882: runs an auto-approved readonly compound through the
/// dedicated argv executor. Eligibility and the executed plan come
/// from the same predicate (`build_readonly_compound_plan`), so the
/// assessment above and the execution here can never disagree about
/// what would run.
fn run_auto_approved_readonly_compound<W: Write>(
    state: &mut InlineState,
    request: RuntimeApprovalRequest,
    output: &mut W,
) -> std::io::Result<AutoApprovalFlow> {
    let Ok(command) = shell_handoff_command_from_request(&request) else {
        // The command text cannot be reconstructed from the request;
        // fall back to the manual flow rather than executing anything.
        return Ok(AutoApprovalFlow::Continue);
    };
    let Some(plan) = build_readonly_compound_plan(&command) else {
        // Route and plan are built by the same predicate, so a miss
        // here means the request changed between assessment and
        // execution; fall back to the manual flow rather than
        // executing anything.
        return Ok(AutoApprovalFlow::Continue);
    };
    let Some(execution_cwd) = readonly_compound_execution_cwd(state, &request) else {
        // No verifiable working directory for this session: guessing
        // (e.g. the process launch directory after the user has cd'd
        // away) could auto-execute in the wrong repository, so keep
        // the manual AskUser flow.
        return Ok(AutoApprovalFlow::Continue);
    };

    let mut request = record_auto_approved_request(state, request);
    // Mirror the handoff path's status gate: when the audit recorder
    // failed closed (blocked_audit_required) the approval is not
    // durable, and a blocked request must never reach the executor.
    let blocked_before_authorize = request.status != ApprovalRequestStatus::Approved;
    let authorized = if blocked_before_authorize {
        Ok(None)
    } else {
        state
            .audit
            .as_mut()
            .map(|audit| {
                audit.authorize_host_execution(
                    approval_audit_input(&request),
                    "readonly_compound_argv_executor",
                )
            })
            .transpose()
    };
    if blocked_before_authorize || authorized.is_err() {
        request.status = ApprovalRequestStatus::Blocked;
        request.execution_path = Some("blocked_audit_required");
        render_approval_resolution(
            state,
            &request,
            MessageId::ApprovalResolutionBlockedTitle,
            output,
        )?;
        return Ok(AutoApprovalFlow::Handled);
    }
    render_hook_warning_notices(state, &request, output)?;
    render_approval_resolution(
        state,
        &request,
        MessageId::ApprovalResolutionAutoApprovedTitle,
        output,
    )?;

    let started = std::time::Instant::now();
    let executed = run_readonly_compound(&plan, &ReadonlyPipelineConfig::default(), &execution_cwd);
    let duration_ms = started.elapsed().as_millis() as u64;
    let status = readonly_compound_completion_status(&executed, &command);
    let pipeline_output = executed.unwrap_or_else(|err| ReadonlyPipelineOutput {
        exit_code: None,
        stdout: String::new(),
        stderr: format!(
            "readonly compound executor error [{}]: {}",
            err.reason, err.detail
        ),
    });
    let evidence = record_readonly_compound_completion(
        state,
        &request,
        &command,
        &pipeline_output,
        &execution_cwd,
        status,
        duration_ms,
    );
    mark_provider_approval_resolved(state);
    if !evidence.provider_result_delivered
        && !request_can_receive_host_executed_result(state, &request)
    {
        stop_active_agent_run_without_rendering(state, output)?;
    }
    Ok(AutoApprovalFlow::Handled)
}

/// Verifiable working directory for the executor route, or `None` when
/// nothing trustworthy is available (the caller then keeps AskUser).
/// Priority: an explicit request cwd wins when it names a real
/// directory — and blocks when it does not (never silently replaced);
/// a placeholder falls through to the shell's live cwd from the latest
/// marker-tracked command block; with zero observed command activity,
/// the shell's own latest prompt-time cwd report is used instead — a
/// positive shell-side signal that also proves the marker channel
/// works, and that a later line submission invalidates (dispatcher).
/// There is no process-cwd guess: a session whose shell never
/// reported anything stays on the manual flow (absent markers alone
/// prove nothing about where the shell sits).
fn readonly_compound_execution_cwd(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> Option<std::path::PathBuf> {
    let explicit = !request.cwd.is_empty() && request.cwd != "<unknown>";
    if explicit {
        let candidate = std::path::Path::new(&request.cwd);
        return candidate.is_dir().then(|| candidate.to_path_buf());
    }
    if let Some(block) = state.session_blocks.last() {
        let live = std::path::Path::new(&block.end_cwd);
        if !block.end_cwd.is_empty() && live.is_dir() {
            return Some(live.to_path_buf());
        }
        return None;
    }
    if state.shell_command_activity_observed {
        return None;
    }
    let reported = std::path::Path::new(state.shell_prompt_cwd.as_deref()?);
    reported.is_dir().then(|| reported.to_path_buf())
}

/// Completion status for the executor route, mirroring the handoff
/// outcome contract (issue #1882, R6): a finished run classifies by its
/// exit code (completed / failed / interrupted), executor timeouts
/// report timed_out, and every other executor error reports
/// not_executed — the provider must never mistake a failed or
/// unfinished execution for success.
pub(super) fn readonly_compound_completion_status(
    executed: &Result<ReadonlyPipelineOutput, ReadonlyPipelineError>,
    command: &str,
) -> &'static str {
    use crate::command::{classify_executed_command_outcome, CommandOutcome};
    match executed {
        Ok(output) => {
            classify_executed_command_outcome(output.exit_code.unwrap_or(-1), command).status()
        }
        Err(err) if err.reason.contains("timeout") => CommandOutcome::TimedOut.status(),
        Err(_) => CommandOutcome::NotExecuted.status(),
    }
}

fn respond_provider_native_shell_fallback(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    let Some(request_id) = request.request_id.as_ref() else {
        return false;
    };
    let Some(active_run) = state.agent_run.active.as_ref() else {
        return true;
    };
    active_run
        .handle
        .respond_approval(provider_allow_response(ProviderResponseInput {
            request_id,
            tool_use_id: request.tool_use_id.as_deref(),
            tool_input: request.tool_input.as_ref(),
        }))
        .is_ok()
}

fn mark_provider_native_shell_execution(
    state: &mut InlineState,
    request: &mut RuntimeApprovalRequest,
) {
    let metadata = approval_execution_metadata(
        ApprovalOutcome::ProviderNativeShellFallback,
        ProviderApprovalStatus::Approved,
        request_is_executable_bash_tool(request),
    );
    set_approval_execution_metadata(state, &request.id, metadata);
    apply_approval_execution_metadata(request, metadata);
}

fn set_approval_execution_metadata(
    state: &mut InlineState,
    approval_id: &str,
    metadata: ApprovalExecutionMetadata,
) {
    for request in &mut state.approvals.requests {
        if request.id == approval_id {
            apply_approval_execution_metadata(request, metadata);
        }
    }
    for entry in &mut state.approvals.journal {
        if entry.id == approval_id {
            entry.execution_path = metadata.execution_path;
            entry.redaction_status = metadata.redaction_status;
        }
    }
}

fn apply_approval_execution_metadata(
    request: &mut RuntimeApprovalRequest,
    metadata: ApprovalExecutionMetadata,
) {
    request.execution_path = metadata.execution_path;
    request.redaction_status = metadata.redaction_status;
}

fn defer_fallback_bash_tool<W: Write>(
    state: &mut InlineState,
    request: RuntimeApprovalRequest,
    output: &mut W,
) -> std::io::Result<bool> {
    if !request_is_executable_bash_tool(&request)
        || request.provider_shell_request_kind.is_control_permission()
    {
        return Ok(false);
    }
    let request = record_deferred_fallback_request(state, request);
    render_approval_resolution(
        state,
        &request,
        MessageId::ApprovalResolutionDeferredTitle,
        output,
    )?;
    stop_active_agent_run_without_rendering(state, output)?;
    Ok(true)
}

fn respond_auto_approval_to_provider(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    if request_is_executable_bash_tool(request) {
        return false;
    }
    let Some(request_id) = request.request_id.as_ref() else {
        return false;
    };
    let Some(active_run) = state.agent_run.active.as_ref() else {
        return true;
    };
    let response = match request.tool_use_id.as_ref() {
        Some(tool_use_id) => provider_allow_response(ProviderResponseInput {
            request_id,
            tool_use_id: Some(tool_use_id),
            tool_input: request.tool_input.as_ref(),
        }),
        None => provider_deny_response(
            ProviderResponseInput {
                request_id,
                tool_use_id: None,
                tool_input: request.tool_input.as_ref(),
            },
            "Missing provider tool_use_id".to_string(),
        ),
    };
    let _ = active_run.handle.respond_approval(response);
    true
}

/// Refuses one shell request on policy grounds: answer the owning provider
/// when there is a control request to answer, then record the refusal.
///
/// A successfully delivered control refusal gets a terminal home so the tail
/// pass cannot resurface it. If delivery is unavailable, the request remains
/// unhomed for the active or detached terminal sweep. Streamed fallbacks have
/// no provider response and are recorded as shell-local refusals.
fn deny_policy_refused_shell_request(
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
