//! Core admission binds the shared Provider rules to one configuration snapshot.
//!
//! Fixtures follow `aw-provider/tests/admission.rs`. Every rule case runs the
//! Core facade and the shared Provider functions on the same bytes and requires
//! the same steps or rejection reason. Capability evidence is synthetic.

use aw_config::Validator;
use aw_core::admission::{AdmissionEngine, AdmissionError, AdmittedPolicy};
use aw_provider::admission::{
    admit, preflight, AdapterCapabilities, ProviderEvidence, StepExecution,
};
use aw_provider::{Error, Protocol, Reply, VERSION};
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

/// Admits through Core and requires the shared Provider rules to agree at each stage.
fn admit_both(
    document: &Value,
    target: &str,
    capabilities: &AdapterCapabilities,
    providers: &BTreeMap<String, ProviderEvidence>,
) -> Result<AdmittedPolicy, &'static str> {
    let raw = bytes(document);
    let config = CONFIGURATION.parse(&raw).unwrap();
    let expected = preflight(&config, target, capabilities).map_err(reason);
    let candidate = ENGINE
        .preflight(&raw, target, capabilities.clone())
        .map_err(core_reason);
    match (&candidate, &expected) {
        (Ok(candidate), Ok(steps)) => assert_eq!(candidate.requirements(), steps.as_slice()),
        (Err(actual), Err(expected)) => assert_eq!(actual, expected),
        _ => panic!("Core preflight diverged from Provider preflight"),
    }
    let expected = admit(&config, target, capabilities, providers).map_err(reason);
    let policy =
        candidate.and_then(|candidate| ENGINE.admit(candidate, providers).map_err(core_reason));
    match (&policy, &expected) {
        (Ok(policy), Ok(steps)) => {
            assert_eq!(policy.steps(), steps.as_slice());
            assert_eq!(policy.providers().len(), providers.len());
        }
        (Err(actual), Err(expected)) => assert_eq!(actual, expected),
        _ => panic!("Core admission diverged from Provider admission"),
    }
    policy
}

fn reason(error: Error) -> &'static str {
    match error {
        Error::Invalid(reason) => reason,
        other => panic!("unexpected Provider error: {other}"),
    }
}

fn core_reason(error: AdmissionError) -> &'static str {
    match error {
        AdmissionError::Provider(error) => reason(error),
        AdmissionError::Configuration(error) => panic!("unexpected configuration error: {error}"),
    }
}

#[test]
fn one_policy_admits_every_adapter_with_the_same_steps() {
    let doc = document();
    let providers = providers(&doc);
    for adapter in ADAPTERS {
        let policy = admit_both(&doc, adapter, &capabilities(adapter), &providers).unwrap();
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
            admit_both(&doc, target, &adapter, &providers).unwrap_err(),
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
        ("tool.before", "replace_input"),
        ("tool.after", "block"),
        ("permission.request", "observe"),
    ] {
        let mut adapter = capabilities("qoder");
        adapter.events.insert(event.into(), vec![effect.into()]);
        assert_eq!(
            admit_both(&doc, "qoder", &adapter, &providers).unwrap_err(),
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
            admit_both(&doc, "qoder", &adapter, &providers).unwrap_err(),
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
            None => doc["spec"]["events"][event] = value,
            Some(field) => doc["spec"]["events"][event]["steps"][0][field] = value,
        }
        assert_eq!(
            admit_both(&doc, "qoder", &capabilities("qoder"), &providers(&doc)).unwrap_err(),
            expected
        );
    }

    // An optional event without steps still needs Adapter support.
    let mut doc = document();
    doc["spec"]["events"]["tool.after"]["steps"] = json!([]);
    doc["spec"]["events"]["tool.after"]["required"] = json!(false);
    let mut adapter = capabilities("qoder");
    adapter.events.remove("tool.after");
    assert_eq!(
        admit_both(&doc, "qoder", &adapter, &providers(&doc)).unwrap_err(),
        "Adapter evidence lacks an enabled event"
    );

    // Withholding is admitted only with event-scoped Adapter evidence.
    let mut doc = document();
    doc["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!("withhold_result");
    let mut adapter = capabilities("agentscope");
    adapter
        .failure_actions
        .insert("tool.after".into(), vec!["withhold_result".into()]);
    let policy = admit_both(&doc, "agentscope", &adapter, &providers(&doc)).unwrap();
    assert!(policy
        .steps()
        .iter()
        .any(|step| step.on_error == "withhold_result"));

    let mut doc = document();
    doc["spec"]["events"]["tool.before"]["guard"] = json!("security.violation");
    doc["spec"]["events"]["security.violation"] = json!({"enabled": true, "steps": []});
    assert!(admit_both(&doc, "qoder", &capabilities("qoder"), &providers(&doc)).is_err());
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
        admit_both(&doc, "qoder", &adapter, &providers).unwrap_err(),
        "requested effect is not supported by the implementation and Adapter"
    );
    adapter
        .events
        .get_mut("tool.after")
        .unwrap()
        .push("replace_result".into());
    let policy = admit_both(&doc, "qoder", &adapter, &providers).unwrap();
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
        admit_both(&doc, "qoder", &adapter, &providers).unwrap_err(),
        "result replacement must be the last applicable enabled event step"
    );
    // Disabled or projected-out later steps are not applicable.
    doc["spec"]["events"]["tool.after"]["steps"][1]["enabled"] = json!(false);
    assert!(admit_both(&doc, "qoder", &adapter, &providers).is_ok());
    doc["spec"]["events"]["tool.after"]["steps"][1]["enabled"] = json!(true);
    doc["spec"]["events"]["tool.after"]["steps"][1]["match"] =
        json!({"tools": ["native:openclaw:custom_tool"]});
    assert!(admit_both(&doc, "qoder", &adapter, &providers).is_ok());
}

#[test]
fn selectors_skip_steps_that_cannot_reach_the_target_adapter() {
    let mut doc = document();
    let providers = providers(&doc);
    doc["spec"]["events"]["tool.before"]["steps"][0]["match"] =
        json!({"tools": ["native:openclaw:custom_tool"]});
    let qoder = admit_both(&doc, "qoder", &capabilities("qoder"), &providers).unwrap();
    assert_eq!(qoder.steps().len(), 1);
    assert_eq!(qoder.steps()[0].event, "tool.after");
    let openclaw = admit_both(&doc, "openclaw", &capabilities("openclaw"), &providers).unwrap();
    assert_eq!(openclaw.steps().len(), 2);

    // A skipped event needs no Adapter support for that event.
    doc["spec"]["events"]["tool.after"]["match"] =
        json!({"tools": ["native:openclaw:custom_tool"]});
    let mut adapter = capabilities("qoder");
    adapter.events.remove("tool.after");
    assert!(admit_both(&doc, "qoder", &adapter, &providers)
        .unwrap()
        .steps()
        .is_empty());

    for tool in ["*", "bash", "native:qoder:custom_tool"] {
        let mut doc = document();
        doc["spec"]["events"]["tool.before"]["match"] = json!({"tools": [tool]});
        let policy = admit_both(&doc, "qoder", &capabilities("qoder"), &providers).unwrap();
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
        admit_both(&doc, "qoder", &capabilities("qoder"), &BTreeMap::new())
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
    let policy = admit_both(&doc, "qoder", &capabilities("qoder"), &providers(&doc)).unwrap();
    let ids: Vec<_> = policy
        .steps()
        .iter()
        .filter(|step| step.event == "tool.before")
        .map(|step| step.step_id.as_str())
        .collect();
    assert_eq!(ids, ["check", "check-second"]);
}

#[test]
fn structured_steps_require_matching_provider_and_private_config_evidence() {
    let doc = document();
    let adapter = capabilities("qoder");
    assert_eq!(
        admit_both(&doc, "qoder", &adapter, &BTreeMap::new()).unwrap_err(),
        "enabled step lacks Provider evidence"
    );
    let mut renamed = providers(&doc);
    let policy = renamed.remove("policy").unwrap();
    renamed.insert("other".into(), policy);
    assert_eq!(
        admit_both(&doc, "qoder", &adapter, &renamed).unwrap_err(),
        "enabled step lacks Provider evidence"
    );
    let private_config = &doc["spec"]["providers"]["policy"]["config"];
    let missing_op = "Provider does not declare the requested operation";
    let missing_event = "Provider operation does not support the requested event";
    let missing_effect = "requested effect is not supported by the Provider";
    for (field, replacement, expected) in [
        ("name", json!("different-operation"), missing_op),
        ("events", json!(["tool.after"]), missing_event),
        ("effects", json!(["observe"]), missing_effect),
    ] {
        let mut operations = operations();
        operations[0][field] = replacement;
        let providers = evidence(private_config, operations);
        assert_eq!(
            admit_both(&doc, "qoder", &adapter, &providers).unwrap_err(),
            expected
        );
    }

    let wrong = evidence(
        &json!({"secret": "must-not-appear-in-errors"}),
        operations(),
    );
    assert_eq!(
        admit_both(&doc, "qoder", &adapter, &wrong).unwrap_err(),
        "Provider validation evidence does not match its private configuration"
    );
    let candidate = ENGINE.preflight(&bytes(&doc), "qoder", adapter).unwrap();
    let error = ENGINE.admit(candidate, &wrong).unwrap_err().to_string();
    assert!(error.contains("private configuration"));
    assert!(!error.contains("must-not-appear-in-errors") && !error.contains("threshold"));
}

#[test]
fn boolean_and_native_hooks_need_no_provider_description() {
    for explicit in [false, true] {
        let mut doc = document();
        let provider = doc["spec"]["providers"]["policy"].as_object_mut().unwrap();
        provider.remove("protocol");
        provider.remove("config");
        if explicit {
            provider.insert("protocol".into(), json!(aw_config::HOOK_PROTOCOL));
        }
        for event in ["tool.before", "tool.after"] {
            let step = doc["spec"]["events"][event]["steps"][0]
                .as_object_mut()
                .unwrap();
            step.remove("operation");
            if explicit {
                step.insert("operation".into(), json!("invoke"));
            }
        }
        for adapter in ADAPTERS {
            let policy =
                admit_both(&doc, adapter, &capabilities(adapter), &BTreeMap::new()).unwrap();
            assert_eq!(policy.steps().len(), 2);
            assert!(policy
                .steps()
                .iter()
                .all(|step| matches!(step.execution, StepExecution::Hook { .. })));
        }
    }

    let mut doc = document();
    doc["spec"]["providers"]["policy"]["protocol"] = json!("native-hook/v1alpha1");
    doc["spec"]["providers"]["policy"]["config"] = json!({});
    for event in ["tool.before", "tool.after"] {
        doc["spec"]["events"][event]["steps"] = json!([
            {"id": "raw", "provider": "policy", "native": {}, "on_error": "report"}
        ]);
    }
    for adapter in ADAPTERS {
        let policy = admit_both(&doc, adapter, &capabilities(adapter), &BTreeMap::new()).unwrap();
        assert_eq!(policy.steps().len(), 2);
        assert!(policy
            .steps()
            .iter()
            .all(|step| step.execution == StepExecution::Native));
    }
    let mut unsupported = capabilities("qoder");
    unsupported.events.remove("tool.after");
    assert_eq!(
        admit_both(&doc, "qoder", &unsupported, &BTreeMap::new()).unwrap_err(),
        "Adapter evidence lacks an enabled event"
    );
}

#[test]
fn admission_never_starts_configured_programs() {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aw-core-admission-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let program = directory.join("probe");
    let marker = directory.join("probe.ran");
    fs::write(&program, "#!/bin/sh\n: > \"$0.ran\"\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let mut doc = document();
    let argv = json!([program.to_str().unwrap()]);
    for adapter in ADAPTERS {
        doc["spec"]["agents"][adapter]["argv"] = argv.clone();
    }
    doc["spec"]["providers"]["policy"]["transport"]["argv"] = argv;
    let providers = providers(&doc);
    for adapter in ADAPTERS {
        assert!(admit_both(&doc, adapter, &capabilities(adapter), &providers).is_ok());
        assert!(admit_both(&doc, adapter, &capabilities(adapter), &BTreeMap::new()).is_err());
    }
    assert!(!marker.exists(), "admission started a configured program");
    assert!(Command::new(&program).status().unwrap().success());
    assert!(marker.exists());
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn revision_is_the_digest_of_the_exact_input_bytes_and_snapshot_remains_bound() {
    let doc = document();
    let compact = bytes(&doc);
    let check_qoder = |raw: &[u8]| {
        ENGINE
            .preflight(raw, "qoder", capabilities("qoder"))
            .unwrap()
    };
    let first = check_qoder(&compact);
    let again = check_qoder(&compact);
    assert_eq!(first.revision(), again.revision());
    assert_eq!(first.revision(), digest(&compact));

    // Semantically equal documents with different bytes keep distinct revisions.
    let pretty = serde_json::to_vec_pretty(&doc).unwrap();
    let reformatted = check_qoder(&pretty);
    assert_eq!(
        reformatted.configuration().as_value(),
        first.configuration().as_value()
    );
    assert_eq!(reformatted.requirements(), first.requirements());
    assert_ne!(reformatted.revision(), first.revision());
    assert_eq!(reformatted.revision(), digest(&pretty));

    let yaml = include_bytes!("../../aw-config/examples/aw.minimal.yaml");
    let json = bytes(CONFIGURATION.parse(yaml).unwrap().as_value());
    let from_yaml = check_qoder(yaml);
    let from_json = check_qoder(&json);
    assert_eq!(
        from_yaml.configuration().as_value(),
        from_json.configuration().as_value()
    );
    assert_eq!(from_yaml.revision(), digest(yaml));
    assert_ne!(from_yaml.revision(), from_json.revision());

    let mut adapter = capabilities("agentscope");
    adapter
        .failure_actions
        .insert("tool.after".into(), vec!["withhold_result".into()]);
    let candidate = ENGINE
        .preflight(&compact, "agentscope", adapter.clone())
        .unwrap();
    let revision = candidate.revision().to_owned();
    let configuration = candidate.configuration().as_value().clone();
    let requirements = candidate.requirements().to_vec();
    let policy = ENGINE.admit(candidate, &providers(&doc)).unwrap();
    assert_eq!(policy.revision(), revision);
    assert_eq!(policy.target(), "agentscope");
    assert_eq!(policy.configuration().as_value(), &configuration);
    assert_eq!(policy.steps(), requirements.as_slice());
    let bound = policy.capabilities();
    assert_eq!(
        (&bound.adapter, &bound.version, &bound.entrypoint),
        (&adapter.adapter, &adapter.version, &adapter.entrypoint)
    );
    assert_eq!(bound.events, adapter.events);
    assert_eq!(bound.failure_actions, adapter.failure_actions);
    let debug = format!("{policy:?}");
    assert!(debug.contains(&revision) && !debug.contains("threshold"));
}

#[test]
fn invalid_documents_fail_before_capability_rules() {
    for input in [
        b"{".as_slice(),
        b"{}",
        b"a: 1\na: 2\n",
        b"apiVersion: aw/v1alpha1\n---\nkind: AWConfiguration\n",
    ] {
        assert!(matches!(
            ENGINE.preflight(input, "qoder", capabilities("qoder")),
            Err(AdmissionError::Configuration(_))
        ));
    }
    let mut doc = document();
    doc["spec"]["events"]["tool.before"]["steps"][0]["provider"] = json!("missing");
    assert!(matches!(
        ENGINE.preflight(&bytes(&doc), "qoder", capabilities("qoder")),
        Err(AdmissionError::Configuration(_))
    ));
}
