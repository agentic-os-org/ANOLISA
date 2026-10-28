//! Strict request decoding and frozen evidence boundaries.

use semantic_arbiter::check::check;
use semantic_arbiter::contract::{AnswerValue, EvidenceFact, Reason, RebuildResult, Scenario};
use semantic_arbiter::outcome::CandidateControl;
use semantic_arbiter::protocol::{
    decode_request, validate_request, CheckRequest, ContractError, MAX_EVIDENCE_ITEMS,
    MAX_MESSAGE_BYTES,
};
use serde_json::{json, Value};

fn request() -> CheckRequest {
    decode_request(include_bytes!("../examples/preservation-allow.json")).unwrap()
}

fn assert_rejection(value: &Value, error: ContractError) {
    assert_eq!(
        decode_request(&serde_json::to_vec(value).unwrap()),
        Err(error)
    );
}

#[test]
fn unknown_fields_are_rejected_at_every_request_object_level() {
    let original: Value =
        serde_json::from_slice(include_bytes!("../examples/coverage-gap.json")).unwrap();
    for path in [
        "",
        "/binding",
        "/budget",
        "/task",
        "/candidate",
        "/evidence/0",
        "/evidence/0/binding",
        "/evidence/0/fact",
        "/evidence/0/fact/parameters",
        "/gaps/0",
    ] {
        let mut changed = original.clone();
        changed
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unregistered_action".into(), json!("synthetic-secret"));
        assert_eq!(
            decode_request(&serde_json::to_vec(&changed).unwrap()),
            Err(ContractError::InvalidJson),
            "unknown field at {path}"
        );
    }

    let mut legacy = original;
    let check_at_ms = legacy["check_at_ms"].clone();
    legacy["replay_at_ms"] = check_at_ms;
    assert_rejection(&legacy, ContractError::InvalidJson);
}

#[test]
fn duplicate_fields_are_rejected_before_values_can_be_overwritten() {
    let original = serde_json::to_string(&request()).unwrap();
    for (field, value) in [
        ("api_version", json!("semantic-arbiter/v1alpha1")),
        ("request_id", json!("preservation-allow")),
        ("candidate_revision", json!("candidate-v1")),
        ("deadline_ms", json!(2000)),
        ("origin", json!("operator")),
        ("operation", json!("replace")),
        ("provenance", json!("local_verifier")),
        ("kind", json!("rebuild")),
        ("result", json!("identical")),
    ] {
        let member = format!("\"{field}\":{value}");
        assert!(original.contains(&member));
        let duplicate = original.replacen(&member, &format!("{member},{member}"), 1);
        assert_eq!(
            decode_request(duplicate.as_bytes()),
            Err(ContractError::InvalidJson),
            "{field}"
        );
    }
}

#[test]
fn versions_and_closed_enums_reject_unregistered_values() {
    for version in [
        "",
        "semantic-arbiter/v1",
        "semantic-arbiter/v1alpha2",
        "semantic-arbiter/v1alpha3",
        "aw/v1alpha1",
    ] {
        let mut value = serde_json::to_value(request()).unwrap();
        value["api_version"] = json!(version);
        assert_rejection(&value, ContractError::UnsupportedVersion);
    }
    for (path, replacement) in [
        ("/mode", json!("live")),
        ("/mode", json!("offline_replay")),
        ("/scenarios/0", json!("unregistered_scenario")),
        ("/task/origin", json!("source_reported")),
        ("/candidate/operation", json!("execute")),
        ("/evidence/0/provenance", json!("trusted_model")),
        ("/evidence/0/fact/kind", json!("command")),
        (
            "/evidence/0/fact/parameters/result",
            json!("probably_identical"),
        ),
    ] {
        let mut value = serde_json::to_value(request()).unwrap();
        *value.pointer_mut(path).unwrap() = replacement;
        assert_rejection(&value, ContractError::InvalidJson);
    }
}

#[test]
fn integer_fields_reject_negative_fractional_string_and_overflow_values() {
    for path in [
        "/check_at_ms",
        "/budget/opened_at_ms",
        "/budget/deadline_ms",
        "/budget/max_evidence_items",
        "/budget/max_evidence_bytes",
        "/evidence/0/captured_at_ms",
        "/evidence/0/valid_until_ms",
    ] {
        for replacement in [json!(-1), json!(1.5), json!(1.0), json!("1500")] {
            let mut value = serde_json::to_value(request()).unwrap();
            *value.pointer_mut(path).unwrap() = replacement;
            assert_eq!(
                decode_request(&serde_json::to_vec(&value).unwrap()),
                Err(ContractError::InvalidJson),
                "{path}"
            );
        }
    }
    let bytes = serde_json::to_string(&request()).unwrap().replace(
        "\"check_at_ms\":1500",
        "\"check_at_ms\":18446744073709551616",
    );
    assert_eq!(
        decode_request(bytes.as_bytes()),
        Err(ContractError::InvalidJson)
    );
}

#[test]
fn task_and_every_evidence_binding_dimension_must_match() {
    for path in [
        "/task/session_id",
        "/task/version",
        "/evidence/0/binding/session_id",
        "/evidence/0/binding/instance_id",
        "/evidence/0/binding/workspace_id",
        "/evidence/0/binding/task_version",
        "/evidence/0/binding/config_revision",
        "/evidence/0/binding/candidate_id",
        "/evidence/0/binding/candidate_revision",
    ] {
        let mut value = serde_json::to_value(request()).unwrap();
        *value.pointer_mut(path).unwrap() = json!("different-valid-id");
        assert_rejection(&value, ContractError::BindingMismatch);
    }
}

#[test]
fn invalid_identifiers_lists_clocks_and_task_lengths_are_rejected() {
    for replacement in [
        json!(""),
        json!("x".repeat(129)),
        json!("/workspace/file"),
        json!("two words"),
        json!("非ASCII"),
    ] {
        let mut value = serde_json::to_value(request()).unwrap();
        value["request_id"] = replacement;
        assert_rejection(&value, ContractError::InvalidInput);
    }
    for (path, replacement) in [
        ("/task/text", json!(" \n\t")),
        ("/task/text", json!("x".repeat(16_385))),
        ("/task/text", json!("é".repeat(8193))),
        (
            "/task/allowed_resource_ids",
            json!(["result-1", "result-1"]),
        ),
        (
            "/task/preserve_resource_ids",
            json!(["result-1", "result-1"]),
        ),
        ("/scenarios", json!([])),
        ("/scenarios", json!(["preservation", "preservation"])),
        (
            "/scenarios",
            json!(["preservation", "task_scope", "preservation"]),
        ),
        ("/budget/deadline_ms", json!(1000)),
        ("/check_at_ms", json!(999)),
        ("/budget/max_evidence_items", json!(MAX_EVIDENCE_ITEMS + 1)),
        ("/budget/max_evidence_bytes", json!(MAX_MESSAGE_BYTES + 1)),
        ("/evidence/0/valid_until_ms", json!(1200)),
        ("/evidence/0/valid_until_ms", json!(1199)),
    ] {
        let mut value = serde_json::to_value(request()).unwrap();
        *value.pointer_mut(path).unwrap() = replacement;
        assert_rejection(&value, ContractError::InvalidInput);
    }
    let mut duplicate = request();
    duplicate.evidence.push(duplicate.evidence[0].clone());
    assert_eq!(
        validate_request(&duplicate),
        Err(ContractError::InvalidInput)
    );
    let mut gap = decode_request(include_bytes!("../examples/coverage-gap.json")).unwrap();
    gap.gaps.push(gap.gaps[0].clone());
    assert_eq!(validate_request(&gap), Err(ContractError::InvalidInput));
}

#[test]
fn exact_hard_bounds_are_accepted_and_one_past_is_rejected() {
    let mut value = request();
    value.request_id = "a".repeat(128);
    value.task.as_mut().unwrap().text = "x".repeat(16_384);
    value.task.as_mut().unwrap().allowed_resource_ids =
        (0..64).map(|i| format!("resource-{i}")).collect();
    value.budget.max_evidence_items = MAX_EVIDENCE_ITEMS;
    value.budget.max_evidence_bytes = MAX_MESSAGE_BYTES;
    value.evidence = (0..64)
        .map(|i| {
            let mut item = value.evidence[0].clone();
            item.id = format!("evidence-{i}");
            item
        })
        .collect();
    assert_eq!(validate_request(&value), Ok(()));
    value
        .task
        .as_mut()
        .unwrap()
        .allowed_resource_ids
        .push("resource-64".into());
    assert_eq!(validate_request(&value), Err(ContractError::InvalidInput));
    value.task.as_mut().unwrap().allowed_resource_ids.pop();
    let mut extra = value.evidence[0].clone();
    extra.id = "evidence-64".into();
    value.evidence.push(extra);
    assert_eq!(validate_request(&value), Err(ContractError::InvalidInput));

    let mut wire = serde_json::to_vec(&request()).unwrap();
    wire.resize(MAX_MESSAGE_BYTES, b' ');
    assert!(decode_request(&wire).is_ok());
    wire.push(b' ');
    assert_eq!(decode_request(&wire), Err(ContractError::MessageTooLarge));
    let mut oversized = request();
    oversized.task.as_mut().unwrap().text = "x".repeat(MAX_MESSAGE_BYTES);
    assert_eq!(
        validate_request(&oversized),
        Err(ContractError::MessageTooLarge)
    );
}

#[test]
fn evidence_budget_exhaustion_is_a_valid_block_not_a_protocol_error() {
    let mut value = request();
    let size = serde_json::to_vec(&value.evidence).unwrap().len();
    value.budget.max_evidence_items = 1;
    value.budget.max_evidence_bytes = size;
    assert_eq!(check(&value).unwrap().answers[0].reason, Reason::Allowed);
    for (items, bytes) in [(0, size), (1, size - 1)] {
        value.budget.max_evidence_items = items;
        value.budget.max_evidence_bytes = bytes;
        let response = check(&value).unwrap();
        assert_eq!(response.outcome.control.candidate, CandidateControl::Block);
        assert_eq!(response.answers[0].reason, Reason::EvidenceBudgetExceeded);
        assert_eq!(response.answers[0].confidence_ppm, 0);
    }
    value.check_at_ms = value.budget.deadline_ms;
    assert_eq!(
        check(&value).unwrap().answers[0].reason,
        Reason::BudgetExceeded
    );
}

#[test]
fn freshness_uses_recorded_time_and_exclusive_expiry() {
    let original = request();
    for (captured, until, expected) in [
        (1500, 1501, Reason::Allowed),
        (1501, 1900, Reason::StaleEvidence),
        (1200, 1500, Reason::StaleEvidence),
        (1200, 1499, Reason::StaleEvidence),
    ] {
        let mut value = original.clone();
        value.evidence[0].captured_at_ms = captured;
        value.evidence[0].valid_until_ms = until;
        let response = check(&value).unwrap();
        assert_eq!(response.answers[0].reason, expected);
        assert_eq!(response.answers[0].evidence_refs, ["rebuild-1"]);
    }
}

#[test]
fn irrelevant_and_conflicting_evidence_cannot_turn_into_allowance() {
    let mut value = request();
    value.evidence[0].fact = EvidenceFact::Rebuild {
        resource_id: "another-result".into(),
        result: RebuildResult::Identical,
    };
    assert_eq!(
        check(&value).unwrap().answers[0].reason,
        Reason::MissingEvidence
    );

    let mut value = request();
    let mut second = value.evidence[0].clone();
    second.id = "another-rebuild".into();
    second.fact = EvidenceFact::Rebuild {
        resource_id: "result-1".into(),
        result: RebuildResult::Unique,
    };
    value.evidence.push(second);
    let conflict = check(&value).unwrap();
    assert_eq!(conflict.answers[0].reason, Reason::ConflictingEvidence);
    assert_eq!(conflict.answers[0].value, AnswerValue::Unknown);
    assert_eq!(
        conflict.answers[0].evidence_refs,
        ["another-rebuild", "rebuild-1"]
    );
    value.evidence.reverse();
    assert_eq!(check(&value).unwrap().answers, conflict.answers);
}

#[test]
fn a_later_allow_does_not_erase_a_block_or_a_coverage_gap() {
    let mut value = decode_request(include_bytes!("../examples/missing-evidence.json")).unwrap();
    for scenarios in [
        vec![Scenario::Preservation, Scenario::TaskScope],
        vec![Scenario::TaskScope, Scenario::Preservation],
    ] {
        value.scenarios = scenarios;
        let response = check(&value).unwrap();
        assert_eq!(response.answers[0].value, AnswerValue::Unknown);
        assert_eq!(response.answers[1].value, AnswerValue::Allow);
        assert_eq!(response.outcome.control.candidate, CandidateControl::Block);
    }
    let mut gap = decode_request(include_bytes!("../examples/coverage-gap.json")).unwrap();
    gap.evidence[0].fact = EvidenceFact::Rebuild {
        resource_id: "result-1".into(),
        result: RebuildResult::Unique,
    };
    let response = check(&gap).unwrap();
    assert_eq!(response.answers[0].reason, Reason::NeedsSnapshot);
    assert_eq!(response.answers[1].reason, Reason::EvidenceGap);
    assert_eq!(response.outcome.control.candidate, CandidateControl::Block);
    assert_eq!(response.outcome.obligations.len(), 1);
    assert_eq!(response.outcome.obligations[0].id, "audit");
}
