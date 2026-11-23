//! SDK startup hints use one strict and versioned deployment shape.

use aw_contracts::deployment::{Availability, DeploymentDescriptor, VERSION};
use serde_json::{json, Value};

fn document() -> Value {
    json!({
        "version": VERSION,
        "configuration_sha256": "a".repeat(64),
        "target": "Worker.v1",
        "profile": {"adapter":"qoder-sdk","version":"public-hooks/v1","entrypoint":"sdk-worker"},
        "endpoint":"/run/aw/aw.sock", "wait_for_input_submit":true,"availability":"required"
    })
}

fn parse(value: &Value) -> Result<DeploymentDescriptor, aw_contracts::Error> {
    DeploymentDescriptor::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn deployment_round_trip_exposes_only_startup_hints() {
    let value = document();
    let descriptor = parse(&value).unwrap();
    assert_eq!(descriptor.version(), VERSION);
    assert_eq!(descriptor.configuration_sha256(), "a".repeat(64));
    assert_eq!(descriptor.target(), "Worker.v1");
    assert_eq!(descriptor.profile().adapter, "qoder-sdk");
    assert_eq!(descriptor.endpoint(), "/run/aw/aw.sock");
    assert!(descriptor.wait_for_input_submit());
    assert!(!descriptor.observe_session_start());
    assert_eq!(descriptor.availability(), Availability::Required);
    assert_eq!(serde_json::to_value(&descriptor).unwrap(), value);
}

#[test]
fn deployment_round_trips_an_enabled_session_start_hint() {
    let mut value = document();
    value["observe_session_start"] = json!(true);
    let descriptor = parse(&value).unwrap();
    assert!(descriptor.observe_session_start());
    assert_eq!(serde_json::to_value(&descriptor).unwrap(), value);
}

#[test]
fn deployment_rejects_unknown_missing_mistyped_or_mismatched_fields() {
    for (field, value) in [
        ("version", json!("aw-deployment/v2")),
        ("configuration_sha256", json!("A".repeat(64))),
        (
            "configuration_sha256",
            json!(format!("{}\n", "a".repeat(64))),
        ),
        ("target", json!("worker\n")),
        ("endpoint", json!("relative.sock")),
        ("endpoint", json!("/invalid\0.sock")),
        ("wait_for_input_submit", json!("true")),
        ("observe_session_start", json!("true")),
        ("availability", json!("optional")),
        ("unexpected", json!(true)),
    ] {
        let mut bad = document();
        bad[field] = value;
        assert!(parse(&bad).is_err(), "{field}");
    }
    for field in document().as_object().unwrap().keys() {
        let mut bad = document();
        bad.as_object_mut().unwrap().remove(field);
        assert!(parse(&bad).is_err(), "{field}");
    }
    let mut bad = document();
    bad["profile"]["unexpected"] = json!(true);
    assert!(parse(&bad).is_err());
    bad["profile"] = json!({"adapter":"qoder-sdk","version":" ","entrypoint":"sdk-worker"});
    assert!(parse(&bad).is_err());
}

#[test]
fn deployment_rejects_duplicate_keys_before_shape_validation() {
    let original = serde_json::to_string(&document()).unwrap();
    let duplicate = original.replacen('{', "{\"wait_for_input_submit\":false,", 1);
    assert!(DeploymentDescriptor::parse(duplicate.as_bytes()).is_err());
    let nested = original.replace("\"adapter\":", "\"adapter\":\"wrong\",\"adapter\":");
    assert!(DeploymentDescriptor::parse(nested.as_bytes()).is_err());
}

#[test]
fn deployment_profile_uses_unicode_white_space() {
    // Mirror the SDK unicode_profile vectors without depending on SDK assets.
    for (value, valid) in [
        ("\u{0085}", false),
        (" \u{0085}\u{2007}\u{202f}", false),
        ("\u{feff}", true),
        ("\u{200b}", true),
        ("\u{001c}", true),
        ("配置😀", true),
    ] {
        for field in ["adapter", "version", "entrypoint"] {
            let mut candidate = document();
            candidate["profile"][field] = json!(value);
            assert_eq!(parse(&candidate).is_ok(), valid, "{field}: {value:?}");
        }
    }
}
