mod common;

use asc_agentsight_client::*;
use asc_policy_target_contracts::TargetDeploymentClient;
use asc_policy_types::target::{FailureKind, Observation, Presence};
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn existing_prepared_attribution_is_replayed_without_rewriting() {
    let mut input = prepared(7);
    let mut payload: Value = serde_json::from_slice(&input.content).unwrap();
    let request = serde_json::from_value::<Vec<u8>>(payload["request"].clone()).unwrap();
    let original_agent = "20000000-0000-4000-8000-000000000001";
    let request = String::from_utf8(request)
        .unwrap()
        .replace(
            "\"agent_id\":\"\"",
            &format!("\"agent_id\":\"{original_agent}\""),
        )
        .into_bytes();
    payload["requestDigest"] = json!(format!("sha256:{:x}", Sha256::digest(&request)));
    payload["request"] = json!(request);
    input.content = serde_json::to_vec(&payload).unwrap();
    let mut acknowledgement: Value = serde_json::from_slice(APPLY).unwrap();
    acknowledgement["request"]["agent_id"] = json!(original_agent);
    let wire = Wire::new([
        Ok(response(200, HEALTH)),
        Ok(response(
            200,
            &serde_json::to_vec(&acknowledgement).unwrap(),
        )),
    ]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    assert!(client.create(&input).error.is_none());
    assert_eq!(wire.requests()[1].body.as_ref().unwrap(), &request);
    wire.consumed();
}

#[test]
fn scope_revision_is_optional_provenance_and_does_not_change_prepared_request() {
    let wire = Wire::new([]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let original = client.prepare_apply(&plan(7)).unwrap();
    for revision in [Some(4), None] {
        let mut input = plan(7);
        let mut value: Value = serde_json::from_slice(&input.content).unwrap();
        if let Some(revision) = revision {
            value["source"]["scopeRevision"] = json!(revision);
        } else {
            value["source"]
                .as_object_mut()
                .unwrap()
                .remove("scopeRevision");
        }
        input.content = serde_json::to_vec(&value).unwrap();
        assert_eq!(client.prepare_apply(&input).unwrap(), original);
    }
    let mut invalid = plan(7);
    let mut value: Value = serde_json::from_slice(&invalid.content).unwrap();
    value["source"]["scopeRevision"] = json!(0);
    invalid.content = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        client.prepare_apply(&invalid).unwrap_err().code,
        "AGENTSIGHT_INVALID_PLAN"
    );
    assert!(wire.requests().is_empty());
}

#[test]
fn scope_identity_is_never_used_as_agent_attribution() {
    let wire = Wire::new([Ok(response(200, HEALTH)), Ok(applied(7))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let original = client.prepare_apply(&plan(7)).unwrap();
    let mut alternate = plan(7);
    let mut value: Value = serde_json::from_slice(&alternate.content).unwrap();
    value["source"]["scopeId"] = json!("another-scope");
    alternate.content = serde_json::to_vec(&value).unwrap();
    assert_eq!(client.prepare_apply(&alternate).unwrap(), original);
    assert!(client.create(&original).error.is_none());
    let requests = wire.requests();
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["agent_id"], "");
    assert_eq!(body["binding_id"], ID7);
    wire.consumed();
}

#[test]
fn replay_compares_boot_uuid_values_and_preserves_request_bytes() {
    let original = prepared(7);
    let payload: Value = serde_json::from_slice(&original.content).unwrap();
    let boot = payload["bootId"].as_str().unwrap();
    for spelling in [
        boot.to_uppercase(),
        boot.replace('-', ""),
        format!("{{{boot}}}"),
        format!("urn:uuid:{boot}"),
    ] {
        let mut alternate = payload.clone();
        alternate["bootId"] = json!(spelling);
        let mut input = original.clone();
        input.content = serde_json::to_vec(&alternate).unwrap();
        let wire = Wire::new([Ok(response(200, HEALTH)), Ok(applied(7))]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        assert!(client.create(&input).error.is_none());
        assert_eq!(
            wire.requests()[1].body.as_ref().unwrap(),
            &serde_json::from_value::<Vec<u8>>(payload["request"].clone()).unwrap()
        );
        wire.consumed();
    }
    for boot in ["not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
        let mut alternate = payload.clone();
        alternate["bootId"] = json!(boot);
        let mut input = original.clone();
        input.content = serde_json::to_vec(&alternate).unwrap();
        let wire = Wire::new([]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        assert_eq!(
            client.create(&input).error.unwrap().code,
            "AGENTSIGHT_INVALID_PREPARED"
        );
        assert!(wire.requests().is_empty());
    }
}

#[test]
fn prepare_is_http_free_and_matches_complete_frozen_payloads() {
    let wire = Wire::new([]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    for revision in [7, 8] {
        let first = client.prepare_apply(&plan(revision)).unwrap();
        assert_eq!(first, prepared(revision));
        assert_eq!(client.prepare_apply(&plan(revision)).unwrap(), first);
    }
    assert!(wire.requests().is_empty());
}

#[test]
fn trait_create_retries_the_exact_saved_bytes_after_unknown() {
    let wire = Wire::new([
        Ok(response(200, HEALTH)),
        Err(AgentSightTransportError::Unavailable),
        Ok(response(200, HEALTH)),
        Ok(applied(7)),
    ]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let input = prepared(7);
    let port: &dyn TargetDeploymentClient = &client;
    let failed = port.create(&input);
    assert_eq!(
        failed.observations,
        [Observation {
            target: input.target.clone(),
            presence: Presence::Unknown
        }]
    );
    assert_eq!(failed.error.unwrap().kind, FailureKind::Retryable);
    let done = port.create(&input);
    assert_eq!(
        done.observations,
        [Observation {
            target: input.target,
            presence: Presence::Present
        }]
    );
    assert!(done.error.is_none());
    let requests = wire.requests();
    assert_eq!(requests[1].body, requests[3].body);
    let payload: Value = serde_json::from_slice(&input.content).unwrap();
    assert_eq!(
        requests[1].body.as_ref().unwrap(),
        &serde_json::from_value::<Vec<u8>>(payload["request"].clone()).unwrap()
    );
    wire.consumed();
}

#[test]
fn update_deletes_old_before_create_and_returns_partial_result() {
    for rejection in [false, true] {
        let error = if rejection {
            remote_error(409, "policy_revision_conflict", false)
        } else {
            remote_error(503, "enforcer_unavailable", true)
        };
        let wire = Wire::new([Ok(response(200, HEALTH)), Ok(response(204, b"")), Ok(error)]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let old = prepared(7).target;
        let new = prepared(8);
        let result = client.update(std::slice::from_ref(&old), &new);
        assert_eq!(
            result.observations,
            [
                Observation {
                    target: old,
                    presence: Presence::Absent
                },
                Observation {
                    target: new.target,
                    presence: Presence::Unknown
                }
            ]
        );
        assert_eq!(
            result.error.unwrap().kind,
            if rejection {
                FailureKind::Rejected
            } else {
                FailureKind::Retryable
            }
        );
        let calls = wire.requests();
        assert_eq!(
            calls.iter().map(|r| r.method).collect::<Vec<_>>(),
            [
                AgentSightHttpMethod::Get,
                AgentSightHttpMethod::Delete,
                AgentSightHttpMethod::Post
            ]
        );
        assert_eq!(calls[1].path, path(ID7));
        wire.consumed();
    }
}

#[test]
fn update_does_not_create_until_every_old_target_is_confirmed_absent() {
    let wire = Wire::new([Ok(response(200, HEALTH)), Ok(response(200, b"{}"))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let result = client.update(&[prepared(7).target], &prepared(8));
    assert!(
        result
            .observations
            .iter()
            .all(|o| o.presence == Presence::Unknown)
    );
    assert_eq!(
        result.error.unwrap().code,
        "AGENTSIGHT_INVALID_DELETE_RESPONSE"
    );
    assert_eq!(wire.requests().len(), 2);
    wire.consumed();
}

#[test]
fn partial_update_retry_with_no_old_targets_does_not_delete_new_id() {
    let wire = Wire::new([Ok(response(200, HEALTH)), Ok(applied(8))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let result = client.update(&[], &prepared(8));
    assert!(result.error.is_none());
    assert_eq!(result.observations[0].presence, Presence::Present);
    assert!(
        wire.requests()
            .iter()
            .all(|r| r.method != AgentSightHttpMethod::Delete)
    );
    wire.consumed();
}

#[test]
fn delete_works_after_process_exit_and_preserves_unknown_subresults() {
    let wire = Wire::new([
        Ok(response(204, b"")),
        Err(AgentSightTransportError::Unavailable),
        Ok(remote_error(404, "binding_not_found", false)),
    ]);
    let identity = Identity::default();
    identity.0.lock().unwrap().start = Err(ProcessIdentityError::Exited);
    identity.0.lock().unwrap().boot = Err(ProcessIdentityError::Unavailable);
    let client = AgentSightClient::with_dependencies(wire.clone(), identity.clone());
    let old = prepared(7).target;
    let new = prepared(8).target;
    let result = TargetDeploymentClient::delete(&client, &[old.clone(), new.clone()]);
    assert_eq!(
        result.observations,
        [
            Observation {
                target: old,
                presence: Presence::Absent
            },
            Observation {
                target: new.clone(),
                presence: Presence::Unknown
            }
        ]
    );
    assert!(result.error.is_some());
    let result = client.delete_targets(&[new]);
    assert_eq!(result.observations[0].presence, Presence::Absent);
    assert!(result.error.is_none());
    assert_eq!(identity.0.lock().unwrap().reads, 0);
    wire.consumed();
}

#[test]
fn replay_rejects_pid_reuse_exit_and_boot_change_without_http() {
    for (start, boot, code) in [
        (
            Ok(123),
            Ok(BOOT_ID.into()),
            "AGENTSIGHT_PROCESS_IDENTITY_CHANGED",
        ),
        (
            Err(ProcessIdentityError::Exited),
            Ok(BOOT_ID.into()),
            "AGENTSIGHT_PROCESS_EXITED",
        ),
        (
            Ok(987_654),
            Ok("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into()),
            "AGENTSIGHT_BOOT_IDENTITY_CHANGED",
        ),
    ] {
        let wire = Wire::new([]);
        let identity = Identity::default();
        identity.0.lock().unwrap().start = start;
        identity.0.lock().unwrap().boot = boot;
        let client = AgentSightClient::with_dependencies(wire.clone(), identity);
        let result = client.create(&prepared(7));
        let error = result.error.unwrap();
        assert_eq!(error.code, code);
        assert_eq!(error.kind, FailureKind::Rejected);
        assert!(wire.requests().is_empty());
    }
}

#[test]
fn pid_change_during_old_cleanup_keeps_confirmed_deletion_and_blocks_post() {
    let wire = Wire::new([Ok(response(200, HEALTH)), Ok(response(204, b""))]);
    let identity = Identity::default();
    let changed = identity.clone();
    wire.after(2, move || changed.0.lock().unwrap().start = Ok(42));
    let client = AgentSightClient::with_dependencies(wire.clone(), identity);
    let result = client.update(&[prepared(7).target], &prepared(8));
    assert_eq!(result.observations[0].presence, Presence::Absent);
    assert_eq!(result.observations[1].presence, Presence::Unknown);
    assert_eq!(
        result.error.unwrap().code,
        "AGENTSIGHT_PROCESS_IDENTITY_CHANGED"
    );
    wire.consumed();
}

#[test]
fn corrupt_payload_route_and_target_are_rejected_before_modification() {
    for variant in 0..6 {
        let wire = Wire::new([]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let mut input = prepared(7);
        match variant {
            0 => input.target.route = "different-endpoint".into(),
            1 => input.target.id = "../../unexpected".into(),
            2 => input.target.cleanup = b"{}".to_vec(),
            3 => input.format = "unknown.v2".into(),
            4 => input.content = b"{}".to_vec(),
            _ => {
                let mut payload: Value = serde_json::from_slice(&input.content).unwrap();
                payload["requestDigest"] = json!("sha256:bad");
                input.content = serde_json::to_vec(&payload).unwrap();
            }
        }
        assert_eq!(
            client.create(&input).error.unwrap().kind,
            FailureKind::Rejected
        );
        assert!(wire.requests().is_empty());
    }
}

#[test]
fn update_never_deletes_its_new_target_and_rejects_duplicate_cleanup() {
    let wire = Wire::new([]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let new = prepared(8);
    assert_eq!(
        client
            .update(std::slice::from_ref(&new.target), &new)
            .error
            .unwrap()
            .code,
        "AGENTSIGHT_INVALID_PREVIOUS_TARGETS"
    );
    let old = prepared(7).target;
    assert_eq!(
        client
            .delete_targets(&[old.clone(), old])
            .error
            .unwrap()
            .code,
        "AGENTSIGHT_DUPLICATE_TARGET_REFERENCE"
    );
    assert!(wire.requests().is_empty());
}

#[test]
fn delete_requires_the_exact_absence_signal_and_sanitizes_errors() {
    for reply in [
        response(200, b"{}"),
        response(202, b"{}"),
        remote_error(404, "unknown_route", false),
        remote_error(401, "unauthorized", false),
    ] {
        let wire = Wire::new([Ok(reply)]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let result = client.delete_targets(&[prepared(7).target]);
        assert_eq!(result.observations[0].presence, Presence::Unknown);
        assert!(result.error.is_some());
        assert!(!format!("{result:?}").contains("private remote detail"));
        wire.consumed();
    }
}

#[test]
fn preparation_requires_boot_identity_before_any_http() {
    let wire = Wire::new([]);
    let identity = Identity::default();
    identity.0.lock().unwrap().boot = Err(ProcessIdentityError::Unavailable);
    let client = AgentSightClient::with_dependencies(wire.clone(), identity);
    assert_eq!(
        client.prepare_apply(&plan(7)).unwrap_err().code,
        "AGENTSIGHT_BOOT_IDENTITY_UNAVAILABLE"
    );
    assert!(wire.requests().is_empty());
}

#[test]
fn later_invalid_cleanup_prevents_all_delete_and_update_requests() {
    let wire = Wire::new([]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let valid = prepared(7).target;
    let mut invalid = prepared(8).target;
    invalid.cleanup = b"{}".to_vec();
    let targets = [valid, invalid];

    let deleted = client.delete_targets(&targets);
    assert!(deleted.observations.is_empty());
    assert_eq!(
        deleted.error.unwrap().code,
        "AGENTSIGHT_INVALID_TARGET_REFERENCE"
    );

    let new = client.prepare_apply(&plan(9)).unwrap();
    let updated = client.update(&targets, &new);
    assert_eq!(
        updated.observations,
        [Observation {
            target: new.target,
            presence: Presence::Unknown,
        }]
    );
    assert_eq!(
        updated.error.unwrap().code,
        "AGENTSIGHT_INVALID_TARGET_REFERENCE"
    );
    assert!(wire.requests().is_empty());
}

#[test]
fn route_identity_is_validated_and_never_retargets_saved_cleanup() {
    let wire = Wire::new([Ok(response(204, b""))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default())
        .with_reconcile_route("host-primary")
        .unwrap();
    let input = client.prepare_apply(&plan(7)).unwrap();
    assert_eq!(input.target.route, "host-primary");
    assert_eq!(input.target.id, ID7);
    assert!(client.delete_targets(&[prepared(7).target]).error.is_some());
    assert!(wire.requests().is_empty());
    assert!(client.delete_targets(&[input.target]).error.is_none());
    wire.consumed();
    let invalid = AgentSightClient::with_dependencies(Wire::new([]), Identity::default())
        .with_reconcile_route("http://private@host");
    assert_eq!(invalid.err().unwrap().code, "AGENTSIGHT_INVALID_ROUTE");
}

#[test]
fn failed_update_preflight_does_not_remove_old_enforcement() {
    let unavailable =
        br#"{"ready":false,"backend":"actplane","capabilities":{"file_delete_guard":true}}"#;
    let wire = Wire::new([Ok(response(200, unavailable))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let report = client.update(&[prepared(7).target], &prepared(8));
    assert_eq!(report.error.unwrap().code, "AGENTSIGHT_BACKEND_NOT_READY");
    assert_eq!(wire.requests().len(), 1);
    assert_eq!(wire.requests()[0].method, AgentSightHttpMethod::Get);
    assert!(
        report
            .observations
            .iter()
            .all(|o| o.presence == Presence::Unknown)
    );
    wire.consumed();
}
