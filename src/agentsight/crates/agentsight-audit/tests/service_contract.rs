use std::sync::Arc;

use agentsight_audit::{AuditService, AuditStore, RiskCase, RiskCaseStatus, RiskSeverity};
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
