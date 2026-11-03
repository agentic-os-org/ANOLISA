//! Common metadata fixtures exercise both producers without importing their SDKs.

mod common;

use aw_contracts::{canonical, objects, Error};
use common::{fixtures, REGISTRY};
use serde_json::{json, Value};

fn event(adapter: &str, name: &str) -> Value {
    let mut value = fixtures()["execution-event-v1alpha1"].clone();
    value["data"]["subject"]["adapterId"] = json!(adapter);
    value["data"]["name"] = json!(name);
    match name {
        "tool.after" => {}
        "tool.before" => {
            let details = value["data"]["details"].as_object_mut().unwrap();
            details.remove("resultDigest");
            details.remove("resultStatus");
        }
        "input.submit" | "turn.ended" => {
            value["data"]["subject"]["toolCallId"] = Value::Null;
            value["data"]["nativeRefs"]["toolCallId"] = Value::Null;
            value["data"]["association"]["verifiedFields"] = json!(["sessionId"]);
            value["data"]["details"] = if name == "input.submit" {
                json!({"inputDigest": value["data"]["details"]["inputDigest"]})
            } else {
                json!({"outcome": "cancelled", "reasonCode": "aw.cancelled"})
            };
        }
        "instance.started" | "instance.ended" => {
            for field in ["sessionId", "turnId", "toolCallId"] {
                value["data"]["subject"][field] = Value::Null;
                value["data"]["nativeRefs"][field] = Value::Null;
            }
            value["data"]["association"] = json!({
                "status": "unassociated", "basis": "none", "verifiedFields": [],
                "gaps": ["aw.instance_has_no_request"]
            });
            value["data"]["details"] = if name == "instance.started" {
                json!({"pid": 42})
            } else {
                json!({"outcome": "exited", "exitCode": null, "signal": 15, "reasonCode": null})
            };
        }
        _ => panic!("missing test event: {name}"),
    }
    value
}

fn record(event: &Value) -> Value {
    let mut value = fixtures()["audit-record-v1alpha1"].clone();
    value["data"]["kind"] = json!("event.observed");
    value["data"]["details"] = json!({});
    value["data"]["eventRef"] = objects::reference(&REGISTRY, event).unwrap();
    value["data"]["eventName"] = event["data"]["name"].clone();
    value["data"]["subject"] = event["data"]["subject"].clone();
    value["data"]["coverage"]["scope"] = json!({
        "level": "instance", "events": [event["data"]["name"]]
    });
    value
}

#[test]
fn registered_object_pairs_are_exact_and_separate_from_existing_protocols() {
    for (name, _) in aw_contracts::registry::SCHEMAS {
        let fixture = fixtures()[name].clone();
        if let (Some(version), Some(kind)) =
            (fixture["apiVersion"].as_str(), fixture["kind"].as_str())
        {
            assert_eq!(REGISTRY.object_schema(version, kind).unwrap(), *name);
            REGISTRY.validate_object(&fixture).unwrap();
        }
    }
    for (version, kind) in [
        ("runtime.aw/v1alpha2", "ExecutionEvent"),
        ("runtime.aw/v1alpha1", "AuditRecord"),
        ("aw/v1alpha1", "AWConfiguration"),
        ("aw.evidence/v1", "Evidence"),
    ] {
        let error = REGISTRY.object_schema(version, kind).unwrap_err();
        assert!(matches!(error, Error::UnsupportedSchema(_)));
    }
    REGISTRY
        .validate("evidence-v1", &fixtures()["evidence-v1"])
        .unwrap();
}

#[test]
fn both_adapters_share_tool_turn_and_instance_shapes() {
    for adapter in ["qoder-sdk", "qwenpaw"] {
        for name in [
            "instance.started",
            "instance.ended",
            "input.submit",
            "turn.ended",
            "tool.before",
            "tool.after",
        ] {
            let event = event(adapter, name);
            let bytes = serde_json::to_vec_pretty(&event).unwrap();
            let decoded = canonical::parse(&bytes).unwrap();
            REGISTRY.validate_object(&decoded).unwrap();
            objects::validate_audit_event(&REGISTRY, &record(&event), &decoded).unwrap();
        }
    }
    // A syntactically valid ID never admits an Adapter or its native capabilities.
    REGISTRY
        .validate_object(&event("community-sdk", "tool.after"))
        .unwrap();
}

#[test]
fn metadata_rejects_raw_payloads_and_unknown_fields_at_every_object_depth() {
    fn check(value: &Value, original: &Value, path: &str) {
        match value {
            Value::Object(map) => {
                let mut changed = original.clone();
                changed.pointer_mut(path).unwrap()["unregistered"] = json!("secret");
                assert!(REGISTRY.validate_object(&changed).is_err(), "{path}");
                for (key, child) in map {
                    check(child, original, &format!("{path}/{key}"));
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    check(child, original, &format!("{path}/{index}"));
                }
            }
            _ => {}
        }
    }
    for fixture in ["execution-event-v1alpha1", "audit-record-v1alpha1"] {
        let original = fixtures()[fixture].clone();
        check(&original, &original, "");
        for field in [
            "input",
            "result",
            "native",
            "text",
            "environment",
            "credentials",
        ] {
            let mut bad = original.clone();
            bad["data"]["details"][field] = json!("private payload");
            assert!(REGISTRY.validate_object(&bad).is_err(), "{field}");
        }
    }
}

#[test]
fn unknown_time_malformed_time_identifiers_and_digests_stay_distinct() {
    let sample = event("qoder-sdk", "tool.after");
    for (pointer, invalid) in [
        ("/metadata/createdAt", json!("2026-02-30T00:00:00.000Z")),
        ("/metadata/createdAt", json!("2026-11-02T00:00:00Z")),
        (
            "/metadata/createdAt",
            json!("2026-11-02T00:00:00.000+00:00"),
        ),
        ("/metadata/createdAt", Value::Null),
        ("/metadata/uid", json!("id\n")),
        ("/metadata/producer/scopeId", json!("")),
        ("/data/subject/adapterId", json!("Bad/Adapter")),
        ("/data/subject/sessionId", json!("x".repeat(513))),
        ("/data/subject/sessionId", json!("session\u{0}")),
        ("/data/subject/configRevision/algorithm", json!("sha1")),
        ("/data/subject/configRevision/value", json!("A".repeat(64))),
        (
            "/data/subject/configRevision/value",
            json!(format!("{}\n", "a".repeat(63))),
        ),
        ("/data/times/receivedAt", json!("unknown")),
    ] {
        let mut bad = sample.clone();
        *bad.pointer_mut(pointer).unwrap() = invalid;
        assert!(REGISTRY.validate_object(&bad).is_err(), "{pointer}");
    }
    assert!(sample["data"]["times"]["occurredAt"].is_null());
    REGISTRY.validate_object(&sample).unwrap();
}

#[test]
fn only_applicable_identities_can_be_asserted_or_verified() {
    let sample = event("qwenpaw", "input.submit");
    assert!(sample["data"]["subject"]["toolCallId"].is_null());
    REGISTRY.validate_object(&sample).unwrap();
    for (pointer, invalid) in [
        ("/data/subject/turnId", Value::Null),
        ("/data/subject/toolCallId", json!("fabricated-call")),
        ("/data/association/verifiedFields", json!(["toolCallId"])),
        (
            "/data/association/verifiedFields",
            json!(["sessionId", "sessionId"]),
        ),
        ("/data/association/basis", json!("none")),
        ("/data/association/status", json!("unassociated")),
        ("/data/nativeRefs/toolCallId", json!("native-call")),
    ] {
        let mut bad = sample.clone();
        *bad.pointer_mut(pointer).unwrap() = invalid;
        assert!(REGISTRY.validate_object(&bad).is_err(), "{pointer}");
    }
    let mut no_turn = event("qoder-sdk", "tool.before");
    no_turn["data"]["subject"]["turnId"] = Value::Null;
    REGISTRY.validate_object(&no_turn).unwrap();
    no_turn["data"]["association"]["verifiedFields"] = json!(["turnId"]);
    assert!(REGISTRY.validate_object(&no_turn).is_err());
}

#[test]
fn lifecycle_and_tool_outcomes_do_not_share_a_generic_success_flag() {
    let mut ended = event("qoder-sdk", "instance.ended");
    ended["data"]["details"]["exitCode"] = json!(0);
    assert!(REGISTRY.validate_object(&ended).is_err());
    ended["data"]["details"] = json!({
        "outcome": "incomplete", "exitCode": null, "signal": null,
        "reasonCode": "aw.owner_lost"
    });
    REGISTRY.validate_object(&ended).unwrap();
    ended["data"]["details"]["reasonCode"] = Value::Null;
    assert!(REGISTRY.validate_object(&ended).is_err());
    let mut before = event("qoder-sdk", "tool.before");
    before["data"]["details"]["resultStatus"] = json!("success");
    assert!(REGISTRY.validate_object(&before).is_err());
}

#[test]
fn reason_codes_require_an_owner_namespace_and_preserve_unknown_values() {
    let mut sample = event("qoder-sdk", "tool.after");
    for code in [
        "native_turn_unavailable",
        "aw.",
        ".missing",
        "AW.missing",
        "aw.a.b",
    ] {
        sample["data"]["association"]["gaps"] = json!([code]);
        assert!(REGISTRY.validate_object(&sample).is_err(), "{code}");
    }
    for code in [
        "aw.native_turn_unavailable",
        "community.identity_unavailable",
    ] {
        sample["data"]["association"]["gaps"] = json!([code]);
        REGISTRY.validate_object(&sample).unwrap();
        let decoded = canonical::parse(&serde_json::to_vec(&sample).unwrap()).unwrap();
        assert_eq!(decoded["data"]["association"]["gaps"][0], code);
    }
    let mut audit = record(&sample);
    audit["data"]["coverage"]["gaps"] = json!(["aw.future_coverage_gap"]);
    REGISTRY.validate_object(&audit).unwrap();
    audit["data"]["coverage"]["gaps"] = json!(["missing"]);
    assert!(REGISTRY.validate_object(&audit).is_err());
    let mut terminal = event("qoder-sdk", "turn.ended");
    terminal["data"]["details"]["reasonCode"] = json!("cancelled");
    assert!(REGISTRY.validate_object(&terminal).is_err());
    let mut failed = fixtures()["audit-record-v1alpha1"].clone();
    failed["data"]["details"]["status"] = json!("failed");
    failed["data"]["details"]["failureAction"] = json!("report");
    failed["data"]["details"]["policyBlock"] = Value::Null;
    failed["data"]["details"]["failureCode"] = json!("aw.provider_timeout");
    REGISTRY.validate_object(&failed).unwrap();
    failed["data"]["details"]["failureCode"] = json!("timeout");
    assert!(REGISTRY.validate_object(&failed).is_err());
}

#[test]
fn native_and_received_timestamps_do_not_assert_a_shared_clock() {
    let mut sample = event("qoder-sdk", "tool.after");
    sample["data"]["times"]["receivedAt"] = json!("2026-11-02T00:00:01.000Z");
    for occurred in [
        Value::Null,
        json!("2026-11-02T00:00:00.000Z"),
        json!("2026-11-02T00:00:01.000Z"),
        json!("2026-11-02T00:00:02.000Z"),
    ] {
        sample["data"]["times"]["occurredAt"] = occurred;
        REGISTRY.validate_object(&sample).unwrap();
        objects::validate_audit_event(&REGISTRY, &record(&sample), &sample).unwrap();
    }
}

#[test]
fn coverage_requires_a_bounded_scope_and_rejects_false_complete_shapes() {
    let mut complete = fixtures()["audit-record-v1alpha1"].clone();
    complete["data"]["coverage"]["status"] = json!("complete");
    complete["data"]["coverage"]["gaps"] = json!([]);
    complete["data"]["coverage"]["interval"] = json!({
        "start": "2026-11-02T00:00:00.000Z", "end": "2026-11-02T00:00:01.000Z"
    });
    REGISTRY.validate_object(&complete).unwrap();
    for (pointer, invalid) in [
        ("/data/coverage/interval/start", Value::Null),
        (
            "/data/coverage/interval/start",
            json!("2026-11-03T00:00:00.000Z"),
        ),
        ("/data/coverage/gaps", json!(["aw.source_unavailable"])),
        ("/data/coverage/truncated", json!(true)),
        ("/data/coverage/scope/events", json!([])),
        ("/data/coverage/scope/level", json!("global")),
        ("/data/coverage/status", json!("unknown")),
    ] {
        let mut bad = complete.clone();
        *bad.pointer_mut(pointer).unwrap() = invalid;
        assert!(REGISTRY.validate_object(&bad).is_err(), "{pointer}");
    }
    let mut instance = record(&event("qoder-sdk", "instance.started"));
    instance["data"]["coverage"]["scope"]["level"] = json!("tool");
    assert!(REGISTRY.validate_object(&instance).is_err());
}

#[test]
fn references_bind_exact_objects_and_normalize_only_metadata_encoding() {
    let sample = event("qwenpaw", "tool.after");
    let expected = objects::reference(&REGISTRY, &sample).unwrap();
    let replay = canonical::parse(&serde_json::to_vec_pretty(&sample).unwrap()).unwrap();
    objects::validate_reference(&REGISTRY, &expected, &replay).unwrap();
    for pointer in [
        "/metadata/uid",
        "/metadata/producer/producerId",
        "/metadata/producer/scopeId",
        "/metadata/producer/generation",
        "/data/subject/instanceId",
        "/data/subject/toolCallId",
    ] {
        let mut changed = sample.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("other");
        assert!(
            objects::validate_reference(&REGISTRY, &expected, &changed).is_err(),
            "{pointer}"
        );
    }
    let mut changed_ref = expected.clone();
    changed_ref["unknown"] = json!(true);
    assert!(objects::validate_reference(&REGISTRY, &changed_ref, &sample).is_err());
    let mut audit = record(&sample);
    audit["data"]["subject"]["instanceId"] = json!("other");
    REGISTRY.validate_object(&audit).unwrap();
    assert!(objects::validate_audit_event(&REGISTRY, &audit, &sample).is_err());
}

#[test]
fn replay_identity_uses_source_position_and_conflicting_content_keeps_its_uid() {
    let sample = fixtures()["audit-record-v1alpha1"].clone();
    let original = objects::reference(&REGISTRY, &sample).unwrap();
    let replay = canonical::parse(&serde_json::to_vec_pretty(&sample).unwrap()).unwrap();
    assert_eq!(objects::reference(&REGISTRY, &replay).unwrap(), original);
    let mut conflict = sample.clone();
    conflict["data"]["chain"]["sourceDigest"]["value"] = json!("0".repeat(64));
    let reference = objects::reference(&REGISTRY, &conflict).unwrap();
    assert_eq!(reference["uid"], original["uid"]);
    assert_ne!(reference["digest"], original["digest"]);
    for pointer in ["/data/chain/id", "/metadata/producer/generation"] {
        let mut changed = sample.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("other");
        assert!(REGISTRY.validate_object(&changed).is_err(), "{pointer}");
    }
    let mut changed = sample.clone();
    changed["data"]["chain"]["sequence"] = json!(2);
    assert!(REGISTRY.validate_object(&changed).is_err());
    assert_ne!(
        objects::audit_uid(&sample["metadata"]["producer"], "chain-1", 2).unwrap(),
        original["uid"]
    );
    assert!(objects::audit_uid(&json!({}), "chain", u64::MAX).is_err());
}

#[test]
fn provider_failure_and_policy_denial_have_separate_post_tool_constraints() {
    let mut record = fixtures()["audit-record-v1alpha1"].clone();
    record["data"]["details"]["policyBlock"] = json!(true);
    assert!(REGISTRY.validate_object(&record).is_err());
    record["data"]["eventName"] = json!("tool.before");
    REGISTRY.validate_object(&record).unwrap();
    record["data"]["details"]["status"] = json!("failed");
    assert!(REGISTRY.validate_object(&record).is_err());
    record["data"]["details"]["policyBlock"] = Value::Null;
    record["data"]["details"]["failureCode"] = json!("aw.provider_timeout");
    record["data"]["details"]["failureAction"] = json!("withhold_result");
    assert!(REGISTRY.validate_object(&record).is_err());
    record["data"]["eventName"] = json!("tool.after");
    REGISTRY.validate_object(&record).unwrap();
    record["data"]["kind"] = json!("event.closed");
    record["data"]["details"] =
        json!({"outcome": "incomplete", "reasonCode": "aw.cancel_cleanup_pending"});
    REGISTRY.validate_object(&record).unwrap();
}
