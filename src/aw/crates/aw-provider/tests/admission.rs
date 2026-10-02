use aw_config::{Configuration, Validator};
use aw_provider::admission::{admit, AdapterCapabilities, ProviderEvidence};
use aw_provider::{Protocol, Reply, VERSION};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::LazyLock;

static CONFIGURATION: LazyLock<Validator> = LazyLock::new(|| Validator::new().unwrap());
static PROTOCOL: LazyLock<Protocol> = LazyLock::new(|| Protocol::new().unwrap());

fn document() -> Value {
    let mut document = CONFIGURATION
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    for adapter in ["qoder", "openclaw", "hermes", "qwenpaw"] {
        document["spec"]["agents"][adapter] = json!({
            "adapter": adapter,
            "argv": ["/does-not-exist/agent", "--fixture"]
        });
    }
    document["spec"]["providers"]["policy"] = json!({
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
        document["spec"]["events"][event]["steps"] = json!([{
            "id": operation, "provider": "policy", "operation": operation,
            "effects": effects, "on_error": on_error
        }]);
    }
    document
}

fn configuration(document: &Value) -> Configuration {
    CONFIGURATION
        .parse(&serde_json::to_vec(document).unwrap())
        .unwrap()
}

fn capabilities(adapter: &str) -> AdapterCapabilities {
    // Synthetic evidence exercises admission; this is not a real version matrix.
    AdapterCapabilities {
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

fn evidence(config: &Value, operations: Value) -> BTreeMap<String, ProviderEvidence> {
    let describe = PROTOCOL
        .parse_request(
            &serde_json::to_vec(&json!({
                "api_version": VERSION, "method": "describe", "request_id": "describe-fixture"
            }))
            .unwrap(),
        )
        .unwrap();
    let Reply::Description(description) = PROTOCOL
        .check_response(
            &describe,
            &serde_json::to_vec(&json!({
                "api_version": VERSION, "request_id": "describe-fixture",
                "status": "ok", "operations": operations
            }))
            .unwrap(),
        )
        .unwrap()
    else {
        panic!("expected description");
    };
    let validate = PROTOCOL
        .parse_request(
            &serde_json::to_vec(&json!({
                "api_version": VERSION, "method": "validate_config",
                "request_id": "validate-fixture", "config": config
            }))
            .unwrap(),
        )
        .unwrap();
    let Reply::Configuration(configuration) = PROTOCOL
        .check_response(
            &validate,
            &serde_json::to_vec(&json!({
                "api_version": VERSION, "request_id": "validate-fixture", "status": "ok"
            }))
            .unwrap(),
        )
        .unwrap()
    else {
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

#[test]
fn one_policy_admits_four_explicit_adapter_boundaries_without_running_programs() {
    let document = document();
    let config = configuration(&document);
    let providers = providers(&document);
    for adapter in ["qoder", "openclaw", "hermes", "qwenpaw"] {
        let steps = admit(&config, adapter, &capabilities(adapter), &providers).unwrap();
        assert_eq!(steps.len(), 2);
        let before = steps
            .iter()
            .find(|step| step.event == "tool.before")
            .unwrap();
        assert_eq!(before.step_id, "check");
        assert_eq!(before.provider, "policy");
        assert_eq!(before.operation, "check");
        assert_eq!(before.effects, ["observe", "block"]);
        assert_eq!(before.on_error, "block");
        let after = steps
            .iter()
            .find(|step| step.event == "tool.after")
            .unwrap();
        assert_eq!(after.operation, "record");
        assert_eq!(after.effects, ["observe"]);
        assert_eq!(after.on_error, "report");
    }
}

#[test]
fn target_and_trusted_adapter_evidence_must_agree() {
    let document = document();
    let config = configuration(&document);
    let providers = providers(&document);
    assert!(admit(&config, "missing", &capabilities("qoder"), &providers).is_err());
    assert!(admit(&config, "qoder", &capabilities("openclaw"), &providers).is_err());
    for (version, entrypoint) in [("", "fixture"), ("fixture", ""), (" ", "fixture")] {
        let mut adapter = capabilities("qoder");
        adapter.version = version.into();
        adapter.entrypoint = entrypoint.into();
        assert!(admit(&config, "qoder", &adapter, &providers).is_err());
    }
}

#[test]
fn caller_capabilities_cannot_expand_the_implemented_surface() {
    let document = document();
    let config = configuration(&document);
    let providers = providers(&document);
    for (event, effect) in [
        ("tool.before", "ask"),
        ("tool.before", "replace_input"),
        ("tool.after", "replace_result"),
        ("tool.after", "block"),
        ("permission.request", "observe"),
    ] {
        let mut adapter = capabilities("qoder");
        adapter.events.insert(event.into(), vec![effect.into()]);
        assert!(admit(&config, "qoder", &adapter, &providers).is_err());
    }
}

#[test]
fn every_enabled_event_requires_support_even_without_steps_or_required_flag() {
    for required in [false, true] {
        let mut document = document();
        document["spec"]["events"]["permission.request"] = json!({
            "enabled": true, "required": required, "steps": []
        });
        assert!(admit(
            &configuration(&document),
            "qoder",
            &capabilities("qoder"),
            &providers(&document)
        )
        .is_err());
    }
    let mut document = document();
    document["spec"]["events"]["tool.after"]["steps"] = json!([]);
    document["spec"]["events"]["tool.after"]["required"] = json!(false);
    let mut adapter = capabilities("qoder");
    adapter.events.remove("tool.after");
    assert!(admit(
        &configuration(&document),
        "qoder",
        &adapter,
        &providers(&document)
    )
    .is_err());
}

#[test]
fn disabled_events_and_steps_need_no_provider_discovery() {
    let mut document = document();
    document["spec"]["events"]["tool.before"]["steps"][0]["enabled"] = json!(false);
    document["spec"]["events"]["tool.after"]["enabled"] = json!(false);
    document["spec"]["events"]["tool.after"]["required"] = json!(false);
    document["spec"]["events"]["permission.request"] = json!({
        "enabled": false, "steps": [{
            "id": "later", "provider": "policy", "operation": "not-discovered",
            "effects": ["observe"], "on_error": "report"
        }]
    });
    let steps = admit(
        &configuration(&document),
        "qoder",
        &capabilities("qoder"),
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(steps.is_empty());
}

#[test]
fn enabled_steps_require_discovery_and_matching_private_validation() {
    let document = document();
    let config = configuration(&document);
    let adapter = capabilities("qoder");
    assert!(admit(&config, "qoder", &adapter, &BTreeMap::new()).is_err());
    let wrong_config = json!({"secret": "must-not-appear-in-errors"});
    let error = admit(
        &config,
        "qoder",
        &adapter,
        &evidence(&wrong_config, operations()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("private configuration"));
    assert!(!error.contains("must-not-appear-in-errors"));
    assert!(!error.contains("threshold"));
}

#[test]
fn operation_event_and_effect_must_all_be_declared_by_the_provider() {
    let document = document();
    let config = configuration(&document);
    let private_config = &document["spec"]["providers"]["policy"]["config"];
    for (field, replacement) in [
        ("name", json!("different-operation")),
        ("events", json!(["tool.after"])),
        ("effects", json!(["observe"])),
    ] {
        let mut operations = operations();
        operations[0][field] = replacement;
        assert!(admit(
            &config,
            "qoder",
            &capabilities("qoder"),
            &evidence(private_config, operations)
        )
        .is_err());
    }
}

#[test]
fn failure_block_requires_host_capability_even_for_observation_only_provider() {
    let mut document = document();
    document["spec"]["events"]["tool.before"]["steps"][0]["effects"] = json!(["observe"]);
    let mut operations = operations();
    operations[0]["effects"] = json!(["observe"]);
    let providers = evidence(
        &document["spec"]["providers"]["policy"]["config"],
        operations,
    );
    let config = configuration(&document);
    let mut adapter = capabilities("qoder");
    assert!(admit(&config, "qoder", &adapter, &providers).is_ok());
    adapter
        .events
        .insert("tool.before".into(), vec!["observe".into()]);
    let error = admit(&config, "qoder", &adapter, &providers)
        .unwrap_err()
        .to_string();
    assert!(error.contains("failure action"));
    document["spec"]["events"]["tool.before"]["steps"][0]["on_error"] = json!("report");
    assert!(admit(&configuration(&document), "qoder", &adapter, &providers).is_ok());
}

#[test]
fn replace_effects_and_result_withholding_remain_explicitly_unsupported() {
    for (event, field, replacement) in [
        ("tool.before", "effects", json!(["replace_input"])),
        ("tool.after", "effects", json!(["replace_result"])),
        ("tool.after", "on_error", json!("withhold_result")),
    ] {
        let mut document = document();
        document["spec"]["events"][event]["steps"][0][field] = replacement;
        assert!(admit(
            &configuration(&document),
            "qoder",
            &capabilities("qoder"),
            &providers(&document)
        )
        .is_err());
    }
}

#[test]
fn all_tools_is_accepted_while_exact_selectors_and_guards_are_rejected() {
    let mut document = document();
    document["spec"]["events"]["tool.before"]["match"] = json!({"tools": ["*"]});
    let adapter = capabilities("qoder");
    let providers = providers(&document);
    assert!(admit(&configuration(&document), "qoder", &adapter, &providers).is_ok());
    for tool in ["bash", "native:qoder:custom_tool"] {
        document["spec"]["events"]["tool.before"]["match"] = json!({"tools": [tool]});
        assert!(admit(&configuration(&document), "qoder", &adapter, &providers).is_err());
    }
    document["spec"]["events"]["tool.before"]["match"] = json!({"tools": ["*"]});
    document["spec"]["events"]["tool.before"]["guard"] = json!("security.violation");
    document["spec"]["events"]["security.violation"] = json!({"enabled": true, "steps": []});
    assert!(admit(&configuration(&document), "qoder", &adapter, &providers).is_err());
}

#[test]
fn configuration_rejects_unknown_references_and_non_native_guarantees_before_admission() {
    for (pointer, replacement) in [
        (
            "/spec/events/tool.before/steps/0/provider",
            json!("missing"),
        ),
        ("/spec/events/tool.before/steps/0/effects", json!(["ask"])),
        ("/spec/execution/guarantee", json!("protected")),
        ("/spec/providers/policy/protocol", json!("unknown/v1")),
        ("/spec/providers/policy/transport/type", json!("http")),
        ("/spec/providers/policy/transport/location", json!("daemon")),
    ] {
        let mut document = document();
        *document.pointer_mut(pointer).unwrap() = replacement;
        assert!(CONFIGURATION
            .parse(&serde_json::to_vec(&document).unwrap())
            .is_err());
    }
}

#[test]
fn enabled_step_order_is_preserved_within_each_event() {
    let mut document = document();
    let mut additional = document["spec"]["events"]["tool.before"]["steps"][0].clone();
    additional["id"] = json!("check-second");
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(additional);
    let steps = admit(
        &configuration(&document),
        "qoder",
        &capabilities("qoder"),
        &providers(&document),
    )
    .unwrap();
    assert_eq!(
        steps
            .iter()
            .filter(|step| step.event == "tool.before")
            .map(|step| step.step_id.as_str())
            .collect::<Vec<_>>(),
        ["check", "check-second"]
    );
}
