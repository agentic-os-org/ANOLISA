//! Offline deployment hints preserve capability and availability admission.

use aw_contracts::deployment::{Availability, DeploymentDescriptor};
use aw_core::{
    admission::AdapterCapabilities,
    descriptor::{
        compile, compile_with_options, verify, verify_with_options, CompileOptions, DescriptorError,
    },
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn document() -> Value {
    let mut value = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["daemon"]["state_dir"] = json!("/run/aw");
    for event in ["tool.before", "tool.after"] {
        value["spec"]["events"][event]["required"] = json!(false);
    }
    value
}

fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        adapter: "qoder".into(),
        version: "fixture/v1".into(),
        entrypoint: "native".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
            (
                "input.submit".into(),
                vec!["observe".into(), "block".into()],
            ),
        ]),
        failure_actions: BTreeMap::new(),
    }
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn compilation_binds_exact_bytes_target_profile_and_resolved_endpoint() {
    let value = document();
    let input = bytes(&value);
    let descriptor = compile(
        &input,
        "qoder",
        capabilities(),
        Availability::NativeFallback,
    )
    .unwrap();
    assert_eq!(
        descriptor.configuration_sha256(),
        format!("{:x}", Sha256::digest(&input))
    );
    assert_eq!(descriptor.target(), "qoder");
    assert_eq!(descriptor.profile().version, "fixture/v1");
    assert_eq!(descriptor.endpoint(), "/run/aw/aw.sock");
    assert!(!descriptor.wait_for_input_submit());
    assert_eq!(descriptor.availability(), Availability::NativeFallback);
    verify(
        &descriptor,
        &input,
        "qoder",
        capabilities(),
        Availability::NativeFallback,
    )
    .unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    assert!(matches!(
        verify(
            &descriptor,
            &pretty,
            "qoder",
            capabilities(),
            Availability::NativeFallback
        ),
        Err(DescriptorError::Mismatch)
    ));
    let mut caps = capabilities();
    caps.version = "fixture/v2".into();
    assert!(matches!(
        verify(
            &descriptor,
            &input,
            "qoder",
            caps,
            Availability::NativeFallback
        ),
        Err(DescriptorError::Mismatch)
    ));
    assert!(matches!(
        verify(
            &descriptor,
            &input,
            "qoder",
            capabilities(),
            Availability::Required
        ),
        Err(DescriptorError::Mismatch)
    ));
}

#[test]
fn enabled_empty_optional_input_still_requires_wait_and_capability() {
    let mut value = document();
    value["spec"]["events"]["input.submit"] = json!({"enabled":true,"required":false,"steps":[]});
    let descriptor = compile(
        &bytes(&value),
        "qoder",
        capabilities(),
        Availability::NativeFallback,
    )
    .unwrap();
    assert!(descriptor.wait_for_input_submit());
    let mut caps = capabilities();
    caps.events.remove("input.submit");
    assert!(matches!(
        compile(&bytes(&value), "qoder", caps, Availability::NativeFallback),
        Err(DescriptorError::Admission(_))
    ));
    value["spec"]["events"]["input.submit"]["enabled"] = json!(false);
    assert!(!compile(
        &bytes(&value),
        "qoder",
        capabilities(),
        Availability::Required
    )
    .unwrap()
    .wait_for_input_submit());
}

#[test]
fn required_availability_follows_applicable_target_requirements() {
    let mut value = document();
    value["spec"]["agents"]["pi"] = json!({"adapter":"pi","argv":["/missing/agent"]});
    value["spec"]["events"]["tool.before"]["required"] = json!(true);
    value["spec"]["events"]["tool.before"]["match"] = json!({"tools":["native:pi:Shell"]});
    assert!(compile(
        &bytes(&value),
        "qoder",
        capabilities(),
        Availability::NativeFallback
    )
    .is_ok());
    assert!(compile(
        &bytes(&value),
        "qoder",
        capabilities(),
        Availability::Required
    )
    .is_ok());
    value["spec"]["events"]["tool.before"]["match"] = json!({"tools":["native:qoder:Shell"]});
    assert!(matches!(
        compile(
            &bytes(&value),
            "qoder",
            capabilities(),
            Availability::NativeFallback
        ),
        Err(DescriptorError::Availability)
    ));
}

#[test]
fn compilation_preserves_failure_action_admission_and_omits_private_configuration() {
    let mut value = document();
    value["spec"]["providers"]["check"] = json!({
        "protocol":"aw-provider/v1alpha1", "transport":{"type":"stdio","location":"agent","argv":["/missing/provider"]},
        "timeout_ms":10,"max_output_bytes":32,"config":{"credential":"private-value"}
    });
    value["spec"]["events"]["tool.after"]["steps"] = json!([
        {"id":"record","provider":"check","operation":"record","effects":["observe"],"on_error":"withhold_result"}
    ]);
    assert!(matches!(
        compile(
            &bytes(&value),
            "qoder",
            capabilities(),
            Availability::Required
        ),
        Err(DescriptorError::Admission(_))
    ));
    let mut caps = capabilities();
    caps.failure_actions
        .insert("tool.after".into(), vec!["withhold_result".into()]);
    let descriptor = compile(&bytes(&value), "qoder", caps, Availability::Required).unwrap();
    let encoded = serde_json::to_string(&descriptor).unwrap();
    assert!(!encoded.contains("private-value"));
    assert!(!encoded.contains("/missing/provider"));
}

#[test]
fn service_path_rules_apply_without_environment_resolution_or_io() {
    for (endpoint, state, expected) in [
        ("auto", "auto", None),
        ("auto", "relative", None),
        ("relative.sock", "/run/aw", None),
        ("auto/", "/run/aw", None),
        ("/run/aw/aw.sock", "auto/", None),
        ("/explicit.sock", "auto", None),
        ("/run/other/aw.sock", "/run/aw", None),
        ("/run/../aw.sock", "auto", None),
        ("auto", "/run/../aw", None),
        ("auto", "/run/aw\0", None),
        ("/run/aw\0/aw.sock", "auto", None),
        ("/run/aw/aw.sock", "auto", Some("/run/aw/aw.sock")),
        ("/run/aw/aw.sock", "/run/aw", Some("/run/aw/aw.sock")),
        ("auto", "/run/aw", Some("/run/aw/aw.sock")),
        // Service preserves Path's normalization of internal current components.
        ("/run/./aw/aw.sock", "/run/aw", Some("/run/aw/aw.sock")),
    ] {
        let mut value = document();
        value["spec"]["daemon"]["endpoint"] = json!(endpoint);
        value["spec"]["daemon"]["state_dir"] = json!(state);
        let result = compile(
            &bytes(&value),
            "qoder",
            capabilities(),
            Availability::Required,
        );
        if let Some(expected) = expected {
            assert_eq!(result.unwrap().endpoint(), expected);
        } else {
            assert!(result.is_err(), "{endpoint:?}, {state:?}");
        }
    }
}

#[test]
fn socket_path_capacity_is_measured_in_bytes_including_the_terminator() {
    for (state, accepted) in [
        (format!("/{}", "x".repeat(98)), true),
        (format!("/{}", "x".repeat(99)), false),
        (format!("/{}", "é".repeat(49)), true),
        (format!("/{}", "é".repeat(50)), false),
    ] {
        for (state, endpoint) in [
            (state.clone(), "auto".to_owned()),
            ("auto".to_owned(), format!("{state}/aw.sock")),
        ] {
            let mut value = document();
            value["spec"]["daemon"]["state_dir"] = json!(state);
            value["spec"]["daemon"]["endpoint"] = json!(endpoint);
            let result = compile(
                &bytes(&value),
                "qoder",
                capabilities(),
                Availability::Required,
            );
            if accepted {
                assert_eq!(result.unwrap().endpoint().len(), 107);
            } else {
                assert!(matches!(result, Err(DescriptorError::Endpoint)));
            }
        }
    }
}

#[test]
fn deployment_verification_rejects_changed_well_shaped_hints() {
    let input = bytes(&document());
    let descriptor = compile(&input, "qoder", capabilities(), Availability::Required).unwrap();
    for (field, change) in [
        ("endpoint", json!("/other.sock")),
        ("wait_for_input_submit", json!(true)),
    ] {
        let mut value = serde_json::to_value(&descriptor).unwrap();
        value[field] = change;
        let changed = DeploymentDescriptor::parse(&bytes(&value)).unwrap();
        assert!(matches!(
            verify(
                &changed,
                &input,
                "qoder",
                capabilities(),
                Availability::Required
            ),
            Err(DescriptorError::Mismatch)
        ));
    }
}

#[test]
fn session_start_hint_is_controlled_by_compile_options() {
    let mut value = document();
    value["spec"]["events"]["session.start"] = json!({"enabled":true,"required":false,"steps":[]});
    let input = bytes(&value);
    let mut caps = capabilities();
    caps.events
        .insert("session.start".into(), vec!["observe".into()]);

    let without_hint =
        compile(&input, "qoder", caps.clone(), Availability::NativeFallback).unwrap();
    assert!(!without_hint.observe_session_start());

    let hinted_options = CompileOptions {
        session_start_hint: true,
    };
    let with_hint = compile_with_options(
        &input,
        "qoder",
        caps.clone(),
        Availability::NativeFallback,
        hinted_options,
    )
    .unwrap();
    assert!(with_hint.observe_session_start());

    verify_with_options(
        &with_hint,
        &input,
        "qoder",
        caps.clone(),
        Availability::NativeFallback,
        hinted_options,
    )
    .unwrap();
    assert!(matches!(
        verify(
            &with_hint,
            &input,
            "qoder",
            caps,
            Availability::NativeFallback
        ),
        Err(DescriptorError::Mismatch)
    ));
}

#[test]
fn plaintext_http_grants_bind_both_the_hint_and_configuration_bytes() {
    let mut value = document();
    let grants = json!([
        "Model.Example.",
        "model.example",
        "Model.Example.:8080",
        "model.example:8080"
    ]);
    let agent = json!({"adapter":"claude-sdk","argv":["python","worker.py"],
        "claude_sdk":{"allow_plaintext_http_hosts":grants}});
    value["spec"]["agents"]["sdk"] = agent;
    let mut caps = capabilities();
    caps.adapter = "claude-sdk".into();
    let input = bytes(&value);
    let descriptor = compile(&input, "sdk", caps.clone(), Availability::Required).unwrap();
    let mut hints = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(hints["allow_plaintext_http_hosts"], grants);
    verify(
        &descriptor,
        &input,
        "sdk",
        caps.clone(),
        Availability::Required,
    )
    .unwrap();
    for (index, changed) in [
        "Model.Example",
        "model.example.",
        "Model.Example:8080",
        "model.example.:8080",
    ]
    .into_iter()
    .enumerate()
    {
        hints["allow_plaintext_http_hosts"] = grants.clone();
        hints["allow_plaintext_http_hosts"][index] = json!(changed);
        let edited = DeploymentDescriptor::parse(&bytes(&hints)).unwrap();
        assert!(matches!(
            verify(&edited, &input, "sdk", caps.clone(), Availability::Required),
            Err(DescriptorError::Mismatch)
        ));
    }
    value["spec"]["agents"]["sdk"]["claude_sdk"]["allow_plaintext_http_hosts"] = json!([]);
    assert!(matches!(
        verify(
            &descriptor,
            &bytes(&value),
            "sdk",
            caps,
            Availability::Required
        ),
        Err(DescriptorError::Mismatch)
    ));
}
