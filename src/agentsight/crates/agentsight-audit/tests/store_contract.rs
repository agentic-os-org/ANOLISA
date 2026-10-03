use std::fs;

use agentsight_audit::{
    AuditError as SecurityStoreError, AuditEventFilter as SecurityEventFilter,
    AuditMaintenancePolicy, AuditStore as SecurityStore, ContainmentAction,
    ContainmentActivationResult, ContainmentClaimResult, ContainmentFailureStage,
    ContainmentLifecycle, RiskCase, RiskCaseStatus, RiskSeverity,
};
use agentsight_enforcement_protocol::{
    DestinationClass, Effect, EventIdentity, FileAction, NetworkAction, NetworkDirection,
    PolicyDecision, PolicyMode, SecurityEvent, SecurityEventKind, TaintTransition,
    TaintTransitionKind,
};
use agentsight_sqlite_lifecycle::MaintenanceStatus;
use uuid::Uuid;

fn containment_action(lifecycle_state: ContainmentLifecycle) -> ContainmentAction {
    ContainmentAction {
        action_id: Uuid::new_v4(),
        case_id: Uuid::new_v4(),
        binding_id: Uuid::new_v4(),
        source_binding_id: Some(Uuid::new_v4()),
        agent_id: "hermes-test".into(),
        root_pid: 4242,
        process_start_time: 99,
        source_path: "/home/test/.ssh/id_rsa".into(),
        duration_secs: Some(900),
        expires_at_ns: Some(1_000),
        lifecycle_state,
        blocked_at_ns: None,
        requested_by: "principal:test-operator".into(),
        failure_stage: None,
        failure_reason: None,
        attempt_count: 0,
        next_retry_at_ns: None,
        created_at_ns: 100,
        updated_at_ns: 100,
    }
}

fn fixture_case(case_id: Uuid, status: RiskCaseStatus) -> RiskCase {
    RiskCase {
        case_id,
        correlation_key: format!("case-{case_id}"),
        policy_id: "credential-exfiltration".into(),
        policy_revision: 3,
        agent_id: "hermes-test".into(),
        session_id: Some("session-1".into()),
        severity: RiskSeverity::High,
        risk_score: 85,
        status,
        blocked: false,
        opened_at_ns: 1,
        updated_at_ns: 1,
        summary: "credential reached an untrusted target".into(),
    }
}

fn security_db_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("agentsight-{label}-{}.db", Uuid::new_v4()))
}

fn fixture_file_action(path: &str, occurred_at_ns: u64) -> SecurityEvent {
    SecurityEvent {
        event_id: Uuid::new_v4(),
        occurred_at_ns,
        observed_at_ns: occurred_at_ns.saturating_add(1),
        identity: EventIdentity {
            binding_id: Uuid::new_v4(),
            agent_id: "hermes-test".into(),
            agent_name: Some("Hermes".into()),
            session_id: Some("session-1".into()),
            conversation_id: None,
            tool_call_id: Some("tool-call-1".into()),
            pid: 4242,
            process_start_time: 99,
            ppid: Some(42),
            cgroup_id: None,
            protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
            enforcer_version: "test".into(),
            actplane_revision: "test".into(),
        },
        kind: SecurityEventKind::FileAction(FileAction {
            policy_id: "credential-exfiltration".into(),
            policy_revision: 3,
            operation: "read".into(),
            path: path.into(),
            resource_class: "credential".into(),
            succeeded: true,
            errno: None,
            rule_id: Some("credential-source".into()),
        }),
    }
}

fn fixture_policy_decision(occurred_at_ns: u64, blocked: bool) -> SecurityEvent {
    let mut event = fixture_file_action("~/.ssh/id_rsa", occurred_at_ns);
    event.kind = SecurityEventKind::PolicyDecision(PolicyDecision {
        policy_id: "credential-exfiltration".into(),
        policy_revision: 3,
        source_event_id: Uuid::new_v4(),
        sink_event_id: Uuid::new_v4(),
        mode: PolicyMode::Enforce,
        requested_effect: Effect::Block,
        blocked,
        killed: false,
        errno: blocked.then_some(libc::EPERM),
        risk_score: 85,
        reason: "credential taint reached a public endpoint".into(),
    });
    event
}

fn fixture_network_action(occurred_at_ns: u64, blocked: bool) -> SecurityEvent {
    let mut event = fixture_file_action("~/.ssh/id_rsa", occurred_at_ns);
    event.kind = SecurityEventKind::NetworkAction(NetworkAction {
        policy_id: "credential-exfiltration".into(),
        policy_revision: 3,
        direction: NetworkDirection::Outbound,
        destination: "198.51.100.10".into(),
        destination_class: DestinationClass::Public,
        protocol: "tcp".into(),
        succeeded: !blocked,
        errno: blocked.then_some(libc::EPERM),
        rule_id: Some("credential-public-sink".into()),
    });
    event
}

fn fixture_taint_transition(occurred_at_ns: u64) -> SecurityEvent {
    let mut event = fixture_file_action("~/.ssh/id_rsa", occurred_at_ns);
    event.kind = SecurityEventKind::TaintTransition(TaintTransition {
        policy_id: "credential-exfiltration".into(),
        policy_revision: 3,
        label: "CREDENTIAL".into(),
        transition: TaintTransitionKind::Add,
        source_pid: 4242,
        source_process_start_time: 99,
        target_pid: 4242,
        target_process_start_time: 99,
        reason: "credential source read".into(),
    });
    event
}

#[test]
fn retention_purges_a_terminal_case_graph_atomically() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let case_id = Uuid::new_v4();
    let mut case = fixture_case(case_id, RiskCaseStatus::FalsePositive);
    case.updated_at_ns = 5;
    let first = fixture_file_action("~/.ssh/id_rsa", 1);
    let second = fixture_network_action(2, false);
    store.insert_event(&first).expect("event should insert");
    store.insert_event(&second).expect("event should insert");
    store
        .upsert_case(&case, &[first.event_id, second.event_id])
        .expect("case graph should persist");
    let mut action = containment_action(ContainmentLifecycle::Failed);
    action.case_id = case_id;
    action.updated_at_ns = 5;
    store
        .insert_containment_action(&action)
        .expect("terminal action should persist");

    let deleted = store.purge_before(10).expect("retention should succeed");

    assert!(deleted >= 6, "the complete terminal graph must be deleted");
    assert!(matches!(
        store.case_detail(case_id),
        Err(SecurityStoreError::MissingCase(id)) if id == case_id
    ));
    assert_eq!(
        store
            .event(first.event_id)
            .expect("event query should work"),
        None
    );
    assert_eq!(
        store
            .event(second.event_id)
            .expect("event query should work"),
        None
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work"),
        None
    );
}

#[test]
fn retention_preserves_active_containment_and_all_evidence() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let case_id = Uuid::new_v4();
    let mut case = fixture_case(case_id, RiskCaseStatus::Confirmed);
    case.updated_at_ns = 5;
    let event = fixture_file_action("~/.ssh/id_rsa", 1);
    store.insert_event(&event).expect("event should insert");
    store
        .upsert_case(&case, &[event.event_id])
        .expect("case graph should persist");
    let mut action = containment_action(ContainmentLifecycle::Active);
    action.case_id = case_id;
    action.updated_at_ns = 5;
    store
        .insert_containment_action(&action)
        .expect("active action should persist");

    store.purge_before(10).expect("retention should succeed");

    let detail = store
        .case_detail(case_id)
        .expect("active case must survive");
    assert_eq!(detail.evidence, vec![event.clone()]);
    assert_eq!(
        store
            .event(event.event_id)
            .expect("event query should work"),
        Some(event)
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work"),
        Some(action)
    );
}

#[test]
fn retention_preserves_open_and_confirmed_case_graphs_without_containment() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut expected = Vec::new();
    for (index, status) in [RiskCaseStatus::Open, RiskCaseStatus::Confirmed]
        .into_iter()
        .enumerate()
    {
        let case_id = Uuid::new_v4();
        let mut case = fixture_case(case_id, status);
        case.updated_at_ns = 5;
        let event = fixture_file_action(&format!("~/.ssh/id_rsa-{index}"), index as u64 + 1);
        store.insert_event(&event).expect("event should insert");
        store
            .upsert_case(&case, &[event.event_id])
            .expect("active case graph should persist");
        expected.push((case_id, event));
    }

    store.purge_before(10).expect("retention should succeed");

    for (case_id, event) in expected {
        assert_eq!(
            store
                .case_detail(case_id)
                .expect("active case graph must survive")
                .evidence,
            vec![event]
        );
    }
}

#[test]
fn retention_preserves_a_recent_terminal_action_and_its_case_graph() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let case_id = Uuid::new_v4();
    let event = fixture_file_action("~/.ssh/id_rsa", 1);
    store.insert_event(&event).expect("event should insert");
    store
        .upsert_case(
            &fixture_case(case_id, RiskCaseStatus::Resolved),
            &[event.event_id],
        )
        .expect("case graph should persist");
    let mut action = containment_action(ContainmentLifecycle::Failed);
    action.case_id = case_id;
    action.updated_at_ns = 20;
    store
        .insert_containment_action(&action)
        .expect("recent terminal action should persist");

    store.purge_before(10).expect("retention should succeed");

    assert_eq!(
        store
            .case_detail(case_id)
            .expect("recent graph must survive")
            .evidence,
        vec![event]
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work"),
        Some(action)
    );
}

#[test]
fn retention_keeps_shared_evidence_until_its_last_case_is_purged() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let event = fixture_file_action("~/.ssh/id_rsa", 1);
    store.insert_event(&event).expect("event should insert");
    let old_id = Uuid::new_v4();
    let mut old_case = fixture_case(old_id, RiskCaseStatus::FalsePositive);
    old_case.updated_at_ns = 5;
    let retained_id = Uuid::new_v4();
    let mut retained_case = fixture_case(retained_id, RiskCaseStatus::AcceptedRisk);
    retained_case.updated_at_ns = 20;
    store
        .upsert_case(&old_case, &[event.event_id])
        .expect("old case should persist");
    store
        .upsert_case(&retained_case, &[event.event_id])
        .expect("retained case should persist");

    store
        .purge_before(10)
        .expect("first retention pass should succeed");
    assert_eq!(
        store
            .event(event.event_id)
            .expect("event query should work"),
        Some(event.clone())
    );
    assert_eq!(
        store
            .case_detail(retained_id)
            .expect("retained case should stay complete")
            .evidence,
        vec![event.clone()]
    );

    store
        .purge_before(30)
        .expect("second retention pass should succeed");
    assert_eq!(
        store
            .event(event.event_id)
            .expect("event query should work"),
        None
    );
}

#[test]
fn maintenance_honors_disabled_and_enabled_retention() {
    let path = security_db_path("maintenance-retention");
    let store = SecurityStore::open(&path).expect("fixture store should open");
    let event = fixture_file_action("~/.ssh/id_rsa", 1);
    store.insert_event(&event).expect("event should insert");

    let disabled = store
        .maintain(AuditMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 0,
        })
        .expect("disabled maintenance should succeed");
    assert_eq!(disabled.expired_rows, 0);
    assert_eq!(disabled.size.status, MaintenanceStatus::Disabled);
    assert!(store.event(event.event_id).unwrap().is_some());

    let enabled = store
        .maintain(AuditMaintenancePolicy {
            retention_days: 1,
            max_db_size_mb: 0,
        })
        .expect("retention maintenance should succeed");
    assert_eq!(enabled.expired_rows, 1);
    assert!(store.event(event.event_id).unwrap().is_none());

    drop(store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn maintenance_stops_size_pruning_when_retention_checkpoint_is_busy() {
    let path = security_db_path("maintenance-checkpoint-busy");
    let store = SecurityStore::open(&path).expect("fixture store should open");
    for index in 0..100 {
        let event = fixture_file_action(&format!("/tmp/{}-{index}", "x".repeat(20_000)), 1);
        store.insert_event(&event).expect("event should insert");
    }
    let retained = fixture_file_action("/tmp/current", 1_800_000_000_000_000_000);
    store
        .insert_event(&retained)
        .expect("current event should insert");

    let reader = rusqlite::Connection::open(&path).expect("reader should open");
    reader
        .execute_batch("BEGIN; SELECT COUNT(*) FROM security_events;")
        .expect("reader should hold a snapshot");

    let report = store
        .maintain(AuditMaintenancePolicy {
            retention_days: 1,
            max_db_size_mb: 1,
        })
        .expect("maintenance should stop safely");

    assert_eq!(report.size.status, MaintenanceStatus::CheckpointBusy);
    assert!(store.event(retained.event_id).unwrap().is_some());
    drop(reader);
    drop(store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn maintenance_enforces_size_for_unreferenced_events() {
    let path = security_db_path("maintenance-size");
    let store = SecurityStore::open(&path).expect("fixture store should open");
    let mut newest_id = None;
    for index in 0..100 {
        let mut event = fixture_file_action(&"x".repeat(20_000), index + 1);
        event.identity.tool_call_id = Some(format!("tool-{index}"));
        newest_id = Some(event.event_id);
        store.insert_event(&event).expect("event should insert");
    }

    let report = store
        .maintain(AuditMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .expect("size maintenance should succeed");

    assert!(report.size.deleted_rows > 0);
    assert!(matches!(
        report.size.status,
        MaintenanceStatus::TargetReached | MaintenanceStatus::MaxRounds
    ));
    assert!(
        store
            .event(newest_id.expect("fixture inserts at least one event"))
            .unwrap()
            .is_some(),
        "oldest-first maintenance should retain the newest event"
    );

    drop(store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn case_detail_rejects_a_dangling_evidence_link() {
    let path = security_db_path("dangling-evidence");
    let case_id = Uuid::new_v4();
    let event = fixture_file_action("~/.ssh/id_rsa", 1);
    {
        let store = SecurityStore::open(&path).expect("fixture store should open");
        store.insert_event(&event).expect("event should insert");
        store
            .upsert_case(
                &fixture_case(case_id, RiskCaseStatus::Open),
                &[event.event_id],
            )
            .expect("case should persist");
    }
    rusqlite::Connection::open(&path)
        .expect("fixture database should open")
        .execute(
            "DELETE FROM security_events WHERE event_id = ?1",
            [event.event_id.to_string()],
        )
        .expect("fixture should create a dangling link");

    let store = SecurityStore::open(&path).expect("fixture store should reopen");
    let error = store
        .case_detail(case_id)
        .expect_err("incomplete case evidence must fail closed");

    assert!(matches!(error, SecurityStoreError::InvalidData(_)));
    drop(store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn case_count_is_independent_of_case_page_size() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    for _ in 0..2 {
        let case_id = Uuid::new_v4();
        store
            .upsert_case(&fixture_case(case_id, RiskCaseStatus::Open), &[])
            .expect("case should persist");
    }

    assert_eq!(
        store
            .list_cases(1, 0, None, None, None)
            .expect("page should load")
            .len(),
        1
    );
    assert_eq!(
        store
            .case_count(None, None, None)
            .expect("case total should load"),
        2
    );
}

#[test]
fn case_summary_counts_all_statuses_independent_of_pagination() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut open = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    open.blocked = true;
    store
        .upsert_case(&open, &[])
        .expect("open case should persist");
    store
        .upsert_case(
            &fixture_case(Uuid::new_v4(), RiskCaseStatus::Confirmed),
            &[],
        )
        .expect("confirmed case should persist");
    store
        .upsert_case(
            &fixture_case(Uuid::new_v4(), RiskCaseStatus::FalsePositive),
            &[],
        )
        .expect("false-positive case should persist");

    let summary = store.case_summary().expect("case summary should load");

    assert_eq!(summary.total, 3);
    assert_eq!(summary.open, 1);
    assert_eq!(summary.blocked, 1);
}

#[test]
fn correlated_case_preserves_strongest_outcome_from_out_of_order_events() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let case_id = Uuid::new_v4();
    let mut blocked = fixture_case(case_id, RiskCaseStatus::Open);
    blocked.severity = RiskSeverity::Critical;
    blocked.risk_score = 95;
    blocked.blocked = true;
    blocked.updated_at_ns = 200;
    blocked.summary = "blocked credential egress".into();
    store
        .upsert_case(&blocked, &[])
        .expect("strong outcome should persist");

    let mut stale_allowed = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    stale_allowed.correlation_key = blocked.correlation_key.clone();
    stale_allowed.severity = RiskSeverity::Medium;
    stale_allowed.risk_score = 60;
    stale_allowed.blocked = false;
    stale_allowed.updated_at_ns = 100;
    stale_allowed.summary = "allowed stale decision".into();
    store
        .upsert_case(&stale_allowed, &[])
        .expect("stale outcome should merge");

    let stored = store
        .list_cases(10, 0, None, None, None)
        .expect("case should load")
        .pop()
        .expect("case should exist");
    assert_eq!(stored.case_id, case_id);
    assert_eq!(stored.severity, RiskSeverity::Critical);
    assert_eq!(stored.risk_score, 95);
    assert!(stored.blocked);
    assert_eq!(stored.updated_at_ns, 200);
    assert_eq!(stored.summary, "blocked credential egress");
}

#[test]
fn policy_revision_rejects_conflicting_immutable_contents() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    store
        .register_policy_revision("credential-egress", 3, r#"{"mode":"audit"}"#, 100)
        .expect("first revision should persist");
    store
        .register_policy_revision("credential-egress", 3, r#"{"mode":"audit"}"#, 200)
        .expect("identical revision should be idempotent");

    let error = store
        .register_policy_revision("credential-egress", 3, r#"{"mode":"enforce"}"#, 300)
        .expect_err("different contents must conflict");
    assert!(matches!(
        error,
        SecurityStoreError::PolicyRevisionConflict { revision: 3, .. }
    ));
}

#[test]
fn duplicate_event_is_idempotent_and_secret_content_is_absent() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let event = fixture_file_action("~/.ssh/id_rsa", 100);

    assert!(
        store
            .insert_event(&event)
            .expect("first insert should work")
    );
    assert!(!store.insert_event(&event).expect("duplicate should work"));

    let stored = store
        .event(event.event_id)
        .expect("query should work")
        .expect("event should exist");
    let json = serde_json::to_string(&stored).expect("fixture should serialize");
    assert!(!json.contains("PRIVATE KEY"));
    assert!(json.contains("~/.ssh/id_rsa"));
}

#[test]
fn process_identity_requires_stable_agent_and_session_evidence() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut event = fixture_file_action("~/.ssh/id_rsa", 100);
    store.insert_event(&event).expect("event should insert");

    assert!(
        store
            .process_identity_matches(4242, 99, "hermes-test", Some("session-1"))
            .expect("identity query should work")
    );
    assert!(
        !store
            .process_identity_matches(4242, 99, "display-name", Some("session-1"))
            .expect("identity query should work")
    );
    assert!(
        !store
            .process_identity_matches(4242, 99, "hermes-test", Some("session-2"))
            .expect("identity query should work")
    );
    assert!(
        !store
            .process_identity_matches(4242, 100, "hermes-test", Some("session-1"))
            .expect("identity query should work")
    );

    event.event_id = Uuid::new_v4();
    event.identity.agent_id = "conflicting-agent".into();
    store.insert_event(&event).expect("conflict should insert");
    assert!(
        !store
            .process_identity_matches(4242, 99, "hermes-test", Some("session-1"))
            .expect("ambiguous identity should fail closed")
    );
}

#[test]
fn list_events_clamps_limit_and_orders_newest_first() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    for occurred_at_ns in [100, 300, 200] {
        store
            .insert_event(&fixture_file_action("~/.ssh/id_rsa", occurred_at_ns))
            .expect("fixture event should insert");
    }

    let page = store
        .list_events(&SecurityEventFilter {
            limit: 5_000,
            ..SecurityEventFilter::default()
        })
        .expect("query should work");

    assert_eq!(page.limit, 1_000);
    assert!(
        page.items
            .windows(2)
            .all(|pair| pair[0].occurred_at_ns >= pair[1].occurred_at_ns)
    );
}

#[test]
fn event_filters_use_exact_bound_values() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let expected = fixture_file_action("~/.ssh/id_rsa", 200);
    let binding_id = expected.identity.binding_id;
    store
        .insert_event(&fixture_file_action("~/.ssh/id_ed25519", 100))
        .expect("fixture event should insert");
    store
        .insert_event(&expected)
        .expect("fixture event should insert");

    let page = store
        .list_events(&SecurityEventFilter {
            start_ns: Some(150),
            end_ns: Some(250),
            event_type: Some("file_action".into()),
            policy_id: Some("credential-exfiltration".into()),
            agent_id: Some("hermes-test".into()),
            session_id: Some("session-1".into()),
            binding_id: Some(binding_id),
            offset: -50,
            ..SecurityEventFilter::default()
        })
        .expect("filtered query should work");

    assert_eq!(page.items, vec![expected]);
    assert_eq!(page.offset, 0);
}

#[test]
fn filtered_event_pages_and_summaries_use_the_complete_result_set() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    for occurred_at_ns in [100, 200, 300] {
        store
            .insert_event(&fixture_file_action("~/.ssh/id_rsa", occurred_at_ns))
            .expect("file event should insert");
    }
    store
        .insert_event(&fixture_policy_decision(400, true))
        .expect("decision event should insert");
    let filter = SecurityEventFilter {
        event_type: Some("file_action".into()),
        limit: 1,
        offset: 1,
        ..SecurityEventFilter::default()
    };

    let page = store.list_events(&filter).expect("page should load");
    let summary = store
        .summary_filtered(&filter)
        .expect("filtered summary should load");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.total, 3);
    assert_eq!(summary.total_events, 3);
    assert_eq!(summary.blocked_events, 0);
}

#[test]
fn count_by_rejects_unknown_columns() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");

    let error = store
        .count_by("event_json; DROP TABLE security_events")
        .expect_err("unknown grouping must fail");

    assert!(matches!(error, SecurityStoreError::InvalidFilter(_)));
}

#[test]
fn summary_and_grouping_use_normalized_event_metadata() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    store
        .insert_event(&fixture_file_action("~/.ssh/id_rsa", 100))
        .expect("file event should insert");
    store
        .insert_event(&fixture_policy_decision(200, true))
        .expect("decision event should insert");

    let counts = store.count_by("event_type").expect("grouping should work");
    assert!(
        counts
            .iter()
            .any(|item| item.key == "file_action" && item.count == 1)
    );
    assert!(
        counts
            .iter()
            .any(|item| item.key == "policy_decision" && item.count == 1)
    );

    let summary = store.summary().expect("summary should work");
    assert_eq!(summary.total_events, 2);
    assert_eq!(summary.blocked_events, 1);
    assert_eq!(summary.evidence_loss_events, 0);
}

#[test]
fn summary_counts_one_blocked_decision_for_a_full_blocked_chain() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    for event in [
        fixture_file_action("~/.ssh/id_rsa", 100),
        fixture_taint_transition(200),
        fixture_network_action(300, true),
        fixture_policy_decision(400, true),
    ] {
        store
            .insert_event(&event)
            .expect("chain event should insert");
    }

    let summary = store.summary().expect("summary should load");

    assert_eq!(summary.total_events, 4);
    assert_eq!(summary.blocked_events, 1);
}

#[test]
fn session_pages_group_the_complete_event_set_before_pagination() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    for index in 0..1_005_u64 {
        let mut event = fixture_file_action("~/.ssh/id_rsa", index + 1);
        event.identity.session_id = Some(format!("session-{index:04}"));
        store
            .insert_event(&event)
            .expect("session event should insert");
    }
    let mut extra = fixture_file_action("~/.aws/credentials", 2_000);
    extra.identity.session_id = Some("session-1004".into());
    store
        .insert_event(&extra)
        .expect("extra session event should insert");

    let page = store
        .list_sessions(&SecurityEventFilter {
            limit: 3,
            offset: 1_000,
            ..SecurityEventFilter::default()
        })
        .expect("grouped session page should load");

    assert_eq!(page.total, 1_005);
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.items[0].session_id, "session-0004");
    assert_eq!(page.items[0].security_event_count, 1);

    let newest = store
        .list_sessions(&SecurityEventFilter {
            limit: 1,
            ..SecurityEventFilter::default()
        })
        .expect("newest session should load");
    assert_eq!(newest.items[0].session_id, "session-1004");
    assert_eq!(newest.items[0].security_event_count, 2);
    assert_eq!(newest.items[0].last_seen_ns, 2_000);
}

#[test]
fn containment_action_round_trips_and_latest_action_is_found() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let older = containment_action(ContainmentLifecycle::Expired);
    let mut action = containment_action(ContainmentLifecycle::Pending);
    action.case_id = older.case_id;
    action.created_at_ns = older.created_at_ns + 1;
    action.updated_at_ns = action.created_at_ns;

    store
        .insert_containment_action(&older)
        .expect("older action should insert");
    store
        .insert_containment_action(&action)
        .expect("action should insert");

    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work"),
        Some(action.clone())
    );
    assert_eq!(
        store
            .latest_containment_action(action.case_id)
            .expect("latest action query should work"),
        Some(action)
    );
}

#[test]
fn containment_action_updates_all_mutable_state() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut action = containment_action(ContainmentLifecycle::Pending);
    store
        .insert_containment_action(&action)
        .expect("action should insert");

    action.lifecycle_state = ContainmentLifecycle::Expiring;
    action.failure_stage = Some(ContainmentFailureStage::Detach);
    action.failure_reason = Some("enforcer temporarily unavailable".into());
    action.attempt_count = 2;
    action.next_retry_at_ns = Some(750);
    action.updated_at_ns = 500;
    store
        .update_containment_action(&action)
        .expect("action should update");

    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work"),
        Some(action)
    );
}

#[test]
fn containment_claim_is_unique_across_store_instances() {
    let path = security_db_path("containment-claim");
    let first_store = SecurityStore::open(&path).expect("first store should open");
    let second_store = SecurityStore::open(&path).expect("second store should open");
    let first = containment_action(ContainmentLifecycle::Pending);
    let mut competing = containment_action(ContainmentLifecycle::Pending);
    competing.case_id = first.case_id;
    first_store
        .upsert_case(&fixture_case(first.case_id, RiskCaseStatus::Open), &[])
        .expect("case should persist");

    assert_eq!(
        first_store
            .claim_containment_action(&first)
            .expect("first claim should work"),
        ContainmentClaimResult::Claimed
    );
    assert_eq!(
        second_store
            .claim_containment_action(&competing)
            .expect("competing claim should work"),
        ContainmentClaimResult::Existing(Box::new(first.clone()))
    );

    let mut failed = first;
    failed.lifecycle_state = ContainmentLifecycle::Failed;
    first_store
        .update_containment_action(&failed)
        .expect("first action should become terminal");
    assert_eq!(
        second_store
            .claim_containment_action(&competing)
            .expect("terminal action should release the claim"),
        ContainmentClaimResult::Claimed
    );
    drop(first_store);
    drop(second_store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn containment_claim_rejects_an_ineligible_case_without_inserting() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let action = containment_action(ContainmentLifecycle::Pending);
    store
        .upsert_case(
            &fixture_case(action.case_id, RiskCaseStatus::FalsePositive),
            &[],
        )
        .expect("case should persist");

    assert_eq!(
        store
            .claim_containment_action(&action)
            .expect("claim should inspect case state"),
        ContainmentClaimResult::CaseIneligible(RiskCaseStatus::FalsePositive)
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work"),
        None
    );
}

#[test]
fn activation_confirms_case_in_the_same_store_operation() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let action = containment_action(ContainmentLifecycle::Pending);
    store
        .upsert_case(&fixture_case(action.case_id, RiskCaseStatus::Open), &[])
        .expect("case should persist");
    store
        .claim_containment_action(&action)
        .expect("action should be claimed");

    assert_eq!(
        store
            .activate_containment_action(action.action_id, action.updated_at_ns, 500)
            .expect("activation should work"),
        ContainmentActivationResult::Activated
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work")
            .expect("action should exist")
            .lifecycle_state,
        ContainmentLifecycle::Active
    );
    assert_eq!(
        store
            .case_detail(action.case_id)
            .expect("case should load")
            .case
            .status,
        RiskCaseStatus::Resolved
    );
}

#[test]
fn activation_cas_preserves_a_concurrent_review() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let action = containment_action(ContainmentLifecycle::Pending);
    store
        .upsert_case(&fixture_case(action.case_id, RiskCaseStatus::Open), &[])
        .expect("case should persist");
    store
        .claim_containment_action(&action)
        .expect("action should be claimed");
    store
        .review_case(action.case_id, RiskCaseStatus::AcceptedRisk, 400)
        .expect("review should persist");

    assert_eq!(
        store
            .activate_containment_action(action.action_id, action.updated_at_ns, 500)
            .expect("activation should inspect case state"),
        ContainmentActivationResult::CaseIneligible(RiskCaseStatus::AcceptedRisk)
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work")
            .expect("action should exist")
            .lifecycle_state,
        ContainmentLifecycle::Pending
    );
    assert_eq!(
        store
            .case_detail(action.case_id)
            .expect("case should load")
            .case
            .status,
        RiskCaseStatus::AcceptedRisk
    );
}

#[test]
fn activation_rejects_a_replaced_claim_version() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut action = containment_action(ContainmentLifecycle::Pending);
    store
        .upsert_case(&fixture_case(action.case_id, RiskCaseStatus::Open), &[])
        .expect("case should persist");
    store
        .claim_containment_action(&action)
        .expect("action should be claimed");
    let stale_claim = action.updated_at_ns;
    action.updated_at_ns = stale_claim + 1;
    store
        .update_containment_action(&action)
        .expect("replacement claim should persist");

    assert_eq!(
        store
            .activate_containment_action(action.action_id, stale_claim, 500)
            .expect("activation should report claim loss"),
        ContainmentActivationResult::LostClaim
    );
    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work")
            .expect("action should exist")
            .lifecycle_state,
        ContainmentLifecycle::Pending
    );
}

#[test]
fn mark_containment_blocked_preserves_the_first_timestamp() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let action = containment_action(ContainmentLifecycle::Active);
    store
        .insert_containment_action(&action)
        .expect("action should insert");

    store
        .mark_containment_blocked(action.binding_id, 500)
        .expect("first block should update");
    store
        .mark_containment_blocked(action.binding_id, 800)
        .expect("duplicate block should be idempotent");

    assert_eq!(
        store
            .containment_action(action.action_id)
            .expect("action query should work")
            .expect("action should exist")
            .blocked_at_ns,
        Some(500)
    );
}

#[test]
fn due_containment_actions_include_only_actionable_temporary_rows() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");

    let mut due_active = containment_action(ContainmentLifecycle::Active);
    due_active.expires_at_ns = Some(500);
    let mut future_active = containment_action(ContainmentLifecycle::Active);
    future_active.expires_at_ns = Some(501);
    let mut persistent = containment_action(ContainmentLifecycle::Active);
    persistent.duration_secs = None;
    persistent.expires_at_ns = None;
    let mut due_retry = containment_action(ContainmentLifecycle::Expiring);
    due_retry.next_retry_at_ns = Some(500);
    let mut persistent_retry = containment_action(ContainmentLifecycle::Expiring);
    persistent_retry.duration_secs = None;
    persistent_retry.expires_at_ns = None;
    persistent_retry.next_retry_at_ns = Some(500);
    let mut future_retry = containment_action(ContainmentLifecycle::Expiring);
    future_retry.next_retry_at_ns = Some(501);
    let mut expired = containment_action(ContainmentLifecycle::Expired);
    expired.expires_at_ns = Some(100);
    let mut failed = containment_action(ContainmentLifecycle::Failed);
    failed.next_retry_at_ns = Some(100);

    for action in [
        &due_active,
        &future_active,
        &persistent,
        &due_retry,
        &persistent_retry,
        &future_retry,
        &expired,
        &failed,
    ] {
        store
            .insert_containment_action(action)
            .expect("action should insert");
    }

    let due = store
        .due_containment_actions(500, 10)
        .expect("due action query should work");
    let due_ids = due
        .iter()
        .map(|action| action.action_id)
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(due_ids.len(), 3);
    assert!(due_ids.contains(&due_active.action_id));
    assert!(due_ids.contains(&due_retry.action_id));
    assert!(due_ids.contains(&persistent_retry.action_id));
}

#[test]
fn due_containment_actions_require_reached_expiry_or_explicit_retry() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");

    let mut future_pending = containment_action(ContainmentLifecycle::Pending);
    future_pending.expires_at_ns = Some(501);
    future_pending.next_retry_at_ns = None;
    let mut future_expiring = containment_action(ContainmentLifecycle::Expiring);
    future_expiring.expires_at_ns = Some(501);
    future_expiring.next_retry_at_ns = None;
    let mut retry_pending = containment_action(ContainmentLifecycle::Pending);
    retry_pending.expires_at_ns = Some(501);
    retry_pending.next_retry_at_ns = Some(500);
    let mut expired_owned_pending = containment_action(ContainmentLifecycle::Pending);
    expired_owned_pending.expires_at_ns = Some(100);
    expired_owned_pending.next_retry_at_ns = Some(600);

    for action in [
        &future_pending,
        &future_expiring,
        &retry_pending,
        &expired_owned_pending,
    ] {
        store
            .insert_containment_action(action)
            .expect("action should insert");
    }

    let due = store
        .due_containment_actions(500, 10)
        .expect("due action query should work");

    assert_eq!(due, vec![retry_pending]);

    let due_after_lease = store
        .due_containment_actions(600, 10)
        .expect("due action query should work after ownership expires");
    assert!(due_after_lease.contains(&expired_owned_pending));
}

#[test]
fn due_containment_actions_reject_due_unknown_lifecycle() {
    let path = security_db_path("invalid-due-containment-lifecycle");
    let mut action = containment_action(ContainmentLifecycle::Pending);
    action.expires_at_ns = Some(500);
    {
        let store = SecurityStore::open(&path).expect("fixture store should open");
        store
            .insert_containment_action(&action)
            .expect("action should insert");
    }
    {
        let conn = rusqlite::Connection::open(&path).expect("fixture database should open");
        conn.execute(
            "UPDATE containment_actions SET lifecycle_state = 'unknown' WHERE action_id = ?1",
            [action.action_id.to_string()],
        )
        .expect("fixture row should mutate");
    }

    let store = SecurityStore::open(&path).expect("fixture store should reopen");
    let error = store
        .due_containment_actions(500, 10)
        .expect_err("due unknown lifecycle must fail");

    assert!(matches!(error, SecurityStoreError::InvalidData(_)));
    drop(store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn containment_queries_reject_unknown_persisted_enums() {
    let path = security_db_path("invalid-containment-enum");
    let action = containment_action(ContainmentLifecycle::Failed);
    {
        let store = SecurityStore::open(&path).expect("fixture store should open");
        store
            .insert_containment_action(&action)
            .expect("action should insert");
    }
    {
        let conn = rusqlite::Connection::open(&path).expect("fixture database should open");
        conn.execute(
            "UPDATE containment_actions SET lifecycle_state = 'unknown' WHERE action_id = ?1",
            [action.action_id.to_string()],
        )
        .expect("fixture row should mutate");
    }

    let store = SecurityStore::open(&path).expect("fixture store should reopen");
    let error = store
        .containment_action(action.action_id)
        .expect_err("unknown lifecycle must fail");

    assert!(matches!(error, SecurityStoreError::InvalidData(_)));
    drop(store);
    {
        let conn = rusqlite::Connection::open(&path).expect("fixture database should open");
        conn.execute(
            "UPDATE containment_actions
             SET lifecycle_state = 'failed', failure_stage = 'unknown'
             WHERE action_id = ?1",
            [action.action_id.to_string()],
        )
        .expect("fixture row should mutate");
    }
    let store = SecurityStore::open(&path).expect("fixture store should reopen");
    let error = store
        .containment_action(action.action_id)
        .expect_err("unknown failure stage must fail");
    assert!(matches!(error, SecurityStoreError::InvalidData(_)));
    drop(store);
    fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn containment_writes_reject_unsigned_values_above_sqlite_range() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut action = containment_action(ContainmentLifecycle::Pending);
    action.process_start_time = u64::MAX;

    let error = store
        .insert_containment_action(&action)
        .expect_err("out-of-range value must fail");

    assert!(matches!(error, SecurityStoreError::TimestampOutOfRange(value) if value == u64::MAX));
}

#[test]
fn list_cases_and_count_filter_by_agent_id() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut alpha_one = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    alpha_one.agent_id = "agent-alpha".into();
    let mut alpha_two = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    alpha_two.agent_id = "agent-alpha".into();
    let mut beta_one = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    beta_one.agent_id = "agent-beta".into();
    store
        .upsert_case(&alpha_one, &[])
        .expect("alpha case should persist");
    store
        .upsert_case(&alpha_two, &[])
        .expect("alpha case should persist");
    store
        .upsert_case(&beta_one, &[])
        .expect("beta case should persist");

    let alpha_cases = store
        .list_cases(10, 0, Some("agent-alpha"), None, None)
        .expect("alpha page should load");
    assert_eq!(alpha_cases.len(), 2);
    assert!(
        alpha_cases
            .iter()
            .all(|case| case.agent_id == "agent-alpha")
    );

    assert_eq!(
        store
            .case_count(Some("agent-alpha"), None, None)
            .expect("alpha total should load"),
        2
    );
    assert_eq!(
        store
            .case_count(Some("agent-beta"), None, None)
            .expect("beta total should load"),
        1
    );
    assert_eq!(
        store
            .case_count(None, None, None)
            .expect("grand total should load"),
        3
    );
}

#[test]
fn list_cases_and_count_filter_by_status_and_blocked() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    let mut open_a = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    open_a.blocked = false;
    let mut open_blocked = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    open_blocked.blocked = true;
    let confirmed = fixture_case(Uuid::new_v4(), RiskCaseStatus::Confirmed);
    for case in &[&open_a, &open_blocked, &confirmed] {
        store.upsert_case(case, &[]).expect("case should persist");
    }

    // status filter must apply at the store, not just in-memory.
    assert_eq!(
        store
            .case_count(None, Some("open"), None)
            .expect("open total should load"),
        2
    );
    assert_eq!(
        store
            .list_cases(10, 0, None, Some("confirmed"), None)
            .expect("confirmed page should load")
            .len(),
        1
    );

    // blocked=true filter must return only the blocked case.
    let blocked_only = store
        .list_cases(10, 0, None, None, Some(true))
        .expect("blocked page should load");
    assert_eq!(blocked_only.len(), 1);
    assert!(blocked_only[0].blocked);
    assert_eq!(
        store
            .case_count(None, None, Some(true))
            .expect("blocked total should load"),
        1
    );

    // Combined filters (open + blocked=false) must match a single case.
    assert_eq!(
        store
            .case_count(None, Some("open"), Some(false))
            .expect("open+unblocked total should load"),
        1
    );
}

#[test]
fn case_ids_for_events_maps_evidence_to_producing_case() {
    use uuid::Uuid;
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    // Two cases in the same agent/policy/revision triplet (different bursts).
    let case_a = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    let case_b = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    let event_a = Uuid::new_v4();
    let event_b = Uuid::new_v4();
    let event_c = Uuid::new_v4(); // never linked
    store
        .upsert_case(&case_a, &[event_a])
        .expect("case A should persist");
    store
        .upsert_case(&case_b, &[event_b])
        .expect("case B should persist");

    let map = store
        .case_ids_for_events(&[event_a, event_b, event_c])
        .expect("lookup should succeed");
    assert_eq!(map.get(&event_a).copied(), Some(case_a.case_id));
    assert_eq!(map.get(&event_b).copied(), Some(case_b.case_id));
    assert!(!map.contains_key(&event_c));

    // Empty input must return an empty map without hitting the DB.
    let empty = store
        .case_ids_for_events(&[])
        .expect("empty lookup should succeed");
    assert!(empty.is_empty());
}

#[test]
fn case_index_maps_agent_policy_revision_triplet_to_latest_case() {
    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    // Older and newer cases share the (agent, policy, revision) triplet; the
    // most recently updated case must win the index slot.
    let mut older = fixture_case(Uuid::new_v4(), RiskCaseStatus::Open);
    older.agent_id = "agent-alpha".into();
    older.policy_id = "credential-exfiltration".into();
    older.policy_revision = 3;
    older.updated_at_ns = 100;
    let newer_id = Uuid::new_v4();
    let mut newer = fixture_case(newer_id, RiskCaseStatus::Open);
    newer.agent_id = "agent-alpha".into();
    newer.policy_id = "credential-exfiltration".into();
    newer.policy_revision = 3;
    newer.updated_at_ns = 200;
    // A different revision is a distinct triplet with its own case slot.
    let other_id = Uuid::new_v4();
    let mut other = fixture_case(other_id, RiskCaseStatus::Open);
    other.agent_id = "agent-alpha".into();
    other.policy_id = "credential-exfiltration".into();
    other.policy_revision = 4;
    other.updated_at_ns = 50;
    store
        .upsert_case(&older, &[])
        .expect("older case should persist");
    store
        .upsert_case(&newer, &[])
        .expect("newer case should persist");
    store
        .upsert_case(&other, &[])
        .expect("other case should persist");

    let index = store
        .case_index_by_agent_policy()
        .expect("case index should build");

    assert_eq!(
        index.get(&(
            "agent-alpha".to_string(),
            "credential-exfiltration".to_string(),
            "3".to_string()
        )),
        Some(&newer_id)
    );
    assert_eq!(
        index.get(&(
            "agent-alpha".to_string(),
            "credential-exfiltration".to_string(),
            "4".to_string()
        )),
        Some(&other_id)
    );
}
