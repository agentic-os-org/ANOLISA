//! Responses cannot change input identity, reference scope, answers or recommendations.

use semantic_arbiter::check::check;
use semantic_arbiter::contract::{AnswerValue, Reason};
use semantic_arbiter::outcome::CandidateControl;
use semantic_arbiter::protocol::{
    decode_request, decode_response, validate_response, CheckRequest, CheckResponse, ContractError,
    MAX_MESSAGE_BYTES,
};
use serde_json::{json, Value};

fn pair() -> (CheckRequest, CheckResponse) {
    let request = decode_request(include_bytes!("../examples/snapshot.json")).unwrap();
    let response = check(&request).unwrap();
    (request, response)
}

#[test]
fn unknown_fields_are_rejected_throughout_response_objects() {
    let (request, response) = pair();
    let mut original = serde_json::to_value(response).unwrap();
    original["outcome"]["control"]["subject"] = json!([{
        "id": "freeze", "instance_id": "synthetic-instance", "action": "freeze",
        "scope_ids": ["scope-1"]
    }]);
    for path in [
        "",
        "/binding",
        "/answers/0",
        "/outcome",
        "/outcome/control",
        "/outcome/control/subject/0",
        "/outcome/obligations/0",
        "/outcome/obligations/0/action",
        "/outcome/obligations/0/action/parameters",
        "/outcome/obligations/1/action/parameters",
    ] {
        let mut changed = original.clone();
        changed
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("executable_command".into(), json!("synthetic-secret"));
        assert_eq!(
            decode_response(&serde_json::to_vec(&changed).unwrap(), &request),
            Err(ContractError::InvalidJson),
            "{path}"
        );
    }
}

#[test]
fn duplicate_response_members_and_noninteger_confidence_are_rejected() {
    let (request, response) = pair();
    let wire = serde_json::to_string(&response).unwrap();
    for member in [
        "\"request_id\":\"snapshot\"",
        "\"confidence_ppm\":1000000",
        "\"candidate\":\"allow\"",
        "\"critical\":true",
        "\"kind\":\"snapshot\"",
    ] {
        assert!(wire.contains(member));
        let duplicate = wire.replacen(member, &format!("{member},{member}"), 1);
        assert_eq!(
            decode_response(duplicate.as_bytes(), &request),
            Err(ContractError::InvalidJson)
        );
    }
    for number in [
        json!(-1),
        json!(0.5),
        json!(1.0),
        json!("1000000"),
        json!(4_294_967_296_u64),
    ] {
        let mut value = serde_json::to_value(&response).unwrap();
        value["answers"][0]["confidence_ppm"] = number;
        assert_eq!(
            decode_response(&serde_json::to_vec(&value).unwrap(), &request),
            Err(ContractError::InvalidJson)
        );
    }
}

#[test]
fn response_version_identity_binding_and_digest_are_exact() {
    let (request, response) = pair();
    let mut wrong_version = response.clone();
    wrong_version.api_version = "semantic-arbiter/v1alpha2".into();
    assert_eq!(
        validate_response(&wrong_version, &request),
        Err(ContractError::UnsupportedVersion)
    );
    for path in [
        "/request_id",
        "/input_digest",
        "/binding/session_id",
        "/binding/instance_id",
        "/binding/workspace_id",
        "/binding/task_version",
        "/binding/config_revision",
        "/binding/candidate_id",
        "/binding/candidate_revision",
    ] {
        let mut changed = serde_json::to_value(&response).unwrap();
        *changed.pointer_mut(path).unwrap() = json!("different-valid-id");
        assert_eq!(
            decode_response(&serde_json::to_vec(&changed).unwrap(), &request),
            Err(ContractError::BindingMismatch),
            "{path}"
        );
    }
    let mut changed_request = request;
    changed_request
        .task
        .as_mut()
        .unwrap()
        .text
        .push_str(" Additional instruction.");
    assert_eq!(
        validate_response(&response, &changed_request),
        Err(ContractError::BindingMismatch)
    );
}

#[test]
fn duplicate_missing_and_forged_answers_cannot_be_admitted() {
    let (request, response) = pair();
    let mut duplicate = response.clone();
    duplicate.answers[1] = duplicate.answers[0].clone();
    assert_eq!(
        validate_response(&duplicate, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut missing = response.clone();
    missing.answers.pop();
    assert_eq!(
        validate_response(&missing, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut extra = response.clone();
    extra.answers.push(extra.answers[0].clone());
    assert_eq!(
        validate_response(&extra, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut reversed = response.clone();
    reversed.answers.reverse();
    assert_eq!(
        validate_response(&reversed, &request),
        Err(ContractError::InvalidResponse)
    );

    let mut forged = response.clone();
    forged.answers[0].value = AnswerValue::Allow;
    forged.answers[0].reason = Reason::Allowed;
    assert_eq!(
        validate_response(&forged, &request),
        Err(ContractError::InvalidResponse)
    );
    for confidence in [0, 999_999, 1_000_001, u32::MAX] {
        let mut forged = response.clone();
        forged.answers[0].confidence_ppm = confidence;
        assert_eq!(
            validate_response(&forged, &request),
            Err(ContractError::InvalidResponse)
        );
    }
    assert_eq!(validate_response(&response, &request), Ok(()));
}

#[test]
fn references_must_be_unique_current_and_actually_used() {
    let (request, response) = pair();
    for path in ["/answers/0/evidence_refs", "/outcome/evidence_refs"] {
        for (refs, error) in [
            (
                json!(["another-request-evidence"]),
                ContractError::InvalidResponse,
            ),
            (
                json!(["rebuild-1", "rebuild-1"]),
                ContractError::InvalidInput,
            ),
            (json!([]), ContractError::InvalidResponse),
        ] {
            let mut changed = serde_json::to_value(&response).unwrap();
            *changed.pointer_mut(path).unwrap() = refs;
            assert_eq!(
                decode_response(&serde_json::to_vec(&changed).unwrap(), &request),
                Err(error)
            );
        }
    }
    let mut forged = response;
    forged.answers[1].evidence_refs = vec!["rebuild-1".into()];
    assert_eq!(
        validate_response(&forged, &request),
        Err(ContractError::InvalidResponse)
    );
}

#[test]
fn a_valid_outcome_shape_does_not_authorize_forged_check_actions() {
    let (request, response) = pair();
    for control in [
        CandidateControl::Nop,
        CandidateControl::Ask,
        CandidateControl::Block,
    ] {
        let mut changed = response.clone();
        changed.outcome.control.candidate = control;
        assert_eq!(
            validate_response(&changed, &request),
            Err(ContractError::InvalidResponse)
        );
    }
    let mut missing_snapshot = response.clone();
    missing_snapshot.outcome.obligations.remove(0);
    assert_eq!(
        validate_response(&missing_snapshot, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut reordered = response.clone();
    reordered.outcome.obligations.reverse();
    assert_eq!(
        validate_response(&reordered, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut downgraded = response.clone();
    downgraded.outcome.obligations[0].critical = false;
    assert_eq!(
        validate_response(&downgraded, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut unknown: Value = serde_json::to_value(response).unwrap();
    unknown["outcome"]["obligations"][0]["action"]["kind"] = json!("run_shell");
    assert_eq!(
        decode_response(&serde_json::to_vec(&unknown).unwrap(), &request),
        Err(ContractError::InvalidJson)
    );
}

#[test]
fn response_wire_bound_is_enforced_before_deserialization() {
    let (request, response) = pair();
    let mut wire = serde_json::to_vec(&response).unwrap();
    wire.resize(MAX_MESSAGE_BYTES, b' ');
    assert_eq!(decode_response(&wire, &request).unwrap(), response);
    wire.push(b' ');
    assert_eq!(
        decode_response(&wire, &request),
        Err(ContractError::MessageTooLarge)
    );
}
