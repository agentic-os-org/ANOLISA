//! The capability and purpose contract both Shell entries send to a provider.
//!
//! The shared invariants live here once; only the execution rules differ, so
//! the process providers keep their `shell` tool bullets while an ACP peer is
//! told exactly what this connection wired.

use crate::evidence::ShellEvidenceAccess;
use crate::types::{AgentRequest, CoshApprovalMode};

use super::{
    output_access_instruction, provider_language_hint, RESTRICTED_OUTPUT_ACCESS_INSTRUCTION,
};

pub fn provider_prompt_contract(mode: CoshApprovalMode, shell_tool_name: &str) -> String {
    provider_prompt_contract_for_language(
        mode,
        shell_tool_name,
        crate::language_config_status().effective,
    )
}

pub fn provider_prompt_contract_for_request(
    request: &AgentRequest,
    mode: CoshApprovalMode,
    shell_tool_name: &str,
) -> String {
    provider_prompt_contract_for_request_with_evidence_access(
        request,
        mode,
        shell_tool_name,
        ShellEvidenceAccess::FencedRequestFallback,
    )
}

pub fn provider_prompt_contract_for_request_with_evidence_access(
    request: &AgentRequest,
    mode: CoshApprovalMode,
    shell_tool_name: &str,
    access: ShellEvidenceAccess,
) -> String {
    if crate::types::request_is_analysis_only_continuation(request) {
        return analysis_continuation_contract_prompt(request, mode, shell_tool_name, access);
    }
    provider_prompt_contract_with_evidence_access(mode, shell_tool_name, access)
}

/// The invocation purpose for one turn, shared by both entries so the mode
/// text cannot drift between them.
fn invocation_mode_instruction(mode: CoshApprovalMode) -> (&'static str, &'static str) {
    match mode {
        CoshApprovalMode::Recommend => (
            "recommend",
            "This invocation is recommend mode: do not emit tool calls. Answer with concise guidance, explanations, and example commands in code blocks.",
        ),
        CoshApprovalMode::Auto | CoshApprovalMode::Trust => (
            "agent",
            "This invocation is agent mode: when the user asks to inspect system, project, file, test, runtime, or command state, actively use tools for live evidence instead of only suggesting commands for the user to run.",
        ),
    }
}

/// The turn-scoped purpose of an analysis-only continuation, including the
/// user's unchanged approval mode.
fn analysis_continuation_instruction(
    request: &AgentRequest,
    mode: CoshApprovalMode,
) -> (String, String) {
    let user_mode_name = request
        .context_hints
        .iter()
        .find_map(|hint| hint.strip_prefix(crate::types::USER_APPROVAL_MODE_HINT_PREFIX))
        .unwrap_or(match mode {
            CoshApprovalMode::Recommend => "recommend",
            CoshApprovalMode::Auto => "auto",
            CoshApprovalMode::Trust => "trust",
        });
    let target_mode = if user_mode_name == "recommend" {
        "recommend"
    } else {
        "agent"
    };
    let mode_instruction = format!(
        "This invocation is an analysis-only continuation after a foreground shell handoff: \
         the user's cosh-shell approval mode is {user_mode_name} and has not changed. \
         Do not emit tool calls in this turn; analyze the completed command's shell evidence \
         and state the conclusion. This restriction applies only to this turn; later turns may \
         use tools again."
    );
    (target_mode.to_string(), mode_instruction)
}

fn analysis_continuation_contract_prompt(
    request: &AgentRequest,
    mode: CoshApprovalMode,
    shell_tool_name: &str,
    access: ShellEvidenceAccess,
) -> String {
    let (target_mode, mode_instruction) = analysis_continuation_instruction(request, mode);
    invariant_contract_prompt(
        &target_mode,
        &mode_instruction,
        &legacy_execution_rules(shell_tool_name),
        provider_language_hint(crate::language_config_status().effective),
        access,
        false,
    )
}

pub fn provider_prompt_contract_for_language(
    mode: CoshApprovalMode,
    shell_tool_name: &str,
    language: crate::Language,
) -> String {
    let (target_mode, mode_instruction) = invocation_mode_instruction(mode);
    invariant_contract_prompt(
        target_mode,
        mode_instruction,
        &legacy_execution_rules(shell_tool_name),
        provider_language_hint(language),
        ShellEvidenceAccess::FencedRequestFallback,
        target_mode == "agent",
    )
}

fn invariant_contract_prompt(
    target_mode: &str,
    mode_instruction: &str,
    execution_rules: &str,
    language_hint: &str,
    access: ShellEvidenceAccess,
    allow_output_requests: bool,
) -> String {
    let output_access = if allow_output_requests {
        output_access_instruction(access, true)
    } else {
        RESTRICTED_OUTPUT_ACCESS_INSTRUCTION
    };
    format!(
        "\n\ncosh-shell Agent contract:\n\
         - User modes: recommend and agent.\n\
         - Mode: {target_mode}. {mode_instruction}\n\
         - Mode is an internal invocation contract; it does not change the user's cosh-shell \
         approval mode, and you must never tell the user their approval mode changed.\n\
         {execution_rules}\n\
         - terminal-output:// refs are cosh-shell evidence ids, not files. Do not use provider file tools to read them. {output_access}\n\
         - The approval system is handled by cosh-shell; do not downgrade to manual command suggestions only because approval may be needed.\n\
         - State the diagnostic conclusion first, or state explicitly when evidence is insufficient. Emit at most one primary recommendation command and explain why it fits this case; do not emit a generic pwd, echo $PATH, or --help probe list without a scenario-specific reason.\n\
         - {language_hint}\n\
         - Keep provider-specific names out of visible responses unless already shown by cosh-shell."
    )
}

pub fn provider_prompt_contract_with_evidence_access(
    mode: CoshApprovalMode,
    shell_tool_name: &str,
    access: ShellEvidenceAccess,
) -> String {
    let (target_mode, mode_instruction) = invocation_mode_instruction(mode);
    invariant_contract_prompt(
        target_mode,
        mode_instruction,
        &legacy_execution_rules(shell_tool_name),
        provider_language_hint(crate::language_config_status().effective),
        access,
        target_mode == "agent",
    )
}

/// The capability and purpose contract for the ACP entry.
///
/// `foreground` must be exactly the condition under which this connection
/// advertised the host-shell capability, so the prompt never promises a
/// foreground run the peer cannot reach. No tool name is fixed here: an ACP
/// peer only has the tools it advertised itself.
pub fn acp_prompt_contract(
    request: &AgentRequest,
    mode: CoshApprovalMode,
    foreground: bool,
    access: ShellEvidenceAccess,
) -> String {
    let execution_rules = acp_execution_rules(foreground);
    let language_hint = provider_language_hint(crate::language_config_status().effective);
    if crate::types::request_is_analysis_only_continuation(request) {
        let (target_mode, mode_instruction) = analysis_continuation_instruction(request, mode);
        return invariant_contract_prompt(
            &target_mode,
            &mode_instruction,
            &execution_rules,
            language_hint,
            access,
            false,
        );
    }
    let (target_mode, mode_instruction) = invocation_mode_instruction(mode);
    invariant_contract_prompt(
        target_mode,
        mode_instruction,
        &execution_rules,
        language_hint,
        access,
        target_mode == "agent",
    )
}

/// The execution claims an ACP peer may rely on, following only what this
/// connection actually wired.
fn acp_execution_rules(foreground: bool) -> String {
    let tools =
        "- Use the tools this peer advertised for live shell evidence when tool use is needed.";
    if foreground {
        format!(
            "{tools}\n- To run a command in the user's shell, request it through the cosh-shell host-shell extension. cosh-shell reviews every request before execution, even read-only commands in auto approval mode, and runs the approved command in the foreground shell transcript. Shell syntax is supported after cosh-shell approval; do not avoid useful shell syntax by asking the user to run commands manually."
        )
    } else {
        format!(
            "{tools}\n- This connection has no foreground shell executor: anything you run executes in your own environment, not in the user's shell. State that boundary instead of claiming the user's shell state changed."
        )
    }
}

/// The two execution bullets the process providers need, kept byte-identical
/// to the contract they already send.
fn legacy_execution_rules(shell_tool_name: &str) -> String {
    format!(
        "- Use `{shell_tool_name}` for live shell evidence when tool use is needed.\n- Always emit a provider permission request for `{shell_tool_name}` before any shell command executes, even read-only commands in auto approval mode. cosh-shell may auto-approve safe commands, but it still needs the request so the exact command can run in the foreground shell transcript. Shell syntax is supported after cosh-shell approval; do not avoid useful shell syntax by asking the user to run commands manually."
    )
}
