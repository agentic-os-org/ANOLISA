//! A wildcard HTTP restriction is stored once even when reported on multiple UID scopes.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use asc_policy_types::policy::{DeploymentBackend, PreparedPolicy};
use asc_policy_types::scope::ProcessMatcher;

use super::ScopeOutcome;
use crate::automatic::derive;

fn policy(id: &str, text: &str) -> PreparedPolicy {
    serde_json::from_value(serde_json::json!({
        "policyId": id, "revision": 1, "policyName": id,
        "template": {"language":"cedar", "content":{
            "schema":{"schemaId":"seccore.system", "revision":3}, "text":text
        }}
    }))
    .unwrap()
}

#[test]
fn wildcard_gateway_diagnostics_reuse_membership_and_return_each_scopes_outcome() {
    let mut grants = String::new();
    for uid in 1000..1004 {
        writeln!(grants, r#"@modify("inject_credential")
            permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
            when {{principal has agentType && principal.agentType == "qoder" && principal has ownerUid && principal.ownerUid == {uid}}};"#).unwrap();
    }
    let restrictions = r#"
        forbid(principal, action, resource is SecCore::HttpEndpoint) when {resource.host == "one.example"};
        forbid(principal, action, resource is SecCore::HttpEndpoint) when {resource.host == "two.example"};
        forbid(principal, action, resource is SecCore::HttpEndpoint)
        when {principal has agentType && principal.agentType == "qoder" && principal has ownerUid && principal.ownerUid == 1000};"#;
    let policies = [
        policy("grants", &grants),
        policy("restrictions", restrictions),
    ];
    let mappings = BTreeMap::from([(
        "qoder".into(),
        ProcessMatcher::Name {
            process_name: "qoder".into(),
        },
    )]);
    let (policies, views) = crate::selection::SelectionCache::isolated_deployment(
        policies.to_vec(),
        &[asc_gateway_plan::selection::SELECTOR],
    )
    .unwrap();
    let plan = derive(policies.clone(), Ok(&mappings), views).unwrap();
    let mut outcomes = Vec::new();
    let mut expected = BTreeMap::new();
    for (id, candidate) in &plan.scopes {
        let uid = candidate.group.uid.unwrap();
        let (outcome, state, reason) = match uid {
            1000 => (
                ScopeOutcome::Failed("rejected".into()),
                "failed",
                Some("rejected"),
            ),
            1001 => (ScopeOutcome::WaitingForProcess, "waitingForProcess", None),
            _ => (ScopeOutcome::Assigned, "assigned", None),
        };
        let snapshots = plan.prepare(id).unwrap().policy_snapshots;
        let ids = snapshots
            .iter()
            .find(|p| p.policy_id == policies[1].policy_id)
            .unwrap()
            .deployment
            .as_ref()
            .unwrap()
            .statement_ids
            .clone();
        assert_eq!(ids.len(), if uid == 1000 { 3 } else { 2 });
        expected.insert(id.clone(), (ids, state, reason));
        outcomes.push((id.clone(), outcome));
    }
    let cache = plan.into_diagnostics(outcomes);
    assert_eq!(cache.scopes.len(), 4);
    assert_eq!(cache.includes.len(), 3);
    assert_eq!(
        cache
            .scopes
            .iter()
            .flat_map(|s| s.bound.values())
            .map(BTreeSet::len)
            .sum::<usize>(),
        4
    );
    let p = &policies[1];
    let rows = cache.for_policy(&p.policy_id, p.revision);
    assert_eq!(rows.len(), 4);
    for row in rows {
        let (ids, state, reason) = expected.remove(row.scope_id.as_ref().unwrap()).unwrap();
        assert_eq!(
            (row.statement_ids, row.state.as_str(), row.reason.as_deref()),
            (ids, state, reason)
        );
        assert_eq!(row.backend, DeploymentBackend::Gateway);
        assert_eq!(row.policy_id, p.policy_id);
        assert_eq!(row.revision, p.revision);
    }
    assert!(
        cache
            .for_policy(
                &p.policy_id,
                asc_foundation_types::Revision::new(2).unwrap()
            )
            .is_empty()
    );
}
