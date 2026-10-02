use crate::command::first_program_token;
use crate::insight::model::{InsightBinding, OutputExcerptStatus};
use crate::insight::scope::resolve_execution_scope;
use crate::types::{
    AgentMode, AgentRequest, CommandBlock, CommandStatus, Finding, FindingKind, Intervention,
    InterventionDecision, OutputRefs, ShellEvent, ShellEventKind,
};

pub fn findings_from_blocks(blocks: &[CommandBlock]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for block in blocks {
        if block.status == CommandStatus::Failed {
            findings.push(Finding {
                id: format!("finding-{}-nonzero", block.id),
                command_block_id: block.id.clone(),
                kind: FindingKind::NonZeroExit,
                severity: "warning".to_string(),
                message: format!(
                    "command exited with code {}: {}",
                    block.exit_code, block.command
                ),
            });

            if block.exit_code == 127 {
                findings.push(Finding {
                    id: format!("finding-{}-notfound", block.id),
                    command_block_id: block.id.clone(),
                    kind: FindingKind::CommandNotFound,
                    severity: "warning".to_string(),
                    message: "command was not found by the shell".to_string(),
                });
            }

            if block.exit_code == 126 {
                findings.push(Finding {
                    id: format!("finding-{}-permission", block.id),
                    command_block_id: block.id.clone(),
                    kind: FindingKind::PermissionDenied,
                    severity: "warning".to_string(),
                    message: "shell reported permission or executable access failure".to_string(),
                });
            }

            let program = first_program_token(&block.command);
            if program == "systemctl" {
                findings.push(Finding {
                    id: format!("finding-{}-service", block.id),
                    command_block_id: block.id.clone(),
                    kind: FindingKind::ServiceFailed,
                    severity: "warning".to_string(),
                    message: "service command failed and may need service-specific analysis"
                        .to_string(),
                });
            }
        }

        if block.output.terminal_output_ref.is_none() {
            findings.push(Finding {
                id: format!("finding-{}-missing-output", block.id),
                command_block_id: block.id.clone(),
                kind: FindingKind::MissingOutput,
                severity: "info".to_string(),
                message: "command output reference is missing".to_string(),
            });
        }
    }

    findings
}

pub fn interventions_from_findings(findings: &[Finding]) -> Vec<Intervention> {
    findings
        .iter()
        .map(|finding| Intervention {
            id: format!("intervention-{}", finding.id),
            finding_id: finding.id.clone(),
            command_block_id: finding.command_block_id.clone(),
            decision: InterventionDecision::Suggest,
            guidance: guidance_for_finding(&finding.kind),
        })
        .collect()
}

pub fn agent_request_after_confirmation(
    session_id: impl Into<String>,
    block: &CommandBlock,
    findings: &[Finding],
    confirmed: bool,
) -> Option<AgentRequest> {
    if !confirmed {
        return None;
    }

    Some(AgentRequest {
        id: format!("agent-request-{}", block.id),
        session_id: session_id.into(),
        command_block: block.clone(),
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: None,
        findings: findings
            .iter()
            .filter(|finding| finding.command_block_id == block.id)
            .cloned()
            .collect(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    })
}

pub(crate) fn failed_command_agent_request_after_confirmation(
    session_id: impl Into<String>,
    block: &CommandBlock,
    findings: &[Finding],
    confirmed: bool,
) -> Option<AgentRequest> {
    let mut request = agent_request_after_confirmation(session_id, block, findings, confirmed)?;
    crate::types::set_request_context_binding(
        &mut request,
        crate::types::AgentContextBinding::FailedCommand,
    );
    Some(request)
}

pub(crate) fn agent_request_for_auto_failure(
    session_id: impl Into<String>,
    block: &CommandBlock,
    findings: &[Finding],
) -> AgentRequest {
    let mut request = AgentRequest {
        id: format!("agent-request-{}", block.id),
        session_id: session_id.into(),
        command_block: block.clone(),
        context_blocks: Vec::new(),
        context_hints: vec!["__cosh_request_source=auto_failure_analysis".to_string()],
        user_input: None,
        findings: findings
            .iter()
            .filter(|finding| finding.command_block_id == block.id)
            .cloned()
            .collect(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: false,
        hook_finding: None,
        recommended_skill: None,
    };
    crate::types::set_request_context_binding(
        &mut request,
        crate::types::AgentContextBinding::FailedCommand,
    );
    request
}

pub(crate) fn agent_request_from_insight_binding(
    binding: &InsightBinding,
    current_session_id: &str,
    visible_input: &str,
    blocks: &[CommandBlock],
    findings: &[Finding],
) -> Option<AgentRequest> {
    let input = visible_input.trim();
    if input.is_empty() || binding.target.source_session_id != current_session_id {
        return None;
    }
    let block = blocks.iter().find(|block| {
        block.session_id == binding.target.source_session_id
            && block.id == binding.target.source_command_block_id
    })?;
    if resolve_execution_scope(&block.session_id, &block.command) != binding.target.scope {
        return None;
    }

    let mut target = binding.target.clone();
    target.evidence_status = current_insight_evidence_status(block, target.evidence_status);
    let mut context_hints = insight_target_context_hints(&target);
    context_hints.push("__cosh_request_source=insight_prompt".to_string());
    let mut request = AgentRequest {
        id: format!("agent-request-insight-{}", binding.target.insight_id),
        session_id: current_session_id.to_string(),
        command_block: block.clone(),
        context_blocks: Vec::new(),
        context_hints,
        user_input: Some(input.to_string()),
        findings: findings
            .iter()
            .filter(|finding| finding.command_block_id == block.id)
            .cloned()
            .collect(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    if block.exit_code != 0 {
        crate::types::set_request_context_binding(
            &mut request,
            crate::types::AgentContextBinding::FailedCommand,
        );
    }
    Some(request)
}

fn current_insight_evidence_status(
    block: &CommandBlock,
    captured_status: OutputExcerptStatus,
) -> OutputExcerptStatus {
    let Some(output_ref) = block.output.terminal_output_ref.as_deref() else {
        return OutputExcerptStatus::Unavailable;
    };
    let path = std::path::Path::new(output_ref);
    if !path.is_file() {
        return OutputExcerptStatus::Expired;
    }
    match std::fs::read_to_string(path) {
        Err(_) => OutputExcerptStatus::ReadFailed,
        Ok(text) if text.trim().is_empty() => OutputExcerptStatus::Empty,
        Ok(_) if captured_status == OutputExcerptStatus::Truncated => {
            OutputExcerptStatus::Truncated
        }
        Ok(_) => OutputExcerptStatus::Available,
    }
}

fn insight_target_context_hints(target: &crate::insight::model::InsightTarget) -> Vec<String> {
    let mut hints = vec![
        format!(
            "__cosh_insight_evidence_status={:?}",
            target.evidence_status
        ),
        format!("__cosh_insight_severity={:?}", target.severity),
        format!("__cosh_insight_confidence={:?}", target.confidence),
    ];
    hints.extend(target.evidence.iter().map(|evidence| {
        let key = bounded_metadata_component(&evidence.key, 128);
        let value = bounded_metadata_component(&evidence.value, 512);
        format!("__cosh_insight_evidence={key}={value}")
    }));
    hints
}

fn bounded_metadata_component(value: &str, max_bytes: usize) -> String {
    let value = value.replace(['\n', '\r'], " ");
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub fn agent_request_from_intercepted_input(
    event: &ShellEvent,
    sequence: usize,
    confirmed: bool,
) -> Option<AgentRequest> {
    if !confirmed || event.kind != ShellEventKind::UserInputIntercepted {
        return None;
    }

    let input = event.input.as_ref()?;
    if input.trim().is_empty() {
        return None;
    }

    let started_at_ms = event.started_at_ms.unwrap_or_default();
    let cwd = event
        .cwd
        .clone()
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or_else(|| "<unknown>".to_string());
    let block_id = format!("input-{sequence}");

    let mut request = AgentRequest {
        id: format!("agent-request-{block_id}"),
        session_id: event.session_id.clone(),
        command_block: CommandBlock {
            id: block_id,
            session_id: event.session_id.clone(),
            command: input.clone(),
            origin: Default::default(),
            cwd: cwd.clone(),
            end_cwd: cwd,
            started_at_ms,
            ended_at_ms: started_at_ms,
            duration_ms: 0,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some(input.clone()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    // Carry the shell-side sensitive flag so durable sinks past the
    // journal (personalization activity store) can whole-field redact.
    if event
        .routing
        .as_ref()
        .is_some_and(|routing| routing.sensitive)
    {
        crate::types::mark_request_sensitive_input(&mut request);
    }
    Some(request)
}

pub fn agent_request_confirmed_by_events(events: &[ShellEvent]) -> bool {
    events.iter().any(event_confirms_failed_command_analysis)
}

pub fn event_requests_agent_cancel(event: &ShellEvent) -> bool {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return false;
    }

    match event.component.as_deref() {
        Some("slash") | None => matches_agent_cancel_slash(event.input.as_deref()),
        Some("control") => event.input.as_deref() == Some("ctrl_c"),
        Some("card") => event.message.as_deref() == Some("agent_cancel"),
        _ => false,
    }
}

pub fn event_confirms_failed_command_analysis(event: &ShellEvent) -> bool {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return false;
    }

    match event.component.as_deref() {
        Some("slash") => matches_failure_analysis_slash(event.input.as_deref()),
        None => matches_failure_analysis_slash(event.input.as_deref()),
        _ => false,
    }
}

pub fn event_cancels_failed_command_analysis(event: &ShellEvent) -> bool {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return false;
    }

    match event.component.as_deref() {
        Some("slash") => matches_cancel_slash(event.input.as_deref()),
        None => matches_cancel_slash(event.input.as_deref()),
        _ => false,
    }
}

pub fn recommendation_selection_from_event(event: &ShellEvent) -> Option<usize> {
    recommendation_action_from_event(event).map(|action| action.index)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalCommandKind {
    Approve,
    ApproveTurn,
    AlwaysTrust,
    Deny,
    Details,
    SendToShell,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCommand {
    pub kind: ApprovalCommandKind,
    pub id: String,
}

pub fn approval_command_from_event(event: &ShellEvent) -> Option<ApprovalCommand> {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return None;
    }

    if event.component.as_deref() == Some("card") {
        let id = event.input.as_deref()?.trim();
        if id.is_empty() {
            return None;
        }
        if id.starts_with("consultation-") {
            return None;
        }
        return match event.message.as_deref() {
            Some("approve") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::Approve,
                id: id.to_string(),
            }),
            Some("approve_turn") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::ApproveTurn,
                id: id.to_string(),
            }),
            Some("always_trust") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::AlwaysTrust,
                id: id.to_string(),
            }),
            Some("deny") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::Deny,
                id: id.to_string(),
            }),
            Some("details") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::Details,
                id: id.to_string(),
            }),
            Some("send_to_shell") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::SendToShell,
                id: id.to_string(),
            }),
            Some("cancel") => Some(ApprovalCommand {
                kind: ApprovalCommandKind::Cancel,
                id: id.to_string(),
            }),
            _ => None,
        };
    }

    parse_approval_details_command(event)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendationActionKind {
    Select,
    Copy,
    Insert,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecommendationAction {
    pub kind: RecommendationActionKind,
    pub index: usize,
}

pub fn recommendation_action_from_event(event: &ShellEvent) -> Option<RecommendationAction> {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return None;
    }

    match event.component.as_deref() {
        Some("slash") | None => parse_recommendation_action(event.input.as_deref()),
        Some("card") => parse_recommendation_card_action(event),
        _ => None,
    }
}

fn matches_failure_analysis_slash(input: Option<&str>) -> bool {
    let first_token = input
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default();
    first_token == "/explain"
}

fn matches_cancel_slash(input: Option<&str>) -> bool {
    let first_token = input
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default();
    matches!(first_token, "/cancel" | "/clear" | "/shell")
}

fn matches_agent_cancel_slash(input: Option<&str>) -> bool {
    let first_token = input
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default();
    first_token == "/cancel"
}

fn parse_recommendation_action(input: Option<&str>) -> Option<RecommendationAction> {
    let mut tokens = input.unwrap_or_default().split_whitespace();
    let command = tokens.next()?;
    let kind = match command {
        "/select" => RecommendationActionKind::Select,
        "/copy" => RecommendationActionKind::Copy,
        _ => return None,
    };

    let index = tokens
        .next()?
        .parse::<usize>()
        .ok()
        .filter(|index| *index > 0)?;
    Some(RecommendationAction { kind, index })
}

fn parse_recommendation_card_action(event: &ShellEvent) -> Option<RecommendationAction> {
    let kind = match event.message.as_deref()? {
        "recommendation_copy" => RecommendationActionKind::Copy,
        "recommendation_insert" => RecommendationActionKind::Insert,
        "recommendation_details" => RecommendationActionKind::Details,
        _ => return None,
    };
    let index = event
        .input
        .as_deref()?
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|index| *index > 0)?;
    Some(RecommendationAction { kind, index })
}

fn parse_approval_details_command(event: &ShellEvent) -> Option<ApprovalCommand> {
    match event.component.as_deref() {
        Some("slash") | None => {}
        _ => return None,
    }

    let input = event.input.as_deref();
    let mut tokens = input.unwrap_or_default().split_whitespace();
    let command = tokens.next()?;
    let id = tokens.next()?.to_string();
    let kind = match command {
        "/details" => ApprovalCommandKind::Details,
        "/send-to-shell" => ApprovalCommandKind::SendToShell,
        _ => return None,
    };

    Some(ApprovalCommand { kind, id })
}

fn guidance_for_finding(kind: &FindingKind) -> String {
    match kind {
        FindingKind::NonZeroExit => {
            "show a short explanation and ask before deeper Agent analysis".to_string()
        }
        FindingKind::CommandNotFound => {
            "recommend checking PATH, package availability, or command spelling".to_string()
        }
        FindingKind::PermissionDenied => {
            "recommend checking executable bit, ownership, or required privileges".to_string()
        }
        FindingKind::ServiceFailed => {
            "recommend collecting service status and recent logs".to_string()
        }
        FindingKind::MissingOutput => {
            "recommend retrying with output capture enabled before detailed analysis".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        agent_request_after_confirmation, agent_request_for_auto_failure,
        agent_request_from_insight_binding, approval_command_from_event,
        event_requests_agent_cancel, failed_command_agent_request_after_confirmation,
        recommendation_action_from_event, ApprovalCommand, ApprovalCommandKind,
        RecommendationAction, RecommendationActionKind,
    };
    use crate::insight::model::{
        ExecutionScope, InsightBinding, InsightTarget, OutputExcerptStatus,
    };
    use crate::types::{CommandBlock, CommandOrigin, CommandStatus, OutputRefs, ShellEvent};

    fn failed_block() -> CommandBlock {
        CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "false".to_string(),
            origin: CommandOrigin::UserInteractive,
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        }
    }

    #[test]
    fn public_agent_request_builder_does_not_leak_internal_binding_hint() {
        let block = failed_block();
        let findings = super::findings_from_blocks(std::slice::from_ref(&block));

        let public_request = agent_request_after_confirmation("session-1", &block, &findings, true)
            .expect("public request");
        assert!(public_request
            .context_hints
            .iter()
            .all(|hint| !hint.starts_with("__cosh_context_binding=")));

        let internal_request =
            failed_command_agent_request_after_confirmation("session-1", &block, &findings, true)
                .expect("internal request");
        assert!(internal_request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_context_binding=failed_command"));
    }

    /// #2138: the sensitive routing flag on an intercept event must land
    /// on the request as the in-band hint so durable sinks past the
    /// journal can whole-field redact; unflagged intercepts stay clean.
    #[test]
    fn intercepted_input_request_carries_sensitive_hint_from_routing() {
        let mut event = ShellEvent::user_input_intercepted("session-1", "带 key 的句子");
        event.routing = Some(crate::types::ShellRoutingMetadata {
            generation: 1,
            top_level_missing: false,
            proven: false,
            sensitive: true,
            unsafe_input: false,
        });
        let request = super::agent_request_from_intercepted_input(&event, 1, true)
            .expect("sensitive request");
        assert!(crate::types::request_has_sensitive_input(&request));

        let plain_event = ShellEvent::user_input_intercepted("session-1", "普通句子");
        let plain_request = super::agent_request_from_intercepted_input(&plain_event, 2, true)
            .expect("plain request");
        assert!(!crate::types::request_has_sensitive_input(&plain_request));
    }

    #[test]
    fn intercepted_input_request_preserves_exact_nonempty_payload() {
        for (sequence, input) in [" 你好", "   你好", "\t你好", "你好 ", "你好\t", "  你好\t "]
            .into_iter()
            .enumerate()
        {
            let event = ShellEvent::user_input_intercepted("session-1", input);
            let request = super::agent_request_from_intercepted_input(&event, sequence, true)
                .expect("nonempty input request");

            assert_eq!(request.command_block.command, input);
            assert_eq!(request.user_input.as_deref(), Some(input));
        }

        for input in ["", " ", "   ", "\t", " \t "] {
            let event = ShellEvent::user_input_intercepted("session-1", input);
            assert!(super::agent_request_from_intercepted_input(&event, 7, true).is_none());
        }
    }

    #[test]
    fn auto_failure_builder_marks_system_generated_unconfirmed_request() {
        let block = failed_block();
        let findings = super::findings_from_blocks(std::slice::from_ref(&block));

        let request = agent_request_for_auto_failure("session-1", &block, &findings);

        assert_eq!(request.user_input, None);
        assert!(!request.user_confirmed);
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_request_source=auto_failure_analysis"));
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_context_binding=failed_command"));
    }

    #[test]
    fn insight_binding_builder_resolves_original_source_block_once() {
        let block = failed_block();
        let blocks = vec![block.clone()];
        let findings = super::findings_from_blocks(&blocks);
        let binding = InsightBinding {
            suggestion_id: "suggestion-1".to_string(),
            target: InsightTarget {
                insight_id: "insight-1".to_string(),
                source_session_id: "session-1".to_string(),
                source_command_block_id: block.id.clone(),
                scope: ExecutionScope::local("session-1"),
                evidence_handle: None,
                evidence_status: OutputExcerptStatus::Available,
                severity: crate::insight::model::InsightSeverity::Warning,
                confidence: crate::insight::model::InsightConfidence::High,
                evidence: vec![crate::insight::model::InsightEvidence {
                    key: "failure_class".to_string(),
                    value: "BuildOrTestFailure".to_string(),
                }],
                created_at_ms: 2,
            },
        };

        let request = agent_request_from_insight_binding(
            &binding,
            "session-1",
            "analyze the edited question",
            &blocks,
            &findings,
        )
        .expect("bound request");

        assert_eq!(request.command_block.id, block.id);
        assert_eq!(
            request.user_input.as_deref(),
            Some("analyze the edited question")
        );
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_insight_evidence_status=Unavailable"));
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_insight_severity=Warning"));
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_insight_confidence=High"));
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_insight_evidence=failure_class=BuildOrTestFailure"));
        assert!(request
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_request_source=insight_prompt"));
        assert!(agent_request_from_insight_binding(
            &binding,
            "other-session",
            "analyze",
            &blocks,
            &findings,
        )
        .is_none());

        let mut expired_block = block.clone();
        expired_block.output.terminal_output_ref =
            Some(format!("/tmp/cosh-expired-output-{}", std::process::id()));
        let expired = agent_request_from_insight_binding(
            &binding,
            "session-1",
            "analyze",
            &[expired_block],
            &findings,
        )
        .expect("expired source remains bound");
        assert!(expired
            .context_hints
            .iter()
            .any(|hint| hint == "__cosh_insight_evidence_status=Expired"));
    }

    #[test]
    fn parses_recommendation_actions_from_slash_events() {
        let mut allow = ShellEvent::user_input_intercepted("session-1", "/allow 2");
        allow.component = Some("slash".to_string());
        let mut select = ShellEvent::user_input_intercepted("session-1", "/select 1");
        select.component = Some("slash".to_string());
        let mut copy = ShellEvent::user_input_intercepted("session-1", "/copy 1");
        copy.component = Some("slash".to_string());
        let mut approve = ShellEvent::user_input_intercepted("session-1", "/approve 2");
        approve.component = Some("slash".to_string());
        let mut deny = ShellEvent::user_input_intercepted("session-1", "/deny 2");
        deny.component = Some("slash".to_string());

        assert_eq!(recommendation_action_from_event(&allow), None);
        assert_eq!(
            recommendation_action_from_event(&select),
            Some(RecommendationAction {
                kind: RecommendationActionKind::Select,
                index: 1,
            })
        );
        assert_eq!(
            recommendation_action_from_event(&copy),
            Some(RecommendationAction {
                kind: RecommendationActionKind::Copy,
                index: 1,
            })
        );
        assert_eq!(recommendation_action_from_event(&approve), None);
        assert_eq!(recommendation_action_from_event(&deny), None);
    }

    #[test]
    fn parses_recommendation_actions_from_card_events() {
        let mut copy = ShellEvent::user_input_intercepted("session-1", "2");
        copy.component = Some("card".to_string());
        copy.message = Some("recommendation_copy".to_string());
        let mut insert = ShellEvent::user_input_intercepted("session-1", "3");
        insert.component = Some("card".to_string());
        insert.message = Some("recommendation_insert".to_string());
        let mut details = ShellEvent::user_input_intercepted("session-1", "1");
        details.component = Some("card".to_string());
        details.message = Some("recommendation_details".to_string());

        assert_eq!(
            recommendation_action_from_event(&copy),
            Some(RecommendationAction {
                kind: RecommendationActionKind::Copy,
                index: 2,
            })
        );
        assert_eq!(
            recommendation_action_from_event(&insert),
            Some(RecommendationAction {
                kind: RecommendationActionKind::Insert,
                index: 3,
            })
        );
        assert_eq!(
            recommendation_action_from_event(&details),
            Some(RecommendationAction {
                kind: RecommendationActionKind::Details,
                index: 1,
            })
        );
    }

    #[test]
    fn parses_approval_commands_from_card_events() {
        let mut approve = ShellEvent::user_input_intercepted("session-1", "req-1");
        approve.component = Some("card".to_string());
        approve.message = Some("approve".to_string());
        let mut deny = ShellEvent::user_input_intercepted("session-1", "req-3");
        deny.component = Some("card".to_string());
        deny.message = Some("deny".to_string());
        let mut details = ShellEvent::user_input_intercepted("session-1", "/details req-4");
        details.component = Some("slash".to_string());
        let mut cancel = ShellEvent::user_input_intercepted("session-1", "req-5");
        cancel.component = Some("card".to_string());
        cancel.message = Some("cancel".to_string());
        let mut send_to_shell = ShellEvent::user_input_intercepted("session-1", "handoff-1");
        send_to_shell.component = Some("card".to_string());
        send_to_shell.message = Some("send_to_shell".to_string());
        let mut recommendation = ShellEvent::user_input_intercepted("session-1", "/approve 2");
        recommendation.component = Some("slash".to_string());

        assert_eq!(
            approval_command_from_event(&approve),
            Some(ApprovalCommand {
                kind: ApprovalCommandKind::Approve,
                id: "req-1".to_string(),
            })
        );
        assert_eq!(
            approval_command_from_event(&deny),
            Some(ApprovalCommand {
                kind: ApprovalCommandKind::Deny,
                id: "req-3".to_string(),
            })
        );
        assert_eq!(
            approval_command_from_event(&details),
            Some(ApprovalCommand {
                kind: ApprovalCommandKind::Details,
                id: "req-4".to_string(),
            })
        );
        assert_eq!(
            approval_command_from_event(&cancel),
            Some(ApprovalCommand {
                kind: ApprovalCommandKind::Cancel,
                id: "req-5".to_string(),
            })
        );
        assert_eq!(
            approval_command_from_event(&send_to_shell),
            Some(ApprovalCommand {
                kind: ApprovalCommandKind::SendToShell,
                id: "handoff-1".to_string(),
            })
        );
        assert_eq!(approval_command_from_event(&recommendation), None);
    }

    #[test]
    fn consultation_card_events_are_not_approval_commands() {
        let mut analyze =
            ShellEvent::user_input_intercepted("session-1", "consultation-hook-cmd-3");
        analyze.component = Some("card".to_string());
        analyze.message = Some("approve".to_string());

        assert_eq!(approval_command_from_event(&analyze), None);
    }

    #[test]
    fn parses_agent_cancel_slash_event() {
        let mut cancel = ShellEvent::user_input_intercepted("session-1", "/cancel");
        cancel.component = Some("slash".to_string());
        let mut ctrl_c = ShellEvent::user_input_intercepted("session-1", "ctrl_c");
        ctrl_c.component = Some("control".to_string());
        let mut agent_cancel = ShellEvent::user_input_intercepted("session-1", "agent-request-1");
        agent_cancel.component = Some("card".to_string());
        agent_cancel.message = Some("agent_cancel".to_string());
        let mut approval_cancel = ShellEvent::user_input_intercepted("session-1", "req-1");
        approval_cancel.component = Some("card".to_string());
        approval_cancel.message = Some("cancel".to_string());
        let mut clear = ShellEvent::user_input_intercepted("session-1", "/clear");
        clear.component = Some("slash".to_string());

        assert!(event_requests_agent_cancel(&cancel));
        assert!(event_requests_agent_cancel(&ctrl_c));
        assert!(event_requests_agent_cancel(&agent_cancel));
        assert!(!event_requests_agent_cancel(&approval_cancel));
        assert!(!event_requests_agent_cancel(&clear));
    }
}
