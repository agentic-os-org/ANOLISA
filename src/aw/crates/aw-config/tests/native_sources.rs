//! Native bytes are outside the strict AW schema; only references are validated.

use aw_config::Validator;
use serde_json::{json, Value};

fn document() -> Value {
    let mut value: Value =
        serde_json::from_str(include_str!("../examples/aw.minimal.json")).unwrap();
    value["spec"]["agents"]["qoder"]["native_config"] = json!({"sources":[{
        "id":"project","kind":"file","scope":"project","reference":"./native.yaml","base_dir":"./workspace"
    }]});
    value
}

#[test]
fn native_references_preserve_spelling_and_keep_public_schema_strict() {
    let validator = Validator::new().unwrap();
    let valid = document();
    let parsed = validator
        .parse(&serde_json::to_vec(&valid).unwrap())
        .unwrap();
    let sources = parsed.native_sources("qoder").unwrap();
    assert_eq!(sources[0].reference, "./native.yaml");
    assert_eq!(sources[0].base_dir.as_deref(), Some("./workspace"));
    assert!(parsed.native_sources("missing").is_err());
    for (field, value) in [
        ("kind", json!("unknown")),
        ("reference", json!("")),
        ("reference", json!("private\u{0000}value")),
        ("contents", json!({"secret":"private-value"})),
    ] {
        let mut invalid = valid.clone();
        invalid["spec"]["agents"]["qoder"]["native_config"]["sources"][0][field] = value;
        let error = validator
            .parse(&serde_json::to_vec(&invalid).unwrap())
            .err()
            .unwrap();
        assert!(!error.to_string().contains("private-value"));
    }
    let mut duplicate = valid.clone();
    let sources = duplicate["spec"]["agents"]["qoder"]["native_config"]["sources"]
        .as_array_mut()
        .unwrap();
    sources.push(sources[0].clone());
    assert!(validator
        .parse(&serde_json::to_vec(&duplicate).unwrap())
        .is_err());
    let mut misplaced = valid;
    misplaced["spec"]["unknown_native_field"] = json!({"retained":true});
    assert!(validator
        .parse(&serde_json::to_vec(&misplaced).unwrap())
        .is_err());
}

#[test]
fn omitted_sources_preserve_native_discovery_and_updates_cannot_change_references() {
    let validator = Validator::new().unwrap();
    let original = validator
        .parse(include_bytes!("../examples/aw.minimal.json"))
        .unwrap();
    assert!(original.native_sources("qoder").unwrap().is_empty());
    let candidate = validator
        .parse(&serde_json::to_vec(&document()).unwrap())
        .unwrap();
    assert!(original.validate_policy_update(&candidate).is_err());
    let mut changed = document();
    changed["spec"]["execution"]["default_event_budget_ms"] = json!(6000);
    let changed = validator
        .parse(&serde_json::to_vec(&changed).unwrap())
        .unwrap();
    candidate.validate_policy_update(&changed).unwrap();
}
