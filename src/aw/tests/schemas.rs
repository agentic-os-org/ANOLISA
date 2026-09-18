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
