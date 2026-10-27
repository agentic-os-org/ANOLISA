//! Native execution grants stay explicit, bounded and Adapter-specific.

use aw_config::Validator;
use serde_json::{json, Value};

fn document(policy: Value) -> Value {
    let mut value = Validator::new()
        .unwrap()
        .parse(include_bytes!("../examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["agents"]["sdk"] = json!({
        "adapter":"qoder-sdk", "argv":["node", "/application/worker.mjs"],
        "qoder_sdk":policy
    });
    value
}

#[test]
fn grants_preserve_sites_and_explicit_asset_digests() {
    let value = document(json!({"sites":["cn","global"],"declarative_plugins":true,
        "command_wrappers":[{"path":"/app/wrapper","sha256":"a".repeat(64)}],
        "runtime_preloads":[{"path":"/app/preload.mjs","sha256":"b".repeat(64)}]}));
    let parsed = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    assert_eq!(parsed.as_value(), &value);
}

#[test]
fn malformed_or_unknown_native_grants_are_rejected() {
    let validator = Validator::new().unwrap();
    for policy in [
        json!({"sites":[]}),
        json!({"sites":["cn","cn"]}),
        json!({"sites":["other"]}),
        json!({"unknown":true}),
        json!({"command_wrappers":[{"path":"relative","sha256":"a".repeat(64)}]}),
        json!({"runtime_preloads":[{"path":"/app/a","sha256":"not-a-digest"}]}),
        json!({"runtime_preloads":[{"path":"/app/a","sha256":"a".repeat(64),"extra":true}]}),
    ] {
        assert!(
            validator
                .parse(&serde_json::to_vec(&document(policy.clone())).unwrap())
                .is_err(),
            "{policy}"
        );
    }
}

#[test]
fn another_adapter_cannot_acquire_qoder_sdk_grants() {
    let mut value = document(json!({"declarative_plugins":true}));
    value["spec"]["agents"]["sdk"]["adapter"] = json!("pi");
    assert!(Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .is_err());
}
