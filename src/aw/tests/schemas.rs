mod common;

use aw_contracts::registry::SCHEMAS;
use common::{fixtures, REGISTRY};
use serde_json::json;

#[test]
fn every_payload_schema_has_a_valid_example_and_rejects_unknown_fields() {
    let f = fixtures();
    assert_eq!(f.as_object().unwrap().len(), SCHEMAS.len() - 1);
    for (name, _) in SCHEMAS {
        if *name == "common-v1" {
            continue;
        }
        REGISTRY
            .validate(name, &f[*name])
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut bad = f[*name].clone();
        bad["unknown"] = json!(true);
        assert!(REGISTRY.validate(name, &bad).is_err(), "{name}");
    }
    assert!(REGISTRY.validate("unknown-v1", &json!({})).is_err());
}

#[test]
fn schema_mismatch_names_the_offending_path_and_constraint() {
    // A rejected value must surface where and why it failed — the same
    // diagnostics aw-config's shape errors carry — not just the schema name,
    // which forced manual field bisection on every rejected capability plan.
    // The invalid value carries a marker that must never be echoed back:
    // masked() keeps payload values out of the message.
    let mut bad = fixtures()["runtime-binding-v1"].clone();
    bad["owner_id"] = json!("launcher-LEAKMARKER-\u{2205}");
    let Err(err) = REGISTRY.validate("runtime-binding-v1", &bad) else {
        panic!("non-printable owner_id must be rejected")
    };
    let msg = err.to_string();
    assert!(
        msg.contains("schema mismatch: runtime-binding-v1 at `") && msg.contains("/owner_id"),
        "must name the schema and the exact failing instance path: {msg}"
    );
    assert!(
        msg.contains("/properties/owner_id"),
        "must name the violated schema constraint: {msg}"
    );
    assert!(
        !msg.contains("LEAKMARKER"),
        "masked reason must not echo the payload value: {msg}"
    );
}

#[test]
fn evidence_mismatch_uses_the_evidence_format() {
    // validate_evidence is a separate format branch; it must carry the same
    // path/constraint diagnostics under its own prefix.
    let bad = json!({
        "source_id": "ev-LEAKMARKER-\u{2205}",
        "record_id": "record-1",
        "digest": "0".repeat(64),
    });
    let Err(err) = REGISTRY.validate_evidence(&bad) else {
        panic!("non-printable source_id must be rejected")
    };
    let msg = err.to_string();
    assert!(
        msg.contains("schema mismatch: common-v1 evidence at `") && msg.contains("/source_id"),
        "evidence mismatch must keep its own prefix and name the path: {msg}"
    );
    assert!(
        !msg.contains("LEAKMARKER"),
        "masked reason must not echo the payload value: {msg}"
    );
}

#[test]
fn identifiers_reject_line_terminators_in_every_regex_engine() {
    let f = fixtures();
    for suffix in ["\n", "\r", "\u{2028}", "\u{2029}"] {
        let mut bad = f["runtime-binding-v1"].clone();
        bad["owner_id"] = json!(format!("owner{suffix}"));
        assert!(REGISTRY.validate("runtime-binding-v1", &bad).is_err());
        let mut bad = f["provider-receipt-v1"].clone();
        bad["input_digest"] = json!(format!("{}{suffix}", "0".repeat(64)));
        assert!(REGISTRY.validate("provider-receipt-v1", &bad).is_err());
    }
}
