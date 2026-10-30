//! Offline policy boundaries preserve launch configuration across candidate edits.

use aw_config::{Configuration, Error, Validator};
use serde_json::{json, Value};
use std::sync::LazyLock;

static VALIDATOR: LazyLock<Validator> = LazyLock::new(|| Validator::new().unwrap());

fn example() -> Configuration {
    VALIDATOR
        .parse(include_bytes!("../examples/aw.minimal.yaml"))
        .unwrap()
}

fn parse(value: &Value) -> Configuration {
    VALIDATOR
        .parse(&serde_json::to_vec(value).unwrap())
        .unwrap()
}

#[test]
fn providers_hooks_and_budgets_can_be_added_updated_and_removed() {
    let baseline = example();
    let mut candidate = baseline.as_value().clone();
    candidate["spec"]["providers"]["policy"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":["/not-installed/provider"]},
        "timeout_ms":1000,"max_output_bytes":4096,
        "config":{"private":"not-displayed","threshold":0.75}});
    candidate["spec"]["events"]["tool.before"]["steps"] = json!([
        {"id":"check","provider":"policy","operation":"check",
         "effects":["observe","block"],"on_error":"block"}]);
    candidate["spec"]["events"]["tool.after"]["budget_ms"] = json!(2000);
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(3000);
    let added = parse(&candidate);
    baseline.validate_policy_update(&added).unwrap();

    candidate["spec"]["providers"]["policy"]["config"]["threshold"] = json!(0.5);
    candidate["spec"]["events"]["tool.before"]["steps"][0]["on_error"] = json!("report");
    let updated = parse(&candidate);
    added.validate_policy_update(&updated).unwrap();
    updated.validate_policy_update(&baseline).unwrap();
}

#[test]
fn launch_and_service_edits_are_rejected_without_values() {
    let baseline = example();
    for (pointer, replacement, section) in [
        ("/metadata/name", json!("private-label"), "/metadata"),
        ("/spec/daemon/startup", json!("external"), "/spec/daemon"),
        (
            "/spec/daemon/endpoint",
            json!("/private/aw.sock"),
            "/spec/daemon",
        ),
        (
            "/spec/daemon/state_dir",
            json!("/private/state"),
            "/spec/daemon",
        ),
        (
            "/spec/agents/qoder/argv",
            json!(["/private/agent"]),
            "/spec/agents",
        ),
        (
            "/spec/agents/qoder/adapter",
            json!("qwenpaw"),
            "/spec/agents",
        ),
        (
            "/spec/agents/qoder/qoder",
            json!({"sequential":true}),
            "/spec/agents",
        ),
    ] {
        let mut candidate = baseline.as_value().clone();
        // Optional public fields do not yet have a JSON Pointer in the starter.
        if pointer.ends_with("/qoder") {
            candidate["spec"]["agents"]["qoder"]["qoder"] = replacement;
        } else {
            *candidate.pointer_mut(pointer).unwrap() = replacement;
        }
        let candidate = parse(&candidate);
        for (previous, next) in [(&baseline, &candidate), (&candidate, &baseline)] {
            let error = previous.validate_policy_update(next).unwrap_err();
            assert!(matches!(&error, Error::Invariant { path, .. } if path == section));
            assert!(!error.to_string().contains("private"));
        }
    }
    let mut candidate = baseline.as_value().clone();
    candidate["spec"]["agents"]["qwenpaw"] = json!({"adapter":"qwenpaw","argv":["qwenpaw","app"]});
    assert!(baseline.validate_policy_update(&parse(&candidate)).is_err());
}

#[test]
fn optional_frozen_fields_cannot_be_added_or_removed() {
    let baseline = example();
    let mut value = baseline.as_value().clone();
    value["spec"]["agents"]["qoder"]["evidence"] = json!({
        "source_id":"sight","endpoint":"http://127.0.0.1:12345",
        "query_timeout_ms":1000,"query_bytes":4096,"max_items":8,"max_bytes":4096});
    let candidate = parse(&value);
    assert!(baseline.validate_policy_update(&candidate).is_err());
    assert!(candidate.validate_policy_update(&baseline).is_err());
    value["spec"]["agents"]["qoder"]["evidence"]["query_timeout_ms"] = json!(500);
    assert!(candidate.validate_policy_update(&parse(&value)).is_err());
}

fn observed() -> Value {
    let mut value = example().as_value().clone();
    value["spec"]["agents"]["qoder"]["evidence"] = json!({
        "source_id":"sight","endpoint":"http://127.0.0.1:12345",
        "query_timeout_ms":1000,"query_bytes":4096,"max_items":8,"max_bytes":8192});
    value
}

#[test]
fn equivalent_schema_integer_spellings_preserve_frozen_observation_limits() {
    for (field, limits) in [
        ("query_timeout_ms", [1, 1000, 60000]),
        ("query_bytes", [1, 4096, 4194304]),
        ("max_items", [1, 8, 128]),
        ("max_bytes", [4096, 8192, 2097152]),
    ] {
        for limit in limits {
            let mut value = observed();
            value["spec"]["agents"]["qoder"]["evidence"][field] = json!(limit);
            let baseline = parse(&value);
            let encoded = serde_json::to_string(&value).unwrap();
            for spelling in [format!("{limit}.0"), format!("{limit}e0")] {
                let candidate = VALIDATOR
                    .parse(
                        encoded
                            .replace(
                                &format!("\"{field}\":{limit}"),
                                &format!("\"{field}\":{spelling}"),
                            )
                            .as_bytes(),
                    )
                    .unwrap();
                assert!(
                    candidate.as_value()["spec"]["agents"]["qoder"]["evidence"][field]
                        .as_number()
                        .unwrap()
                        .is_f64()
                );
                baseline.validate_policy_update(&candidate).unwrap();
                candidate.validate_policy_update(&baseline).unwrap();
                assert_eq!(baseline.as_value(), &value);
            }
        }
    }
}

#[test]
fn different_observation_limits_remain_frozen_in_either_number_representation() {
    let value = observed();
    let baseline = parse(&value);
    for field in ["query_timeout_ms", "query_bytes", "max_items", "max_bytes"] {
        let limit = value["spec"]["agents"]["qoder"]["evidence"][field]
            .as_u64()
            .unwrap();
        for changed in [json!(limit + 1), json!((limit + 1) as f64)] {
            let mut candidate = value.clone();
            candidate["spec"]["agents"]["qoder"]["evidence"][field] = changed;
            let candidate = parse(&candidate);
            assert!(baseline.validate_policy_update(&candidate).is_err());
            assert!(candidate.validate_policy_update(&baseline).is_err());
        }
        for invalid in [
            json!(limit as f64 + 0.5),
            json!(limit.to_string()),
            json!(true),
        ] {
            let mut candidate = value.clone();
            candidate["spec"]["agents"]["qoder"]["evidence"][field] = invalid;
            assert!(VALIDATOR
                .parse(&serde_json::to_vec(&candidate).unwrap())
                .is_err());
        }
    }
}

#[test]
fn comparison_is_offline_and_does_not_depend_on_the_adapter() {
    for adapter in [
        "qoder",
        "qoder-sdk",
        "qwenpaw",
        "openclaw",
        "hermes",
        "pi",
        "claude-sdk",
        "agentscope",
    ] {
        let mut value = example().as_value().clone();
        value["spec"]["agents"] =
            json!({"worker":{"adapter":adapter,"argv":["/not-installed/agent"]}});
        let baseline = parse(&value);
        value["spec"]["events"]["tool.before"]["enabled"] = json!(false);
        value["spec"]["events"]["tool.before"]["required"] = json!(false);
        baseline.validate_policy_update(&parse(&value)).unwrap();
    }
}

#[test]
fn parsing_and_static_references_are_required_before_comparison() {
    let mut value = example().as_value().clone();
    value["spec"]["events"]["tool.before"]["steps"] = json!([
        {"id":"missing","provider":"unknown","operation":"check",
         "effects":["observe"],"on_error":"report"}]);
    assert!(VALIDATOR
        .parse(&serde_json::to_vec(&value).unwrap())
        .is_err());
    assert!(VALIDATOR.parse(b"spec: {}\nspec: {}\n").is_err());
}

#[test]
fn formatting_and_mapping_order_do_not_change_the_policy_scope() {
    let baseline = example();
    let json = serde_json::to_vec_pretty(baseline.as_value()).unwrap();
    let equivalent = VALIDATOR.parse(&json).unwrap();
    baseline.validate_policy_update(&equivalent).unwrap();
    let yaml = serde_yaml_ng::to_string(equivalent.as_value()).unwrap();
    equivalent
        .validate_policy_update(&VALIDATOR.parse(yaml.as_bytes()).unwrap())
        .unwrap();
}
