//! Snapshot validation binds exact content, current subjects and execution provenance.

mod common;
#[path = "evidence/provenance.rs"]
mod provenance;

use aw_contracts::{canonical, evidence::validate_context};
use common::{fixtures, REGISTRY};
use serde_json::{json, Value};

fn resign(value: &mut Value) {
    value.as_object_mut().unwrap().remove("context_revision");
    value["context_revision"] = json!(canonical::document_digest(value).unwrap());
}

#[test]
fn exact_content_and_subject_are_required_even_with_a_fresh_outer_digest() {
    let sample = fixtures()["evidence-context-v1"].clone();
    let subject = &sample["subject"];
    let candidate = sample["candidate_revision"].as_str().unwrap();
    validate_context(&REGISTRY, &sample, subject, candidate).unwrap();
    for mutation in [
        "subject",
        "candidate",
        "content",
        "reference",
        "evidence",
        "execution",
        "constraints",
        "bytes",
        "deadline",
    ] {
        let mut changed = sample.clone();
        match mutation {
            "subject" => changed["subject"]["instance_id"] = json!("a".repeat(64)),
            "candidate" => changed["candidate_revision"] = json!("a".repeat(64)),
            "content" => changed["contents"][0]["text"] = json!("changed"),
            "reference" => changed["evidence"][0]["content_ref"] = json!("a".repeat(64)),
            "evidence" => changed["evidence"][0]["evidence_id"] = json!("a".repeat(64)),
            "execution" => changed["execution_refs"] = json!(["a".repeat(64)]),
            "constraints" => changed["constraints_ref"] = json!("a".repeat(64)),
            "bytes" => {
                changed["contents"][1]["text"] = json!("x".repeat(5000));
                changed["limits"]["max_bytes"] = json!(4096);
            }
            "deadline" => changed["original_deadline_ns"] = json!("99999999999999999999"),
            _ => unreachable!(),
        }
        resign(&mut changed);
        assert!(
            validate_context(&REGISTRY, &changed, subject, candidate).is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn source_history_and_process_claims_cannot_certify_executions() {
    let sample = fixtures()["evidence-v1"].clone();
    for (pointer, value) in [
        ("/kind", json!("trajectory.step")),
        ("/association/status", json!("unverified")),
        ("/association/tool_call_id", Value::Null),
        ("/association/basis", json!("process")),
        ("/trust", json!("source_reported")),
    ] {
        let mut bad = sample.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(REGISTRY.validate("evidence-v1", &bad).is_err(), "{pointer}");
    }
    // PID-only metadata has no association shape. Future process sources need a
    // separate reviewed contract with host, boot and process-start identities.
    let mut bad = sample;
    bad["association"]["pid"] = json!(42);
    assert!(REGISTRY.validate("evidence-v1", &bad).is_err());
}

#[test]
fn native_json_numbers_and_unicode_keys_stay_inside_encoded_content() {
    let context = fixtures()["evidence-context-v1"].clone();
    let decoded: Value =
        serde_json::from_str(context["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(decoded["tool"]["input"]["ratio"], 0.125);
    assert!(canonical::bytes(&decoded).is_err());
    validate_context(
        &REGISTRY,
        &context,
        &context["subject"],
        context["candidate_revision"].as_str().unwrap(),
    )
    .unwrap();
}
