use std::sync::Arc;

use agentsight_audit::{AuditService, AuditStore, RiskCase, RiskCaseStatus, RiskSeverity};
use agentsight_enforcement_protocol::{
    DestinationClass, Effect, EventIdentity, FileAction, FileDecision, NetworkAction,
    NetworkDirection, PolicyDecision, PolicyMode, SecurityEvent, SecurityEventKind,
    TaintTransition, TaintTransitionKind,
};
use uuid::Uuid;

#[test]
fn service_accepts_an_independent_store() {
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let service = AuditService::new(Arc::clone(&store));

    assert!(Arc::ptr_eq(service.store(), &store));
}

#[test]
fn service_exposes_retention_cleanup_for_server_scheduling() {
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let service = AuditService::new(store);

    assert_eq!(service.purge_before(10).expect("purge should run"), 0);
}

fn sample_case(agent_id: &str, policy_revision: u64, updated_at_ns: u64) -> RiskCase {
    let case_id = Uuid::new_v4();
    RiskCase {
        case_id,
        correlation_key: format!("case-{case_id}"),
        policy_id: "credential-exfiltration".into(),
        policy_revision,
        agent_id: agent_id.into(),
        session_id: Some("session-1".into()),
        severity: RiskSeverity::High,
        risk_score: 85,
        status: RiskCaseStatus::Open,
        blocked: false,
        opened_at_ns: 1,
        updated_at_ns,
        summary: "credential reached an untrusted target".into(),
    }
}

#[test]
fn service_case_queries_delegate_to_store_with_agent_filter() {
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let alpha = sample_case("agent-alpha", 3, 100);
    let alpha_id = alpha.case_id;
    let beta = sample_case("agent-beta", 7, 200);
    store
        .upsert_case(&alpha, &[])
        .expect("alpha case should persist");
    store
        .upsert_case(&beta, &[])
        .expect("beta case should persist");
    let service = AuditService::new(Arc::clone(&store));

    assert_eq!(
        service
            .case_count(None, None, None)
            .expect("total should load"),
        2
    );
    assert_eq!(
        service
            .case_count(Some("agent-alpha"), None, None)
            .expect("alpha total should load"),
        1
    );

    let alpha_cases = service
        .cases(10, 0, Some("agent-alpha"), None, None)
        .expect("alpha page should load");
    assert_eq!(alpha_cases.len(), 1);
    assert_eq!(alpha_cases[0].agent_id, "agent-alpha");

    let index = service
        .case_index_by_agent_policy()
        .expect("index should build");
    assert_eq!(
        index.get(&(
            "agent-alpha".to_string(),
            "credential-exfiltration".to_string(),
            "3".to_string()
        )),
        Some(&alpha_id)
    );
}

#[test]
fn ingest_keeps_every_chained_transition_beyond_one_page() {
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let service = AuditService::new(Arc::clone(&store));
    let binding_id = Uuid::new_v4();

    let identity = |pid: i32| EventIdentity {
        binding_id,
        agent_id: "agent".into(),
        agent_name: None,
        session_id: Some("session".into()),
        conversation_id: None,
        tool_call_id: None,
        pid,
        process_start_time: pid as u64 * 10,
        ppid: None,
        cgroup_id: None,
        protocol_version: 1,
        enforcer_version: "test".into(),
        actplane_revision: "test".into(),
    };
    let event = |pid: i32, kind: SecurityEventKind, time: u64| SecurityEvent {
        event_id: Uuid::new_v4(),
        occurred_at_ns: time,
        observed_at_ns: time,
        identity: identity(pid),
        kind,
    };

    let source = event(
        10,
        SecurityEventKind::FileAction(FileAction {
            policy_id: "policy".into(),
            policy_revision: 1,
            operation: "read".into(),
            path: "/secret".into(),
            resource_class: "credential".into(),
            succeeded: true,
            errno: None,
            rule_id: None,
        }),
        1,
    );
    let sink = event(
        1011,
        SecurityEventKind::NetworkAction(NetworkAction {
            policy_id: "policy".into(),
            policy_revision: 1,
            direction: NetworkDirection::Outbound,
            destination: "203.0.113.10:443".into(),
            destination_class: DestinationClass::Public,
            protocol: "tcp".into(),
            succeeded: true,
            errno: None,
            rule_id: None,
        }),
        1_003,
    );

    // One inherit chain of 1_001 transitions: pid 10 -> 11 -> ... -> 1011.
    // The correlation query is store-bounded to 1_000 newest rows, so the
    // first hop off the source is the row a single page drops first.
    let transitions: Vec<SecurityEvent> = (1..=1_001)
        .map(|step| {
            event(
                10 + step,
                SecurityEventKind::TaintTransition(TaintTransition {
                    policy_id: "policy".into(),
                    policy_revision: 1,
                    label: "SENSITIVE".into(),
                    transition: TaintTransitionKind::Inherit,
                    source_pid: 9 + step,
                    source_process_start_time: (9 + step) as u64 * 10,
                    target_pid: 10 + step,
                    target_process_start_time: (10 + step) as u64 * 10,
                    reason: "inherit".into(),
                }),
                1 + step as u64,
            )
        })
        .collect();
    let decision = event(
        10,
        SecurityEventKind::PolicyDecision(PolicyDecision {
            policy_id: "policy".into(),
            policy_revision: 1,
            source_event_id: source.event_id,
            sink_event_id: sink.event_id,
            mode: PolicyMode::Enforce,
            requested_effect: Effect::Block,
            blocked: true,
            killed: false,
            errno: None,
            risk_score: 90,
            reason: "credential reached an untrusted target".into(),
        }),
        1_004,
    );

    service
        .ingest(source.clone())
        .expect("source should ingest");
    for transition in &transitions {
        service
            .ingest(transition.clone())
            .expect("transition should ingest");
    }
    service.ingest(sink.clone()).expect("sink should ingest");
    let case_id = decision.event_id;
    let decision_id = case_id;
    service.ingest(decision).expect("decision should ingest");

    let detail = service.case(case_id).expect("case should load");
    let evidence_ids: Vec<Uuid> = detail.evidence.iter().map(|item| item.event_id).collect();
    assert_eq!(
        evidence_ids.len(),
        transitions.len() + 3,
        "evidence must keep every chained transition, not only the newest 1_000"
    );
    for transition in &transitions {
        assert!(
            evidence_ids.contains(&transition.event_id),
            "transition {} must stay on the evidence chain",
            transition.event_id
        );
    }
    assert_eq!(evidence_ids.first(), Some(&source.event_id));
    assert_eq!(evidence_ids.last(), Some(&decision_id));
}

fn file_guard_decision() -> FileDecision {
    FileDecision {
        policy_id: "file-deletion-policy".into(),
        policy_revision: 4,
        operation: "unlink".into(),
        path: "~/reports".into(),
        resource_class: "protected_path".into(),
        rule_id: Some("deny-delete".into()),
        mode: PolicyMode::Enforce,
        requested_effect: Effect::Block,
        blocked: true,
        killed: false,
        errno: Some(1),
        risk_score: 90,
        reason: "protected path deletion denied".into(),
    }
}

fn guard_identity(binding_id: Uuid) -> EventIdentity {
    EventIdentity {
        binding_id,
        agent_id: "agent".into(),
        agent_name: None,
        session_id: None,
        conversation_id: None,
        tool_call_id: None,
        pid: 42,
        process_start_time: 420,
        ppid: Some(1),
        cgroup_id: None,
        protocol_version: 1,
        enforcer_version: "test".into(),
        actplane_revision: "test".into(),
    }
}

#[test]
fn ingest_maps_a_blocked_file_operation_to_a_risk_case() {
    // A file-deletion guard hit used to stay inside the enforcement stream, so
    // the audit store never correlated it into a risk case and the violation
    // row had no `case_id` to link to. The emitted pair must close that gap:
    // the file event is the evidence a violation carries, and the decision
    // event is what the correlation turns into a case.
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let service = AuditService::new(Arc::clone(&store));

    let [file_event, decision_event] = SecurityEvent::file_operation_decision(
        guard_identity(Uuid::new_v4()),
        1_000,
        1_000,
        file_guard_decision(),
    );
    let file_event_id = file_event.event_id;
    let case_id = decision_event.event_id;

    service
        .ingest(file_event)
        .expect("file evidence should persist");
    // Ingestion fails closed while the referenced evidence is missing, so the
    // pair is only correlatable in this order.
    service
        .ingest(decision_event)
        .expect("decision should correlate into a case");

    let links = service
        .case_ids_for_events(&[file_event_id])
        .expect("evidence links should load");
    assert_eq!(
        links.get(&file_event_id),
        Some(&case_id),
        "the violation's event id must resolve to the case it opened"
    );

    let detail = service.case(case_id).expect("case detail should load");
    let evidence: Vec<Uuid> = detail.evidence.iter().map(|item| item.event_id).collect();
    assert!(
        evidence.contains(&file_event_id),
        "the blocked file operation must be immutable case evidence: {evidence:?}"
    );
}

#[test]
fn a_blocked_file_operation_needs_its_file_event_first() {
    // The decision alone is not evidence: correlating it without the file event
    // must fail loudly rather than open a case that cannot be traced back.
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let service = AuditService::new(store);

    let [_, decision_event] = SecurityEvent::file_operation_decision(
        guard_identity(Uuid::new_v4()),
        1_000,
        1_000,
        file_guard_decision(),
    );

    let error = service
        .ingest(decision_event)
        .expect_err("missing evidence must not open a case");
    assert!(
        format!("{error}").contains("missing source event"),
        "the error must name the missing evidence role and id: {error}"
    );
}

#[test]
fn a_file_operation_without_its_decision_opens_no_case() {
    // Only a policy decision correlates into a risk case. A file event on its
    // own — the shape the enforcement stream produced for a blocked unlink
    // before this change — therefore leaves the case table empty, which is why
    // the dashboard had no evidence to link to.
    let store = Arc::new(AuditStore::open_in_memory().expect("in-memory audit store should open"));
    let service = AuditService::new(Arc::clone(&store));

    let [file_event, _] = SecurityEvent::file_operation_decision(
        guard_identity(Uuid::new_v4()),
        1_000,
        1_000,
        file_guard_decision(),
    );
    let file_event_id = file_event.event_id;
    service
        .ingest(file_event)
        .expect("file evidence should persist");

    assert_eq!(
        service
            .case_count(None, None, None)
            .expect("count should load"),
        0,
        "a file event alone must not fabricate a case"
    );
    assert!(
        service
            .case_ids_for_events(&[file_event_id])
            .expect("evidence links should load")
            .is_empty(),
        "no case means no link for a violation to follow"
    );
}
