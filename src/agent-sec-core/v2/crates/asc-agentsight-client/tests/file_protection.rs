mod common;

use asc_agentsight_client::*;
use asc_policy_types::target::{Presence, TargetBindingPlan};
use common::*;
use serde_json::{Value, json};

fn structured_plan(delete: bool, write: bool) -> TargetBindingPlan {
    plan_with_policy(
        &json!({"version":1,"topology_mode":"static_at_apply","targets":[{
            "path":"/workspace","kind":"directory_tree","protections":{"delete":delete,"ordinary_write":write},
            "exclusions":[{"path":"/workspace/cache","kind":"directory_subtree"}]
        }]}),
    )
}

fn plan_with_policy(policy: &Value) -> TargetBindingPlan {
    let mut plan = plan(7);
    let mut value: Value = serde_json::from_slice(&plan.content).unwrap();
    value["policy"] = json!({
        "mediaType":"application/vnd.agentsight.file-protection.v1+json",
        "content":policy.to_string()
    });
    plan.content = serde_json::to_vec(&value).unwrap();
    plan
}

fn request_bytes(prepared: &asc_policy_types::target::PreparedApply) -> Vec<u8> {
    let value: Value = serde_json::from_slice(&prepared.content).unwrap();
    serde_json::from_value(value["request"].clone()).unwrap()
}

#[test]
fn structured_requests_match_the_http_contract_and_replay_exact_bytes() {
    for (delete, write) in [(true, false), (false, true), (true, true)] {
        let input = AgentSightClient::with_dependencies(Wire::new([]), Identity::default())
            .prepare_apply(&structured_plan(delete, write))
            .unwrap();
        let bytes = request_bytes(&input);
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            request,
            json!({
                "binding_id":ID7,"agent_id":"","root_pid":4242,"process_start_time":987_654,
                "policy_id":"00000000-0000-4000-8000-000000000002","policy_revision":"1",
                "policy_mode":"enforce","file_protection_policy":{
                    "version":1,"topology_mode":"static_at_apply","targets":[{
                        "path":"/workspace","kind":"directory_tree",
                        "protections":{"delete":delete,"ordinary_write":write},
                        "exclusions":[{"path":"/workspace/cache","kind":"directory_subtree"}]
                    }]
                }
            })
        );
        let ack = json!({"request":request,"state":"enforced","domain_id":1});
        let health = serde_json::to_vec(&structured_health()).unwrap();
        let wire = Wire::new([
            Ok(response(200, &health)),
            Err(AgentSightTransportError::Unavailable),
            Ok(response(200, &health)),
            Ok(response(200, &serde_json::to_vec(&ack).unwrap())),
        ]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        assert!(client.create(&input).error.is_some());
        let result = client.create(&input);
        assert!(result.error.is_none());
        assert_eq!(result.observations[0].presence, Presence::Present);
        assert_eq!(wire.requests()[1].body.as_ref().unwrap(), &bytes);
        assert_eq!(wire.requests()[1].body, wire.requests()[3].body);
        wire.consumed();
    }
}

#[test]
fn v1_contract_fixture_preserves_all_target_and_exclusion_kinds() {
    let policy: Value = serde_json::from_str(include_str!(
        "../../../fixtures/clients/agentsight/file-protection-v1.json"
    ))
    .unwrap();
    let client = AgentSightClient::with_dependencies(Wire::new([]), Identity::default());
    let input = client.prepare_apply(&plan_with_policy(&policy)).unwrap();
    let request: Value = serde_json::from_slice(&request_bytes(&input)).unwrap();
    assert_eq!(request["file_protection_policy"], policy);
}

#[test]
fn unsupported_contract_versions_fields_and_kinds_are_rejected_before_http() {
    let policy: Value = serde_json::from_str(include_str!(
        "../../../fixtures/clients/agentsight/file-protection-v1.json"
    ))
    .unwrap();
    for (pointer, replacement) in [
        ("/version", json!(2)),
        ("/topology_mode", json!("dynamic")),
        ("/targets/0/kind", json!("directory_entry")),
        ("/targets/1/exclusions/0/kind", json!("glob")),
        (
            "/targets/0/protections",
            json!({"delete":true,"ordinary_write":false,"rename":true}),
        ),
    ] {
        let mut invalid = policy.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        let wire = Wire::new([]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let error = client
            .prepare_apply(&plan_with_policy(&invalid))
            .unwrap_err();
        assert_eq!(error.code, "AGENTSIGHT_INVALID_FILE_PROTECTION_POLICY");
        assert!(wire.requests().is_empty());
    }
}

#[test]
fn missing_structured_capabilities_prevent_update_cleanup_and_post() {
    for capability in [
        "static_mount_topology",
        "directory_tree_delete_protection",
        "ordinary_write_protection",
    ] {
        let mut health = structured_health();
        health["capabilities"]
            .as_object_mut()
            .unwrap()
            .remove(capability);
        let wire = Wire::new([Ok(response(200, &serde_json::to_vec(&health).unwrap()))]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let input = client.prepare_apply(&structured_plan(true, true)).unwrap();
        assert!(client.update(&[prepared(8).target], &input).error.is_some());
        assert_eq!(wire.requests().len(), 1);
        assert_eq!(wire.requests()[0].method, AgentSightHttpMethod::Get);
    }
}

#[test]
fn mismatched_protection_acknowledgement_is_not_success() {
    let input = AgentSightClient::with_dependencies(Wire::new([]), Identity::default())
        .prepare_apply(&structured_plan(true, true))
        .unwrap();
    let mut request: Value = serde_json::from_slice(&request_bytes(&input)).unwrap();
    request["file_protection_policy"]["targets"][0]["protections"]["ordinary_write"] = json!(false);
    let wire = Wire::new([
        Ok(response(
            200,
            &serde_json::to_vec(&structured_health()).unwrap(),
        )),
        Ok(response(
            200,
            &serde_json::to_vec(&json!({"request":request,"state":"enforced","domain_id":1}))
                .unwrap(),
        )),
    ]);
    let client = AgentSightClient::with_dependencies(wire, Identity::default());
    assert_eq!(
        client.create(&input).error.unwrap().code,
        "AGENTSIGHT_APPLY_RESPONSE_MISMATCH"
    );
}

#[test]
fn malformed_structured_artifacts_never_prepare_a_request() {
    for replacement in [
        json!({"version":2,"topology_mode":"static_at_apply","targets":[]}),
        json!({"version":1,"topology_mode":"static_at_apply","targets":[]}),
        json!({"version":1,"topology_mode":"dynamic","targets":[]}),
        json!({"version":1,"topology_mode":"static_at_apply","targets":[{"path":"/a","kind":"exact_file","protections":{"delete":false,"ordinary_write":false},"exclusions":[]}]}),
        json!({"version":1,"topology_mode":"static_at_apply","targets":[],"ignored_condition":"program"}),
    ] {
        let mut input = structured_plan(true, true);
        let mut value: Value = serde_json::from_slice(&input.content).unwrap();
        value["policy"]["content"] = json!(replacement.to_string());
        input.content = serde_json::to_vec(&value).unwrap();
        let client = AgentSightClient::with_dependencies(Wire::new([]), Identity::default());
        assert!(client.prepare_apply(&input).is_err());
    }
}
