//! Core admission owns the Hook policy capability rules and binds them to one configuration snapshot.

use aw_config::Validator;
use aw_core::admission::{
    admit, preflight, AdapterCapabilities, AdmissionEngine, AdmissionError, AdmittedPolicy,
    ProviderEvidence, StepExecution,
};
use aw_provider::{Protocol, Reply, VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, process::Command,
    sync::LazyLock,
};

static ENGINE: LazyLock<AdmissionEngine> = LazyLock::new(|| AdmissionEngine::new().unwrap());
static CONFIGURATION: LazyLock<Validator> = LazyLock::new(|| Validator::new().unwrap());
static PROTOCOL: LazyLock<Protocol> = LazyLock::new(|| Protocol::new().unwrap());

const ADAPTERS: [&str; 7] = [
    "qoder",
    "openclaw",
    "hermes",
    "qwenpaw",
    "pi",
    "claude-sdk",
    "agentscope",
];

fn document() -> Value {
    let mut doc = CONFIGURATION
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    for adapter in ADAPTERS {
        doc["spec"]["agents"][adapter] = json!({
            "adapter": adapter,
            "argv": ["/does-not-exist/agent", "--fixture"]
        });
    }
    doc["spec"]["providers"]["policy"] = json!({
        "protocol": VERSION,
        "transport": {"type": "stdio", "argv": ["/does-not-exist/policy"], "location": "agent"},
        "timeout_ms": 250,
        "max_output_bytes": 4096,
        "config": {"threshold": 0.75, "标签": "example"}
    });
    for (event, operation, effects, on_error) in [
        ("tool.before", "check", json!(["observe", "block"]), "block"),
        ("tool.after", "record", json!(["observe"]), "report"),
    ] {
        doc["spec"]["events"][event]["steps"] = json!([{
            "id": operation, "provider": "policy", "operation": operation,
            "effects": effects, "on_error": on_error
        }]);
    }
    doc
}

fn bytes(document: &Value) -> Vec<u8> {
    serde_json::to_vec(document).unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn capabilities(adapter: &str) -> AdapterCapabilities {
    AdapterCapabilities {
        failure_actions: Default::default(),
        adapter: adapter.into(),
        version: "fixture-1".into(),
        entrypoint: "fixture-tool-hooks".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
    }
}

fn operations() -> Value {
    json!([
        {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe"]}
    ])
}

fn exchange(req: Value, res: Value) -> Reply {
    let req = PROTOCOL.parse_request(&bytes(&req)).unwrap();
    PROTOCOL.check_response(&req, &bytes(&res)).unwrap()
}

fn evidence(config: &Value, operations: Value) -> BTreeMap<String, ProviderEvidence> {
    let Reply::Description(description) = exchange(
        json!({"api_version": VERSION, "method": "describe", "request_id": "describe-fixture"}),
        json!({"api_version": VERSION, "request_id": "describe-fixture", "status": "ok", "operations": operations}),
    ) else {
        panic!("expected description");
    };
    let Reply::Configuration(configuration) = exchange(
        json!({"api_version": VERSION, "method": "validate_config", "request_id": "validate-fixture", "config": config}),
        json!({"api_version": VERSION, "request_id": "validate-fixture", "status": "ok"}),
    ) else {
        panic!("expected configuration validation");
    };
    BTreeMap::from([(
        "policy".into(),
        ProviderEvidence {
            description,
            configuration,
        },
    )])
}

fn providers(document: &Value) -> BTreeMap<String, ProviderEvidence> {
    evidence(
        &document["spec"]["providers"]["policy"]["config"],
        operations(),
    )
}

/// Admits through [`AdmissionEngine`] and verifies the parsed-configuration entrypoints match.
fn admit_policy(
    document: &Value,
    target: &str,
    capabilities: &AdapterCapabilities,
    providers: &BTreeMap<String, ProviderEvidence>,
) -> Result<AdmittedPolicy, &'static str> {
    let raw = bytes(document);
    let config = CONFIGURATION.parse(&raw).unwrap();
    let direct_preflight = preflight(&config, target, capabilities).map_err(reason);
    let candidate = ENGINE
        .preflight(&raw, target, capabilities.clone())
        .map_err(reason);
    match (&candidate, &direct_preflight) {
        (Ok(candidate), Ok(steps)) => assert_eq!(candidate.requirements(), steps.as_slice()),
        (Err(actual), Err(expected)) => assert_eq!(actual, expected),
        _ => panic!("AdmissionEngine::preflight diverged from preflight"),
    }
    let direct_admit = admit(&config, target, capabilities, providers).map_err(reason);
    let policy = candidate.and_then(|candidate| ENGINE.admit(candidate, providers).map_err(reason));
    match (&policy, &direct_admit) {
        (Ok(policy), Ok(steps)) => {
            assert_eq!(policy.steps(), steps.as_slice());
            assert_eq!(policy.providers().len(), providers.len());
        }
        (Err(actual), Err(expected)) => assert_eq!(actual, expected),
        _ => panic!("AdmissionEngine::admit diverged from admit"),
    }
    policy
}

fn reason(error: AdmissionError) -> &'static str {
    match error {
        AdmissionError::Rejected(reason) => reason,
        AdmissionError::Configuration(error) => panic!("unexpected configuration error: {error}"),
    }
}

#[test]
fn one_policy_admits_every_adapter_with_the_same_steps() {
    let doc = document();
    let providers = providers(&doc);
    for adapter in ADAPTERS {
        let policy = admit_policy(&doc, adapter, &capabilities(adapter), &providers).unwrap();
        assert_eq!(policy.target(), adapter);
        assert_eq!(policy.capabilities().adapter, adapter);
        assert_eq!(policy.steps().len(), 2);
        let before = policy
            .steps()
            .iter()
            .find(|step| step.event == "tool.before")
            .unwrap();
        assert_eq!(
            (before.step_id.as_str(), before.provider.as_str()),
            ("check", "policy")
        );
        assert_eq!(before.on_error, "block");
        assert_eq!(
            before.execution,
            StepExecution::Provider {
                operation: "check".into(),
                effects: vec!["observe".into(), "block".into()],
            }
        );
        let after = policy
            .steps()
            .iter()
            .find(|step| step.event == "tool.after")
            .unwrap();
        assert_eq!(after.on_error, "report");
        assert_eq!(
            after.execution,
            StepExecution::Provider {
                operation: "record".into(),
                effects: vec!["observe".into()],
            }
        );
    }
}

#[test]
fn target_and_adapter_evidence_must_agree() {
    let doc = document();
    let providers = providers(&doc);
    let mut blank_version = capabilities("qoder");
    blank_version.version = " ".into();
    let mut blank_entrypoint = capabilities("qoder");
    blank_entrypoint.entrypoint = String::new();
    let unknown = "unknown configured Agent target";
    let mismatch = "Adapter evidence does not match the configured target";
    let missing = "Adapter evidence requires a version and entrypoint";
    for (target, adapter, expected) in [
        ("missing", capabilities("qoder"), unknown),
        ("qoder", capabilities("openclaw"), mismatch),
        ("qoder", capabilities("unknown-adapter"), mismatch),
        ("qoder", blank_version, missing),
        ("qoder", blank_entrypoint, missing),
    ] {
        assert_eq!(
            admit_policy(&doc, target, &adapter, &providers).unwrap_err(),
            expected
        );
    }
}

#[test]
fn adapter_evidence_cannot_expand_the_implemented_surface() {
    let doc = document();
    let providers = providers(&doc);
    for (event, effect) in [
        ("tool.before", "ask"),
        ("tool.after", "replace_input"),
        ("tool.after", "block"),
        ("permission.request", "observe"),
    ] {
        let mut adapter = capabilities("qoder");
        adapter.events.insert(event.into(), vec![effect.into()]);
        assert_eq!(
            admit_policy(&doc, "qoder", &adapter, &providers).unwrap_err(),
            "Adapter evidence exceeds the supported event or effect surface"
        );
    }
    for (event, action) in [
        ("tool.before", "withhold_result"),
        ("tool.after", "block"),
        ("unknown", "withhold_result"),
    ] {
        let mut adapter = capabilities("qoder");
        adapter.failure_actions = BTreeMap::from([(event.into(), vec![action.into()])]);
        assert_eq!(
            admit_policy(&doc, "qoder", &adapter, &providers).unwrap_err(),
            "Adapter evidence exceeds the supported failure action surface"
        );
    }
}

#[test]
fn unsupported_events_effects_and_failure_actions_are_rejected() {
    for (event, field, value, expected) in [
        (
            "permission.request",
            None,
            json!({"enabled": true, "required": false, "steps": []}),
            "enabled event is not supported",
        ),
        (
            "permission.request",
            None,
            json!({"enabled": true, "required": true, "steps": []}),
            "enabled event is not supported",
        ),
        (
            "tool.before",
            Some("effects"),
            json!(["replace_input"]),
            "requested effect is not supported by the implementation and Adapter",
        ),
        (
            "tool.after",
            Some("on_error"),
            json!("withhold_result"),
            "failure action is not supported by the implementation and Adapter",
        ),
    ] {
        let mut doc = document();
        match field {
            None => doc["spec"]["events"][event] = value.clone(),
            Some(field) => doc["spec"]["events"][event]["steps"][0][field] = value.clone(),
        }
        assert_eq!(
            admit_policy(&doc, "qoder", &capabilities("qoder"), &providers(&doc)).unwrap_err(),
            expected
        );
        if let Some(field) = field {
            let mut doc = document();
            let mut later = doc["spec"]["events"][event]["steps"][0].clone();
            later["id"] = json!("unsupported-later-step");
            later[field] = value;
            doc["spec"]["events"][event]["steps"]
                .as_array_mut()
                .unwrap()
                .push(later);
            assert_eq!(
                admit_policy(&doc, "qoder", &capabilities("qoder"), &BTreeMap::new()).unwrap_err(),
                expected
            );
        }
    }

    // An optional event without steps still needs Adapter support.
    let mut doc = document();
    doc["spec"]["events"]["tool.after"]["steps"] = json!([]);
    doc["spec"]["events"]["tool.after"]["required"] = json!(false);
    let mut adapter = capabilities("qoder");
    adapter.events.remove("tool.after");
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &providers(&doc)).unwrap_err(),
        "Adapter evidence lacks an enabled event"
    );

    // Failure blocking requires Adapter block support even for an observe-only step.
    let mut doc = document();
    doc["spec"]["events"]["tool.before"]["steps"][0]["effects"] = json!(["observe"]);
    let mut ops = operations();
    ops[0]["effects"] = json!(["observe"]);
    let obs_providers = evidence(&doc["spec"]["providers"]["policy"]["config"], ops);
    let mut adapter = capabilities("qoder");
    adapter
        .events
        .insert("tool.before".into(), vec!["observe".into()]);
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &obs_providers).unwrap_err(),
        "failure action is not supported by the implementation and Adapter"
    );
    doc["spec"]["events"]["tool.before"]["steps"][0]["on_error"] = json!("report");
    assert!(admit_policy(&doc, "qoder", &adapter, &obs_providers).is_ok());

    // Withholding is admitted only with event-scoped Adapter evidence.
    let mut doc = document();
    doc["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!("withhold_result");
    for adapter in ["qoder", "openclaw", "hermes", "qwenpaw", "pi", "claude-sdk"] {
        assert!(admit_policy(&doc, adapter, &capabilities(adapter), &providers(&doc)).is_err());
    }
    for target in ["agentscope", "qwenpaw"] {
        let mut adapter = capabilities(target);
        adapter
            .failure_actions
            .insert("tool.after".into(), vec!["withhold_result".into()]);
        let policy = admit_policy(&doc, target, &adapter, &providers(&doc)).unwrap();
        assert!(policy
            .steps()
            .iter()
            .any(|step| step.on_error == "withhold_result"));
    }

    let mut doc = document();
    doc["spec"]["events"]["tool.before"]["guard"] = json!("security.violation");
    doc["spec"]["events"]["security.violation"] = json!({"enabled": true, "steps": []});
    assert!(admit_policy(&doc, "qoder", &capabilities("qoder"), &providers(&doc)).is_err());
}

#[test]
fn result_replacement_must_be_the_last_applicable_step() {
    let mut doc = document();
    doc["spec"]["events"]["tool.after"]["steps"][0]["effects"] = json!(["replace_result"]);
    let mut adapter = capabilities("qoder");
    let mut operations = operations();
    operations[1]["effects"] = json!(["observe", "replace_result"]);
    let providers = evidence(&doc["spec"]["providers"]["policy"]["config"], operations);
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &providers).unwrap_err(),
        "requested effect is not supported by the implementation and Adapter"
    );
    adapter
        .events
        .get_mut("tool.after")
        .unwrap()
        .push("replace_result".into());
    let policy = admit_policy(&doc, "qoder", &adapter, &providers).unwrap();
    assert!(policy
        .steps()
        .iter()
        .any(|step| step.execution.replaces_result()));

    let mut later = doc["spec"]["events"]["tool.after"]["steps"][0].clone();
    later["id"] = json!("later");
    later["effects"] = json!(["observe"]);
    doc["spec"]["events"]["tool.after"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(later);
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &providers).unwrap_err(),
        "result replacement must be the last applicable enabled event step"
    );
    // Disabled or projected-out later steps are not applicable.
    doc["spec"]["events"]["tool.after"]["steps"][1]["enabled"] = json!(false);
    assert!(admit_policy(&doc, "qoder", &adapter, &providers).is_ok());
    doc["spec"]["events"]["tool.after"]["steps"][1]["enabled"] = json!(true);
    doc["spec"]["events"]["tool.after"]["steps"][1]["match"] =
        json!({"tools": ["native:openclaw:custom_tool"]});
    assert!(admit_policy(&doc, "qoder", &adapter, &providers).is_ok());
}

#[test]
fn selectors_skip_steps_that_cannot_reach_the_target_adapter() {
    let mut doc = document();
    let providers = providers(&doc);
    doc["spec"]["events"]["tool.before"]["steps"][0]["match"] =
        json!({"tools": ["native:openclaw:custom_tool"]});
    let qoder = admit_policy(&doc, "qoder", &capabilities("qoder"), &providers).unwrap();
    assert_eq!(qoder.steps().len(), 1);
    assert_eq!(qoder.steps()[0].event, "tool.after");
    let openclaw = admit_policy(&doc, "openclaw", &capabilities("openclaw"), &providers).unwrap();
    assert_eq!(openclaw.steps().len(), 2);

    // A skipped event needs no Adapter support for that event.
    doc["spec"]["events"]["tool.after"]["match"] =
        json!({"tools": ["native:openclaw:custom_tool"]});
    let mut adapter = capabilities("qoder");
    adapter.events.remove("tool.after");
    assert!(admit_policy(&doc, "qoder", &adapter, &providers)
        .unwrap()
        .steps()
        .is_empty());

    for tool in ["*", "bash", "native:qoder:custom_tool"] {
        let mut doc = document();
        doc["spec"]["events"]["tool.before"]["match"] = json!({"tools": [tool]});
        let policy = admit_policy(&doc, "qoder", &capabilities("qoder"), &providers).unwrap();
        assert_eq!(policy.steps().len(), 2);
    }
}

#[test]
fn disabled_events_and_steps_need_no_provider_evidence_and_preserve_order() {
    let mut doc = document();
    doc["spec"]["events"]["tool.before"]["steps"][0]["enabled"] = json!(false);
    doc["spec"]["events"]["tool.after"]["enabled"] = json!(false);
    doc["spec"]["events"]["tool.after"]["required"] = json!(false);
    doc["spec"]["events"]["permission.request"] = json!({
        "enabled": false, "steps": [{
            "id": "later", "provider": "policy", "operation": "not-discovered",
            "effects": ["observe"], "on_error": "report"
        }]
    });
    assert!(
        admit_policy(&doc, "qoder", &capabilities("qoder"), &BTreeMap::new())
            .unwrap()
            .steps()
            .is_empty()
    );

    let mut doc = document();
    let mut second = doc["spec"]["events"]["tool.before"]["steps"][0].clone();
    second["id"] = json!("check-second");
    doc["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let policy = admit_policy(&doc, "qoder", &capabilities("qoder"), &providers(&doc)).unwrap();
    let ids: Vec<_> = policy
        .steps()
        .iter()
        .filter(|step| step.event == "tool.before")
        .map(|step| step.step_id.as_str())
        .collect();
    assert_eq!(ids, ["check", "check-second"]);
}

#[path = "admission/engine.rs"]
mod engine;

#[test]
fn input_replacement_requires_one_initial_fail_closed_provider_step() {
    let mut doc = document();
    let mut caps = capabilities("qoder");
    caps.events
        .get_mut("tool.before")
        .unwrap()
        .push("replace_input".into());
    let rewrite = json!({"id":"rewrite","provider":"policy","operation":"rewrite","effects":["replace_input"],"on_error":"block"});
    let scan = doc["spec"]["events"]["tool.before"]["steps"][0].clone();
    doc["spec"]["events"]["tool.before"]["steps"] = json!([rewrite, scan]);
    let config = CONFIGURATION.parse(&bytes(&doc)).unwrap();
    assert!(preflight(&config, "qoder", &caps).is_ok());
    let mut report = scan.clone();
    report["on_error"] = json!("report");
    let mut mixed = rewrite.clone();
    mixed["effects"] = json!(["replace_input", "block"]);
    let mut second = rewrite.clone();
    second["id"] = json!("rewrite-two");
    for steps in [
        json!([scan, rewrite]),
        json!([rewrite, second]),
        json!([rewrite, report]),
        json!([mixed]),
    ] {
        doc["spec"]["events"]["tool.before"]["steps"] = steps;
        let config = CONFIGURATION.parse(&bytes(&doc)).unwrap();
        assert!(preflight(&config, "qoder", &caps).is_err());
    }
}
