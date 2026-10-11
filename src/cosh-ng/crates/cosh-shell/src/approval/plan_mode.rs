use crate::approval::approved_tool::{
    request_is_executable_bash_tool, request_is_readonly_builtin_tool,
};
use crate::approval::policy::{deny_policy_refused_request, ShellRequestPolicyHandling};
use crate::approval::requests::approval_request_from_governed_event;
use crate::runtime::prelude::*;
use crate::tools::{
    assess_shell_command, is_plan_mode_allowed_tool_name, is_shell_tool_name, AssessmentSource,
    AutoExecutionPolicy, AutoExecutionRoute,
};

/// Refusal reason for a mutating tool request while plan mode is on
/// (#1776). The provider needs the exit path in the message itself: it
/// steers the agent back to investigation instead of retrying the call.
pub(crate) const PLAN_MODE_TOOL_DENY_MESSAGE: &str = "Plan mode is active: only read-only investigation is allowed. Present your plan, then ask the user to run /plan (or /mode plan off) to exit plan mode before executing changes.";

/// Plan mode (#1776) denies tool requests that are not read-only:
/// read-only builtin tools keep running, shell commands run only when
/// they route to a readonly executor, everything else is refused with
/// the `/plan` exit pointer. Unknown tools fail closed.
pub(crate) fn plan_mode_denies_request(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> bool {
    if !state.plan_mode {
        return false;
    }
    if request.kind != ApprovalRequestKind::Tool {
        return false;
    }
    if is_plan_mode_allowed_tool_name(&request.subject) {
        return false;
    }
    if request_is_readonly_builtin_tool(request) {
        return false;
    }
    if request_is_executable_bash_tool(request) && shell_request_routes_readonly(request) {
        return false;
    }
    true
}

/// A shell command survives plan mode only when the runtime auto policy
/// routes it to a readonly executor — the same evidence the auto path
/// itself would auto-execute on, so the two paths can never disagree.
fn shell_request_routes_readonly(request: &RuntimeApprovalRequest) -> bool {
    if !is_shell_tool_name(&request.subject) {
        return false;
    }
    let command = request
        .preview
        .strip_prefix("$ ")
        .unwrap_or(&request.preview)
        .trim();
    if command.is_empty() {
        return false;
    }
    let policy = AutoExecutionPolicy::current_runtime()
        .assessment_policy(AssessmentSource::ProviderShellTool);
    let assessment = assess_shell_command(command, policy);
    matches!(
        AutoExecutionPolicy::current_runtime().route(&assessment),
        AutoExecutionRoute::DirectReadonlyBroker | AutoExecutionRoute::CompoundReadonlyExecutor
    )
}

/// Denies one request on plan-mode grounds. Mirrors the shell-request
/// policy contract: the handling variant tells approval loops whether
/// the batch must stop for the detached terminal sweep.
pub(crate) fn deny_request_for_plan_mode(
    state: &mut InlineState,
    request: &RuntimeApprovalRequest,
) -> ShellRequestPolicyHandling {
    deny_policy_refused_request(state, request, PLAN_MODE_TOOL_DENY_MESSAGE)
}

/// Plan mode partition for the approval passes (#1776): mutating tool
/// events are refused before a card can be recorded — a card would let
/// the user approve around the mode plan mode exists to enforce — and
/// dropped from the batch the passes render. Requests that are not tool
/// requests (questions, auth) pass through untouched.
///
/// The refusal handling travels with the partition: a delivered refusal
/// (`Refused`) is event-local, but `AwaitingTerminalSweep` means the
/// denial could not be delivered or homed, so the caller must stop the
/// batch instead of advancing approval state while the unhomed request
/// awaits the terminal sweep — the same contract the trust/auto passes
/// apply to their own denials.
pub(crate) fn partition_plan_mode_gated_events(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    run_request: Option<&AgentRequest>,
    origin: AgentRunOrigin,
    ignore_tool_calls: bool,
) -> (Vec<GovernedEvent>, ShellRequestPolicyHandling) {
    let mut allowed = Vec::new();
    for event in governed_events {
        let Some(request) = approval_request_from_governed_event(
            state,
            event,
            run_request,
            origin,
            ignore_tool_calls,
        ) else {
            allowed.push(event.clone());
            continue;
        };
        if plan_mode_denies_request(state, &request) {
            if let ShellRequestPolicyHandling::AwaitingTerminalSweep =
                deny_request_for_plan_mode(state, &request)
            {
                return (allowed, ShellRequestPolicyHandling::AwaitingTerminalSweep);
            }
            continue;
        }
        allowed.push(event.clone());
    }
    (allowed, ShellRequestPolicyHandling::Continue)
}

#[cfg(test)]
mod tests {
    use super::{plan_mode_denies_request, PLAN_MODE_TOOL_DENY_MESSAGE};
    use crate::runtime::prelude::*;
    use crate::runtime::state::InlineState;

    fn request(subject: &str, preview: &str) -> RuntimeApprovalRequest {
        RuntimeApprovalRequest {
            id: "req-1".to_string(),
            audit_ref: None,
            run_id: "run-1".to_string(),
            origin: AgentRunOrigin::Standard,
            session_id: "sess-1".to_string(),
            cwd: "/tmp".to_string(),
            source: "test",
            provider_shell_request_kind: ProviderShellRequestKind::StreamedToolCallFallback,
            kind: ApprovalRequestKind::Tool,
            subject: subject.to_string(),
            preview: preview.to_string(),
            risk: "medium",
            request_id: None,
            tool_use_id: None,
            tool_input: None,
            original_user_request: None,
            status: ApprovalRequestStatus::Pending,
            execution_path: None,
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            hook_requires_approval: false,
            hook_warnings: Vec::new(),
        }
    }

    #[test]
    fn plan_mode_denies_mutating_tools_and_allows_readonly_investigation() {
        let mut state = InlineState::default();

        // Off: nothing is denied.
        assert!(!plan_mode_denies_request(
            &state,
            &request("Bash", "$ rm -rf /tmp/scratch")
        ));

        state.plan_mode = true;
        // Mutating shell command.
        assert!(plan_mode_denies_request(
            &state,
            &request("Bash", "$ rm -rf /tmp/scratch")
        ));
        // Mutating builtin tools.
        assert!(plan_mode_denies_request(
            &state,
            &request("Write", "/tmp/out.txt")
        ));
        assert!(plan_mode_denies_request(
            &state,
            &request("Edit", "/tmp/a.rs")
        ));
        // Unknown tools fail closed.
        assert!(plan_mode_denies_request(
            &state,
            &request("MysteryTool", "x")
        ));
        // Read-only builtins.
        assert!(!plan_mode_denies_request(
            &state,
            &request("Read", "/etc/os-release")
        ));
        assert!(!plan_mode_denies_request(
            &state,
            &request("Grep", "pattern")
        ));
        // Planning-surface tools.
        assert!(!plan_mode_denies_request(
            &state,
            &request("todo_write", "plan")
        ));
        assert!(!plan_mode_denies_request(
            &state,
            &request("AskUserQuestion", "which?")
        ));
        // Read-only shell commands still run.
        assert!(!plan_mode_denies_request(
            &state,
            &request("Bash", "$ ls /tmp")
        ));
        assert!(!plan_mode_denies_request(
            &state,
            &request("Bash", "$ cat /etc/os-release")
        ));
        assert!(!plan_mode_denies_request(
            &state,
            &request("Bash", "$ grep -c root /etc/passwd")
        ));
    }

    #[test]
    fn plan_mode_deny_message_points_at_the_exit() {
        assert!(PLAN_MODE_TOOL_DENY_MESSAGE.contains("/plan"));
        assert!(PLAN_MODE_TOOL_DENY_MESSAGE.contains("read-only"));
    }
}
