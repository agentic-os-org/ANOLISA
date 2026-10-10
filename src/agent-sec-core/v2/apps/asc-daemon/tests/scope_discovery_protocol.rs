#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

use std::sync::Arc;

use asc_daemon::ScopeDiscoveryRegistry;
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::DaemonDispatcher;
use asc_daemon_protocol::{DaemonRequest, RequestId};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use serde_json::{Value, json};

struct Role(PrincipalRole);
impl PrincipalPolicy for Role {
    fn role_for(&self, _peer: PeerCredentials) -> PrincipalRole {
        self.0
    }
}

#[test]
fn scope_assignment_protocol_creates_reads_and_deletes_discovery_intent() {
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let pap = PapService::new(repository);
    let registry = Arc::new(ScopeDiscoveryRegistry::new(Arc::new(pap.clone())));
    let pap = pap.with_scope_discovery(registry.clone());
    let policy = pap
        .create_policy("scope-policy", &file_policy(vec!["/protected".to_owned()]))
        .unwrap();
    let dispatcher = DaemonDispatcher::new(
        pap,
        Arc::new(Role(PrincipalRole::PolicyAdministrator)),
        asc_daemon::scan_application(
            asc_action_runtime::testing::discarding_finalizer(),
            Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
        ),
    );
    let send = |method: &str, params: Value| {
        serde_json::to_value(dispatcher.handle(
            RequestId::new("scope-test".to_owned()).unwrap(),
            PeerCredentials::new(1000, 1000, 42),
            DaemonRequest {
                method: method.to_owned(),
                params,
                trace_context: None,
                compatibility: None,
            },
        ))
        .unwrap()
    };
    assert_eq!(
        send("agent.probes.create", json!({}))["error"]["code"],
        "unknown_method"
    );
    let params = json!({"selector":{"kind":"process","match":{"processName":"scope-test"}},"policyTemplates":[{"policyId":policy.policy_id,"policyRevision":policy.revision}]});
    let mut missing = params.clone();
    missing["policyTemplates"][0]["policyRevision"] = json!(99);
    assert_eq!(
        send("policy.scopes.create", missing)["error"]["code"],
        "not_found"
    );
    let created = send("policy.scopes.create", params.clone());
    assert!(created.get("result").is_some(), "{created}");
    assert_eq!(created["result"]["selector"], params["selector"]);
    assert_eq!(created["result"]["policySnapshots"], json!([policy]));
    let revision = json!({"id":created["result"]["scopeId"]});
    assert_eq!(
        send("policy.scopes.get", revision.clone())["result"],
        created["result"]
    );
    assert_eq!(
        send("policy.scopes.delete", revision.clone())["result"]["completed"],
        true
    );
    assert_eq!(
        send("policy.scopes.get", revision)["error"]["code"],
        "not_found"
    );
    registry.shutdown().unwrap();
}
