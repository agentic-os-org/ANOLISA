use super::*;
use crate::agent::skill_context::finalize_agent_request_skill_context;
use crate::diagnostics::health::{
    HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue, HealthScanReport,
    HealthSeverity,
};
use crate::types::STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT;

#[test]
fn only_insight_origins_use_strict_result_presentation() {
    assert!(!AgentRunOrigin::Standard.is_insight_triggered());
    assert!(AgentRunOrigin::InsightPrompt.is_insight_triggered());
    assert!(AgentRunOrigin::AutoFailure.is_insight_triggered());
}

#[test]
fn final_insight_context_never_exceeds_provider_budget() {
    let mut request = test_agent_request();
    request.context_hints = vec![
        "x".repeat(crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES),
        "insight_evidence\ntarget_facts:\ncommand_id=cmd-1".to_string(),
    ];

    finalize_agent_request_skill_context(&mut request, None);
    enforce_insight_context_budget(&mut request);

    assert!(
        serialized_context_hint_bytes(&request.context_hints)
            <= crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES
    );
    assert_eq!(request.context_hints.len(), 1);
    assert!(request.context_hints[0].starts_with("insight_evidence\n"));
}

#[test]
fn malformed_oversized_insight_payload_is_utf8_safely_bounded() {
    let mut request = test_agent_request();
    request.context_hints = vec![format!(
        "insight_evidence\n{}",
        "界".repeat(crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES)
    )];

    enforce_insight_context_budget(&mut request);

    assert!(
        serialized_context_hint_bytes(&request.context_hints)
            <= crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES
    );
    assert!(request.context_hints[0].is_char_boundary(request.context_hints[0].len()));
}

#[test]
fn health_context_hint_is_not_attached_to_free_form_request() {
    let report = test_health_report();
    let mut request = test_agent_request();

    finalize_agent_request_skill_context(&mut request, Some(&report));

    assert!(!request
        .context_hints
        .iter()
        .any(|hint| hint.starts_with("health_scan ")));
}

#[test]
fn health_context_hint_is_attached_to_startup_health_follow_up() {
    let report = test_health_report();
    let mut request = test_agent_request();
    request
        .context_hints
        .push(STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT.to_string());

    finalize_agent_request_skill_context(&mut request, Some(&report));

    let hint = request
        .context_hints
        .iter()
        .find(|hint| hint.starts_with("health_scan "))
        .expect("health context hint");
    assert!(hint.contains("scan_id=health-1"), "{hint}");
    assert!(hint.contains("overall_severity=warning"), "{hint}");
    assert!(hint.contains("bounded_facts_only=true"), "{hint}");
    assert!(hint.contains("no_collector_stdout=true"), "{hint}");
    assert!(!hint.contains("/tmp/cosh"), "{hint}");
}

#[test]
fn health_context_hint_dedupes_existing_health_hint() {
    let report = test_health_report();
    let mut request = test_agent_request();
    request
        .context_hints
        .push(STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT.to_string());
    request
        .context_hints
        .push("health_scan scan_id=existing".to_string());

    finalize_agent_request_skill_context(&mut request, Some(&report));

    assert_eq!(
        request
            .context_hints
            .iter()
            .filter(|hint| hint.starts_with("health_scan "))
            .count(),
        1
    );
}

fn test_health_report() -> HealthScanReport {
    let mut report = HealthScanReport::new("health-1", 0);
    report.overall_severity = HealthSeverity::Warning;
    report.facts.push(HealthFact {
        id: "memory.available_ratio".to_string(),
        category: HealthFactCategory::Memory,
        key: "memory.available_ratio".to_string(),
        value: HealthFactValue::Float(0.08),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    });
    report
}

fn test_agent_request() -> AgentRequest {
    AgentRequest {
        id: "agent-request-health".to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "分析一下这台机器内存风险".to_string(),
            origin: CommandOrigin::UserInteractive,
            cwd: "/repo".to_string(),
            end_cwd: "/repo".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
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
        user_input: Some("分析一下这台机器内存风险".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

struct FailedWriter;

impl std::io::Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "test writer failed",
        ))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn accepted_request_remains_owned_when_initial_output_fails() {
    let request = test_agent_request();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let result = start_agent_run_with_origin_disposition(
        &request,
        AgentRunOrigin::Standard,
        AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut FailedWriter,
        None,
    );
    assert!(result.is_err());
    assert_eq!(
        state
            .agent_run
            .active
            .as_ref()
            .map(|active| active.request.id.as_str()),
        Some(request.id.as_str())
    );
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn deferred_acceptance_is_pure_and_activation_keeps_fifo_while_busy_or_compacting() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut first = test_agent_request();
    first.id = "earlier".into();
    let mut second = first.clone();
    second.id = "initial".into();
    second.recommended_skill = Some("must remain untouched until activation".into());
    assert_eq!(
        accept_deferred_agent_run(&first, &mut state),
        AgentStartDisposition::Queued
    );
    assert_eq!(
        accept_deferred_agent_run(&second, &mut state),
        AgentStartDisposition::Queued
    );
    assert!(state.agent_run.active.is_none());
    assert_eq!(
        state
            .agent_run
            .queued_requests
            .back()
            .unwrap()
            .request
            .recommended_skill,
        second.recommended_skill
    );
    let queue_ids = |state: &InlineState| {
        state
            .agent_run
            .queued_requests
            .iter()
            .map(|pending| pending.request.id.clone())
            .collect::<Vec<_>>()
    };
    let expected = vec!["earlier".to_string(), "initial".to_string()];
    let (active, _receiver) = test_support::test_active_run_with_id("busy");
    state.agent_run.active = Some(active);
    assert_eq!(activate_deferred_agent_run(&adapter, &mut state), None);
    assert_eq!(queue_ids(&state), expected);
    state.agent_run.active = None;
    crate::slash::session::note_compaction_recommendation(
        &mut state,
        "11111111-1111-4111-8111-111111111111:1:1:100:20",
    );
    assert!(crate::slash::session::compaction_pending_or_active(&state));
    assert_eq!(activate_deferred_agent_run(&adapter, &mut state), None);
    assert_eq!(queue_ids(&state), expected);
    let mut idle_state = InlineState::default();
    idle_state.agent_run.queued_requests = std::mem::take(&mut state.agent_run.queued_requests);
    assert_eq!(
        activate_deferred_agent_run(&adapter, &mut idle_state),
        Some(AgentStartDisposition::Started)
    );
    assert_eq!(
        idle_state.agent_run.active.as_ref().unwrap().request.id,
        "earlier"
    );
    assert_eq!(queue_ids(&idle_state), vec!["initial".to_string()]);
}

fn ready_run(events: Vec<AgentEvent>) -> InlineState {
    let mut state = InlineState::default();
    let (mut active, _approval_receiver) = test_support::test_active_run_with_id("accepted-owner");
    active.handle = AgentRunHandle::test_with_events(events);
    state.agent_run.active = Some(active);
    for id in ["queued-first", "queued-second"] {
        let mut request = test_agent_request();
        request.id = id.into();
        assert_eq!(
            accept_deferred_agent_run(&request, &mut state),
            AgentStartDisposition::Queued
        );
    }
    state
}

fn assert_one_runtime_retirement(
    state: &mut InlineState,
    output: &mut Vec<u8>,
    adapter: &AdapterInstance,
) {
    let active = state
        .agent_run
        .active
        .as_ref()
        .expect("caller still owns the accepted run");
    assert_eq!(active.request.id, "accepted-owner");
    assert!(active.completed);
    assert_eq!(state.agent_run.queued_requests.len(), 2);
    crate::agent::poll::poll_active_agent_run(state, output, adapter).unwrap();
    assert_eq!(
        state.agent_run.active.as_ref().unwrap().request.id,
        "queued-first"
    );
    assert_eq!(state.agent_run.queued_requests.len(), 1);
    assert_eq!(
        state.agent_run.queued_requests.front().unwrap().request.id,
        "queued-second"
    );
}

#[test]
fn initial_poll_keeps_terminal_body_and_advances_fifo_once_on_next_poll() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = ready_run(vec![
        AgentEvent::TextDelta {
            run_id: "accepted-owner".into(),
            text: "visible first body\n\n".into(),
        },
        AgentEvent::AgentCompleted {
            run_id: "accepted-owner".into(),
            summary: "finished once".into(),
        },
    ]);
    let mut output = Vec::new();
    poll_accepted_agent_run(&mut state, &mut output, &adapter).unwrap();
    let active = state
        .agent_run
        .active
        .as_ref()
        .expect("accepted terminal owner");
    assert_eq!(
        active
            .governed_events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TextDelta { .. }))
            .count(),
        1
    );
    assert_eq!(
        active
            .governed_events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::AgentCompleted { .. }))
            .count(),
        1
    );
    assert!(String::from_utf8_lossy(&output).contains("visible first body"));
    assert_one_runtime_retirement(&mut state, &mut output, &adapter);
    assert_eq!(
        String::from_utf8_lossy(&output)
            .matches("visible first body")
            .count(),
        1
    );
}

#[test]
fn initial_poll_keeps_disconnected_owner_and_next_poll_advances_once() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = ready_run(Vec::new());
    let mut output = Vec::new();
    poll_accepted_agent_run(&mut state, &mut output, &adapter).unwrap();
    assert_one_runtime_retirement(&mut state, &mut output, &adapter);
}

#[test]
fn initial_poll_processes_auth_without_consuming_events_past_its_boundary() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = ready_run(vec![
        AgentEvent::AuthRequired {
            run_id: "accepted-owner".into(),
            request_id: "auth-start".into(),
            reason: "credentials required".into(),
            error_message: None,
            providers: Vec::new(),
        },
        AgentEvent::AgentCompleted {
            run_id: "accepted-owner".into(),
            summary: "not polled past auth".into(),
        },
    ]);
    let mut output = Vec::new();
    poll_accepted_agent_run(&mut state, &mut output, &adapter).unwrap();
    assert_eq!(state.auth.state.as_ref().unwrap().request_id, "auth-start");
    let active = state.agent_run.active.as_ref().unwrap();
    assert_eq!(active.request.id, "accepted-owner");
    assert!(!active.completed);
    assert_eq!(active.governed_events.len(), 1);
    assert!(matches!(
        active.governed_events[0].event,
        AgentEvent::AuthRequired { .. }
    ));
    assert_eq!(state.agent_run.queued_requests.len(), 2);
    assert!(!output.is_empty());
}
