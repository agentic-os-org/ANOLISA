//! Release composition metadata stays separate from Provider and Panel protocols.
use serde_json::{json, Value};

#[test]
fn independent_composition_fixture_is_valid_and_closed() {
    let schema: Value = serde_json::from_str(include_str!(
        "../packaging/agentsight/composition.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../packaging/agentsight/composition.example.json"
    ))
    .unwrap();
    assert!(validator.is_valid(&fixture));
    assert_ne!(
        fixture["aw"]["components"][0]["version"],
        fixture["agentsight"]["version"]
    );
    for (pointer, value) in [
        ("/format", json!(2)),
        ("/agentsight/ownership", json!("aw-provider")),
        ("/agentsight/archive", json!("../escape.tar.gz")),
        ("/agentsight/sha256", json!("not-a-checksum")),
        ("/aw/components/0/component", json!("agentsight")),
        ("/platform/arch", json!("unknown")),
    ] {
        let mut bad = fixture.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(!validator.is_valid(&bad), "accepted {pointer}");
    }
    let mut bad = fixture;
    bad["ready"] = json!(true);
    assert!(!validator.is_valid(&bad));
}
