mod common;

use asc_agentsight_client::{
    AgentSightClient, AgentSightClientErrorKind, AgentSightHttpMethod, AgentSightTransportError,
    ProcessIdentityError,
};
use asc_policy_types::target::{FailureKind, Presence};
use common::*;

const APPLY_REQUEST: &[u8] =
    include_bytes!("../../../fixtures/clients/agentsight/file-deletion/apply.request.json");
const RETRYABLE_ERROR_RESPONSE: &[u8] = include_bytes!(
    "../../../fixtures/clients/agentsight/file-deletion/retryable-error.response.json"
);

#[test]
fn create_checks_health_and_reuses_the_derived_idempotency_identity() {
    let wire = Wire::new([
        Ok(response(200, HEALTH)),
        Ok(applied(7)),
        Ok(response(200, HEALTH)),
        Ok(applied(7)),
    ]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let input = client.prepare_apply(&plan(7)).unwrap();
    assert!(wire.requests().is_empty());

    let first = client.create(&input);
    let second = client.create(&input);

    assert_eq!(first, second);
    assert!(first.error.is_none());
    assert_eq!(first.observations[0].presence, Presence::Present);
    let requests = wire.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, AgentSightHttpMethod::Get);
    assert_eq!(requests[0].path, "/enforcement/health");
    assert_eq!(requests[1].method, AgentSightHttpMethod::Post);
    assert_eq!(requests[1].path, "/enforcement/bindings");
    assert_eq!(requests[1].body, requests[3].body);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_deref().unwrap()).unwrap(),
        serde_json::from_slice::<serde_json::Value>(APPLY_REQUEST).unwrap()
    );
    wire.consumed();
}

#[test]
fn delete_treats_no_content_and_binding_not_found_as_absent() {
    let wire = Wire::new([
        Ok(response(204, b"")),
        Ok(remote_error(404, "binding_not_found", false)),
    ]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let target = client.prepare_apply(&plan(7)).unwrap().target;
    for _ in 0..2 {
        let report = client.delete_targets(std::slice::from_ref(&target));
        assert!(report.error.is_none());
        assert_eq!(report.observations[0].presence, Presence::Absent);
    }
    let requests = wire.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| {
        request.method == AgentSightHttpMethod::Delete
            && request.path == path(ID7)
            && request.body.is_none()
    }));
    wire.consumed();
}

#[test]
fn different_binding_revisions_derive_different_target_ids() {
    let wire = Wire::new([Ok(response(204, b"")), Ok(response(204, b""))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    for revision in [7, 8] {
        let target = client.prepare_apply(&plan(revision)).unwrap().target;
        assert!(client.delete_targets(&[target]).error.is_none());
    }
    let requests = wire.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, path(ID7));
    assert_eq!(requests[1].path, path(ID8));
    wire.consumed();
}

#[test]
fn delete_absence_depends_only_on_status_and_code() {
    for extra in [
        "",
        ",\"retryable\":true",
        ",\"retryable\":null",
        ",\"retryable\":\"invalid\"",
    ] {
        let body = format!("{{\"error\":{{\"code\":\"binding_not_found\"{extra}}}}}");
        let wire = Wire::new([Ok(response(404, body.as_bytes()))]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let report = client.delete_targets(&[prepared(7).target]);
        assert!(report.error.is_none());
        assert_eq!(report.observations[0].presence, Presence::Absent);
        wire.consumed();
    }
    for status in [403, 404, 429, 503] {
        let wire = Wire::new([Ok(response(status, br#"{"error":{"code":"unavailable"}}"#))]);
        let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
        let report = client.delete_targets(&[prepared(7).target]);
        assert_eq!(report.observations[0].presence, Presence::Unknown);
        let error = report.error.unwrap();
        assert_eq!(error.code, "AGENTSIGHT_UNAVAILABLE");
        assert_eq!(
            error.kind,
            if status >= 500 || status == 429 {
                FailureKind::Retryable
            } else {
                FailureKind::Rejected
            }
        );
        wire.consumed();
    }
}

#[test]
fn remote_errors_are_classified_without_exposing_target_messages() {
    let wire = Wire::new([
        Ok(response(200, HEALTH)),
        Ok(response(503, RETRYABLE_ERROR_RESPONSE)),
        Ok(response(200, HEALTH)),
        Ok(remote_error(409, "policy_revision_conflict", false)),
    ]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let input = client.prepare_apply(&plan(7)).unwrap();
    for (kind, code) in [
        (FailureKind::Retryable, "AGENTSIGHT_ENFORCER_UNAVAILABLE"),
        (FailureKind::Rejected, "AGENTSIGHT_POLICY_REVISION_CONFLICT"),
    ] {
        let report = client.create(&input);
        assert_eq!(report.observations[0].presence, Presence::Unknown);
        let error = report.error.unwrap();
        assert_eq!(error.kind, kind);
        assert_eq!(error.code, code);
        assert!(!format!("{error:?}").contains("sensitive"));
        assert!(!format!("{error:?}").contains("private remote detail"));
    }
    wire.consumed();
}

#[test]
fn local_transport_and_process_failures_keep_stable_retry_categories() {
    let wire = Wire::new([Err(AgentSightTransportError::Unavailable)]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let input = client.prepare_apply(&plan(7)).unwrap();
    let unavailable = client.create(&input).error.unwrap();
    assert_eq!(unavailable.kind, FailureKind::Retryable);
    assert_eq!(unavailable.code, "AGENTSIGHT_TRANSPORT_UNAVAILABLE");
    wire.consumed();

    let wire = Wire::new([]);
    let identity = Identity::default();
    identity.0.lock().unwrap().start = Err(ProcessIdentityError::Unavailable);
    let client = AgentSightClient::with_dependencies(wire.clone(), identity);
    let unavailable = client.prepare_apply(&plan(7)).unwrap_err();
    assert_eq!(unavailable.kind, AgentSightClientErrorKind::Retryable);
    assert_eq!(unavailable.code, "AGENTSIGHT_PROCESS_IDENTITY_UNAVAILABLE");
    assert!(wire.requests().is_empty());
}

#[test]
fn prepare_rejects_an_unsupported_plan_format_before_target_io() {
    let wire = Wire::new([]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let mut input = plan(7);
    input.format = "agentsight.actplane.binding.v2".to_owned();
    let error = client.prepare_apply(&input).unwrap_err();
    assert_eq!(error.kind, AgentSightClientErrorKind::Rejected);
    assert_eq!(error.code, "AGENTSIGHT_UNSUPPORTED_PLAN_FORMAT");
    assert!(wire.requests().is_empty());
}

#[test]
fn create_fails_closed_when_file_delete_guard_is_unavailable() {
    let health = br#"{
        "ready": true,
        "backend": "actplane",
        "capabilities": { "file_delete_guard": false }
    }"#;
    let wire = Wire::new([Ok(response(200, health))]);
    let client = AgentSightClient::with_dependencies(wire.clone(), Identity::default());
    let input = client.prepare_apply(&plan(7)).unwrap();
    let error = client.create(&input).error.unwrap();
    assert_eq!(error.kind, FailureKind::Rejected);
    assert_eq!(error.code, "AGENTSIGHT_FILE_DELETE_GUARD_UNAVAILABLE");
    assert_eq!(wire.requests().len(), 1);
    wire.consumed();
}
