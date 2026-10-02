use crate::diagnostics::health::{health_context_hint, HealthScanReport};
use crate::evidence::{output_excerpt_status_for_block, provider_safe_command_facts};
use crate::types::{
    request_context_binding, AgentContextBinding, AgentRequest, CommandBlock,
    CONTEXT_BINDING_HINT_PREFIX,
};

pub(crate) fn finalize_agent_request_skill_context(
    request: &mut AgentRequest,
    startup_health_report: Option<&HealthScanReport>,
) {
    let binding = request_context_binding(request);
    remove_provider_visible_skill_directives(request);
    request.recommended_skill = None;
    if let Some(finding) = request.hook_finding.as_mut() {
        finding.skill = None;
    }

    match binding {
        AgentContextBinding::FailedCommand => attach_failed_command_context(request),
        AgentContextBinding::HookConsultation => attach_hook_diagnostic_context(request),
        AgentContextBinding::StartupHealthFollowUp => {
            attach_startup_health_context(request, startup_health_report);
        }
        AgentContextBinding::FreeForm
        | AgentContextBinding::SelectedCommand
        | AgentContextBinding::ControlProtocolEvidence
        | AgentContextBinding::ShellHandoffContinuation => {}
    }
}

fn remove_provider_visible_skill_directives(request: &mut AgentRequest) {
    request.context_hints.retain(|hint| {
        !hint.starts_with(CONTEXT_BINDING_HINT_PREFIX) && !contains_legacy_skill_directive(hint)
    });
}

fn contains_legacy_skill_directive(hint: &str) -> bool {
    hint.split_whitespace().any(|token| {
        let key = token.split_once('=').map(|(key, _)| key).unwrap_or(token);
        let key = key
            .trim_matches(|ch: char| ch == '`' || ch == ',' || ch == ';' || ch == ':')
            .to_ascii_lowercase();
        matches!(key.as_str(), "recommended_skill" | "skill_preference")
            || key.starts_with("required_")
    })
}

fn attach_failed_command_context(request: &mut AgentRequest) {
    if request
        .context_hints
        .iter()
        .any(|hint| hint.starts_with("insight_evidence\n"))
    {
        return;
    }

    if !request
        .context_hints
        .iter()
        .any(|hint| hint.starts_with("failed_command_context "))
    {
        request
            .context_hints
            .push(failed_command_context_hint(&request.command_block));
    }

    if let Some(hint) = memory_diagnostic_context_hint(&request.command_block) {
        if !request
            .context_hints
            .iter()
            .any(|existing| existing == &hint)
        {
            request.context_hints.push(hint);
        }
    }
}

fn attach_hook_diagnostic_context(request: &mut AgentRequest) {
    let Some(finding) = request.hook_finding.as_ref() else {
        return;
    };
    if !hook_finding_is_memory_diagnostic(finding) {
        return;
    }
    let hint = format!(
        "diagnostic_context domain=memory source=hook_consultation hook_id={} severity={} confidence=medium workflow={}",
        quote_value(&finding.hook_id),
        quote_value(&format!("{:?}", finding.severity).to_ascii_lowercase()),
        quote_value("invoke_matching_available_diagnostic_skill_before_ad_hoc_shell"),
    );
    if !request
        .context_hints
        .iter()
        .any(|existing| existing == &hint)
    {
        request.context_hints.push(hint);
    }
}

fn hook_finding_is_memory_diagnostic(finding: &crate::types::HookFinding) -> bool {
    if matches!(
        finding.hook_id.as_str(),
        "memory-pressure" | "high-memory-process"
    ) {
        return true;
    }

    let text = format!(
        "{} {} {}",
        finding.hook_id, finding.title, finding.description
    );
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .any(|token| {
            matches!(
                token.as_str(),
                "memory" | "mem" | "swap" | "oom" | "shmem" | "tmpfs" | "filecache"
            )
        })
}

fn attach_startup_health_context(
    request: &mut AgentRequest,
    startup_health_report: Option<&HealthScanReport>,
) {
    let Some(report) = startup_health_report else {
        return;
    };
    let Some(hint) = health_context_hint(report) else {
        return;
    };
    if !request
        .context_hints
        .iter()
        .any(|existing| existing.starts_with("health_scan "))
    {
        request.context_hints.push(hint);
    }
}

fn failed_command_context_hint(block: &CommandBlock) -> String {
    let facts = provider_safe_command_facts(block);
    let output_available = facts.output_id != "<missing>" && facts.output_bytes > 0;
    format!(
        "failed_command_context block_id={} command={} cwd={} exit_code={} output_available={} output_id={} output_bytes={} excerpt_status={} redaction_status=command_provider_safe",
        quote_value(&facts.id),
        quote_value(&facts.command),
        quote_value(&facts.cwd),
        facts.exit_code,
        output_available,
        quote_value(&facts.output_id),
        facts.output_bytes,
        quote_value(output_excerpt_status_for_block(block)),
    )
}

fn memory_diagnostic_context_hint(block: &CommandBlock) -> Option<String> {
    let facts = provider_safe_command_facts(block);
    let symptom = if block.exit_code == 137 {
        Some(("oom_or_sigkill", "exit_code:137", "high"))
    } else {
        let command = facts.command.to_ascii_lowercase();
        if ["oom", "out of memory", "killed"]
            .iter()
            .any(|needle| command.contains(needle))
        {
            Some(("oom_or_sigkill", "command_text", "medium"))
        } else {
            None
        }
    }?;
    Some(format!(
        "diagnostic_context domain=memory symptom={} evidence={} source=failed_command confidence={} workflow={}",
        quote_value(symptom.0),
        quote_value(symptom.1),
        quote_value(symptom.2),
        quote_value("invoke_matching_available_diagnostic_skill_before_ad_hoc_shell"),
    ))
}

fn quote_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::set_request_context_binding;
    use crate::types::{CommandOrigin, CommandStatus, FindingSeverity, HookFinding, OutputRefs};

    fn request_for_block(block: CommandBlock) -> AgentRequest {
        let mut request = AgentRequest {
            id: "agent-request-1".to_string(),
            session_id: block.session_id.clone(),
            command_block: block,
            context_blocks: Vec::new(),
            context_hints: Vec::new(),
            user_input: None,
            findings: Vec::new(),
            mode: crate::types::AgentMode::RecommendOnly,
            user_confirmed: true,
            hook_finding: None,
            recommended_skill: Some("memory-analysis".to_string()),
        };
        set_request_context_binding(
            &mut request,
            crate::types::AgentContextBinding::FailedCommand,
        );
        request
    }

    fn block(exit_code: i32, command: &str, status: CommandStatus) -> CommandBlock {
        CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: command.to_string(),
            origin: CommandOrigin::UserInteractive,
            cwd: "/tmp/project".to_string(),
            end_cwd: "/tmp/project".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code,
            status,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        }
    }

    fn hook_finding(hook_id: &str, title: &str, description: &str) -> HookFinding {
        HookFinding {
            hook_id: hook_id.to_string(),
            severity: FindingSeverity::Warning,
            title: title.to_string(),
            description: description.to_string(),
            suggestion: "Inspect the finding".to_string(),
            skill: None,
            cli_hint: None,
            context_refs: Vec::new(),
        }
    }

    #[test]
    fn failed_command_gets_source_bound_context_without_skill_route() {
        let mut request =
            request_for_block(block(1, "cargo build --token abc", CommandStatus::Failed));
        request.context_hints.push(
            "recommended_skill=alibabacloud-sysom-diagnosis required_first_step=memory_classify"
                .to_string(),
        );

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(hints.contains("failed_command_context "));
        assert!(hints.contains("command=\"cargo build --token <redacted>\""));
        assert!(!hints.contains("recommended_skill"));
        assert!(!hints.contains("alibabacloud-sysom-diagnosis"));
        assert!(request.recommended_skill.is_none());
    }

    #[test]
    fn insight_evidence_replaces_legacy_failed_command_context() {
        let mut request = request_for_block(block(1, "cargo build", CommandStatus::Failed));
        request
            .context_hints
            .push("insight_evidence\ntarget_facts:\ncommand_id=cmd-1".to_string());

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(hints.contains("insight_evidence"));
        assert!(!hints.contains("failed_command_context"));
        assert!(!hints.contains("diagnostic_context"));
    }

    #[test]
    fn neutral_context_hint_with_sysom_name_is_preserved() {
        let mut request = request_for_block(block(
            1,
            "systemctl status sysom-agent",
            CommandStatus::Failed,
        ));
        request
            .context_hints
            .push("observed_process name=sysom-agent state=running".to_string());

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(
            hints.contains("observed_process name=sysom-agent"),
            "{hints}"
        );
        assert!(!hints.contains("recommended_skill"), "{hints}");
    }

    #[test]
    fn oom_failed_command_gets_facts_only_memory_diagnostic_context() {
        let mut request = request_for_block(block(137, "/tmp/worker", CommandStatus::Failed));

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(hints.contains("diagnostic_context domain=memory"));
        assert!(hints.contains("symptom=\"oom_or_sigkill\""));
        assert!(hints.contains("evidence=\"exit_code:137\""));
        assert!(hints.contains(
            "workflow=\"invoke_matching_available_diagnostic_skill_before_ad_hoc_shell\""
        ));
        assert!(!hints.contains("sysom"));
        assert!(!hints.contains("required_first_step"));
    }

    #[test]
    fn free_form_request_does_not_get_failed_or_health_context() {
        let mut request = request_for_block(block(0, "分析一下系统", CommandStatus::Completed));
        request.user_input = Some("分析一下系统".to_string());
        set_request_context_binding(&mut request, crate::types::AgentContextBinding::FreeForm);

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(!hints.contains("__cosh_context_binding"));
        assert!(!hints.contains("failed_command_context"));
        assert!(!hints.contains("health_scan"));
    }

    #[test]
    fn hook_consultation_memory_token_gets_diagnostic_context() {
        let mut request = request_for_block(block(0, "free -m", CommandStatus::Completed));
        set_request_context_binding(
            &mut request,
            crate::types::AgentContextBinding::HookConsultation,
        );
        request.hook_finding = Some(hook_finding(
            "memory-pressure",
            "Available memory is low",
            "Command output shows available memory pressure",
        ));

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(
            hints.contains("diagnostic_context domain=memory"),
            "{hints}"
        );
        assert!(hints.contains("source=hook_consultation"), "{hints}");
    }

    #[test]
    fn hook_consultation_cache_text_without_memory_token_is_not_memory_context() {
        let mut request = request_for_block(block(0, "npm test", CommandStatus::Completed));
        set_request_context_binding(
            &mut request,
            crate::types::AgentContextBinding::HookConsultation,
        );
        request.hook_finding = Some(hook_finding(
            "build-cache",
            "Cache artifacts are stale",
            "Rebuild dependency cache before running tests",
        ));

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(
            !hints.contains("diagnostic_context domain=memory"),
            "{hints}"
        );
    }

    #[test]
    fn control_protocol_evidence_does_not_get_failed_command_context() {
        let mut request = request_for_block(block(137, "tool evidence", CommandStatus::Failed));
        request.user_input =
            Some("ShellEvidenceExcerpt\noutput_id: terminal-output://s/cmd-1".to_string());
        set_request_context_binding(
            &mut request,
            crate::types::AgentContextBinding::ControlProtocolEvidence,
        );

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(!hints.contains("failed_command_context"));
        assert!(!hints.contains("diagnostic_context domain=memory"));
    }

    #[test]
    fn shell_handoff_continuation_does_not_get_failed_command_context() {
        let mut request =
            request_for_block(block(137, "approved shell handoff", CommandStatus::Failed));
        request.user_input = Some("ShellCommandCompleted evidence".to_string());
        set_request_context_binding(
            &mut request,
            crate::types::AgentContextBinding::ShellHandoffContinuation,
        );

        finalize_agent_request_skill_context(&mut request, None);

        let hints = request.context_hints.join("\n");
        assert!(!hints.contains("failed_command_context"));
        assert!(!hints.contains("diagnostic_context domain=memory"));
    }
}
