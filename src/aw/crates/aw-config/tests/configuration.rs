use aw_config::{Error, Validator, MAX_DEPTH, MAX_DOCUMENT_BYTES, SCHEMA};
use serde_json::{json, Value};
use std::sync::LazyLock;

static VALIDATOR: LazyLock<Validator> = LazyLock::new(|| Validator::new().unwrap());
const EXAMPLE: &str = include_str!("../examples/aw.yaml");

fn example() -> Value {
    VALIDATOR
        .parse(EXAMPLE.as_bytes())
        .unwrap()
        .as_value()
        .clone()
}

fn parse(value: &Value) -> Result<aw_config::Configuration, Error> {
    VALIDATOR.parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn starter_example_needs_no_policy_program_and_requests_only_tool_events() {
    let configuration = VALIDATOR
        .parse(include_bytes!("../examples/aw.minimal.yaml"))
        .unwrap();
    let spec = &configuration.as_value()["spec"];
    assert_eq!(spec["providers"], json!({}));
    let events = spec["events"].as_object().unwrap();
    assert_eq!(events.len(), 2);
    for name in ["tool.before", "tool.after"] {
        assert_eq!(events[name]["enabled"], true);
        assert_eq!(events[name]["required"], true);
        assert_eq!(events[name]["steps"], json!([]));
        assert!(events[name].get("guard").is_none());
    }
}

#[test]
fn one_configuration_covers_all_four_adapters_and_sixteen_event_names() {
    let mut value = example();
    for adapter in ["qwenpaw", "qoder", "openclaw", "hermes"] {
        value["spec"]["agents"][adapter] = json!({
            "adapter": adapter,
            "argv": ["/nonexistent/agent", "--argument", ""]
        });
    }
    let configuration = parse(&value).unwrap();
    assert_eq!(configuration.as_value(), &value);
    assert_eq!(value["spec"]["events"].as_object().unwrap().len(), 16);
    // Configuration acceptance cannot depend on programs existing or running.
    let events = value["spec"]["events"].as_object().unwrap();
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let allowed = schema["properties"]["spec"]["properties"]["events"]["properties"]
        .as_object()
        .unwrap();
    assert_eq!(
        events.keys().collect::<Vec<_>>(),
        allowed.keys().collect::<Vec<_>>()
    );
}

#[test]
fn opaque_provider_config_preserves_json_values_without_wire_canonical_restrictions() {
    let mut value = example();
    value["spec"]["providers"]["business"]["config"] = json!({
        "threshold": 0.75, "标签": "平台", "options": [true, null, {"unicode": "🦀"}]
    });
    assert_eq!(parse(&value).unwrap().as_value(), &value);
    let yaml = serde_yaml_ng::to_string(&value).unwrap();
    assert_eq!(VALIDATOR.parse(yaml.as_bytes()).unwrap().as_value(), &value);
}

#[test]
fn unknown_public_fields_versions_targets_and_types_fail() {
    for pointer in [
        "",
        "/metadata",
        "/spec",
        "/spec/daemon",
        "/spec/execution",
        "/spec/audit",
        "/spec/agents/qoder",
        "/spec/providers/security",
        "/spec/providers/security/transport",
        "/spec/events/tool.before",
        "/spec/events/tool.before/match",
        "/spec/events/tool.before/steps/0",
    ] {
        let mut value = example();
        value.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
        assert!(
            matches!(parse(&value), Err(Error::Shape { .. })),
            "{pointer}"
        );
    }
    for (pointer, replacement) in [
        ("/apiVersion", json!("aw/v99")),
        ("/kind", json!("Policy")),
        ("/spec/agents/qoder/adapter", json!("qwen-code")),
        ("/spec/agents/qoder/argv", json!([])),
        ("/spec/agents/qoder/argv", json!([""])),
        ("/spec/agents/qoder/argv", json!(["qoder\u{0000}cli"])),
        (
            "/spec/providers/security/transport",
            json!({"type": "http", "url": "https://example.invalid"}),
        ),
        ("/spec/providers/security/protocol", json!("unknown/v1")),
        ("/spec/providers/security/timeout_ms", json!(0)),
        ("/spec/providers/security/max_output_bytes", json!(-1)),
        ("/spec/providers/security/config", json!([])),
        ("/spec/execution/default_event_budget_ms", json!("5000")),
        ("/spec/execution/guarantee", json!("protected")),
        ("/spec/audit/enabled", json!(false)),
        ("/spec/audit/payload", json!("raw")),
        ("/spec/events/tool.before/enabled", json!("true")),
        ("/spec/events/tool.before/steps/0/effects", json!(["allow"])),
    ] {
        let mut value = example();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            matches!(parse(&value), Err(Error::Shape { .. })),
            "{pointer}"
        );
    }
    let mut value = example();
    value["status"] = json!({"Ready": true});
    assert!(parse(&value).is_err());
    value = example();
    value["spec"]["events"]["tool.befor"] = json!({"enabled": true, "steps": []});
    assert!(parse(&value).is_err());
    value = example();
    let version = value.as_object_mut().unwrap().remove("apiVersion").unwrap();
    value["api_version"] = version;
    assert!(parse(&value).is_err());
}

#[test]
fn references_and_duplicate_step_ids_are_checked_even_in_disabled_steps() {
    let mut value = example();
    value["spec"]["events"]["tool.after"]["steps"][1]["provider"] = json!("missing");
    let error = parse(&value).err().unwrap().to_string();
    assert!(error.contains("/spec/events/tool.after/steps/1/provider"));
    assert!(error.contains("unknown Provider"));
    value = example();
    let step = value["spec"]["events"]["tool.before"]["steps"][0].clone();
    value["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(step);
    assert!(parse(&value)
        .err()
        .unwrap()
        .to_string()
        .contains("duplicate step ID"));
}

#[test]
fn final_guard_must_be_linked_enabled_and_read_only() {
    let mut value = example();
    value["spec"]["events"]["security.violation"]["enabled"] = json!(false);
    value["spec"]["events"]["security.violation"]["required"] = json!(false);
    assert!(parse(&value).is_err());
    value = example();
    value["spec"]["events"]
        .as_object_mut()
        .unwrap()
        .remove("security.violation");
    assert!(parse(&value).is_err());
    value = example();
    value["spec"]["events"]["tool.before"]
        .as_object_mut()
        .unwrap()
        .remove("guard");
    assert!(parse(&value).is_err());
    value = example();
    value["spec"]["events"]["security.violation"]["steps"][0]["effects"] = json!(["replace_input"]);
    assert!(parse(&value).is_err());
    value = example();
    value["spec"]["events"]["tool.after"]["guard"] = json!("security.violation");
    assert!(parse(&value).is_err());
}

#[test]
fn actions_respect_event_timing_and_active_ask_is_rejected() {
    for (event, field, replacement) in [
        ("tool.before", "effects", json!(["replace_result"])),
        ("tool.after", "effects", json!(["block"])),
        ("tool.before", "on_error", json!("withhold_result")),
        ("tool.after", "on_error", json!("block")),
        ("tool.before", "effects", json!(["ask"])),
    ] {
        let mut value = example();
        value["spec"]["events"][event]["steps"][0][field] = replacement;
        assert!(parse(&value).is_err(), "{event}/{field}");
    }
    let mut value = example();
    value["spec"]["events"]["tool.before"]["steps"][0]["effects"] = json!(["ask"]);
    value["spec"]["events"]["tool.before"]["steps"][0]["enabled"] = json!(false);
    assert!(parse(&value).is_ok());
    value = example();
    value["spec"]["events"]["session.start"]["steps"] = json!([{
        "id":"control", "provider":"business", "operation":"start",
        "effects":["block"], "on_error":"report"
    }]);
    assert!(parse(&value).is_err());
}

#[test]
fn exact_native_selectors_cover_custom_tools_without_regex_semantics() {
    for tools in [
        json!(["*"]),
        json!(["bash", "file_read", "file_write"]),
        json!([
            "native:qwenpaw:custom",
            "native:qoder:mcp__read",
            "native:hermes:tool",
            "native:openclaw:exec"
        ]),
    ] {
        let mut value = example();
        value["spec"]["events"]["tool.before"]["match"]["tools"] = tools;
        assert!(parse(&value).is_ok());
    }
    for tools in [
        json!([]),
        json!(["*", "bash"]),
        json!(["bash", "bash"]),
        json!(["Bash"]),
        json!(["native:qoder:"]),
        json!(["native:unknown:exec"]),
        json!(["native:qoder:foo*"]),
        json!(["native:qoder:bad\nname"]),
    ] {
        let mut value = example();
        value["spec"]["events"]["tool.before"]["match"]["tools"] = tools;
        assert!(parse(&value).is_err());
    }
}

#[test]
fn disabled_or_omitted_events_do_not_claim_support_and_defaults_stay_omitted() {
    let mut value = example();
    value["spec"]["events"] = json!({"tool.before": {"enabled": true, "steps": []}});
    value["spec"]["providers"] = json!({});
    assert_eq!(parse(&value).unwrap().as_value(), &value);
    value["spec"]["events"]["tool.before"]["enabled"] = json!(false);
    value["spec"]["events"]["tool.before"]["required"] = json!(true);
    assert!(parse(&value).is_err());
}

#[test]
fn yaml_rejects_duplicate_keys_tags_merge_keys_non_string_keys_and_multiple_documents() {
    for input in [
        "key: 1\nkey: 2",
        "config: {a: 1, a: 2}",
        r#"{"config":{"a":1,"a":2}}"#,
        "key: !custom 1",
        "key: {<<: {a: 1}}",
        "key: {1: value}",
        "key: {true: value}",
        "key: {[a]: value}",
        "key: .nan",
        "key: .inf",
        "key: 1\n---\nkey: 2",
        "key: 1\n---",
        "key: [unterminated",
    ] {
        assert!(
            matches!(
                VALIDATOR.parse(input.as_bytes()),
                Err(Error::Document { .. })
            ),
            "{input}"
        );
    }
    let input = EXAMPLE.replace(
        "project: platform",
        "project: platform\n        project: duplicate",
    );
    assert!(VALIDATOR
        .parse(input.as_bytes())
        .err()
        .unwrap()
        .to_string()
        .contains("duplicate mapping key"));
}

#[test]
fn unrepresentable_integer_literals_cannot_become_rounded_floats() {
    for literal in [
        "340282366920938463463374607431768211456",
        "10000000000000000000000000000000000000000",
        "-170141183460469231731687303715884105729",
    ] {
        for value in [
            format!("project: {literal}"),
            format!("project: [{literal}]"),
            format!("project:\n          - {literal}"),
        ] {
            let input = EXAMPLE.replace("project: platform", &value);
            assert!(
                input.contains(literal),
                "{literal} is absent from the example"
            );
            assert!(
                VALIDATOR.parse(input.as_bytes()).is_err(),
                "{value} was accepted"
            );
        }
    }
    for config in [
        "project: 18446744073709551615",
        "project: 0.75",
        "project: 1e300",
        "project: 1.7976931348623157e308",
        "project: \"10000000000000000000000000000000000000000\"",
        "project: platform\n        notes: |\n          10000000000000000000000000000000000000000",
    ] {
        let input = EXAMPLE.replace("project: platform", config);
        assert!(
            VALIDATOR.parse(input.as_bytes()).is_ok(),
            "{config} was rejected"
        );
    }
}

#[test]
fn anchored_and_aliased_integer_literals_are_still_rejected() {
    for config in [
        "project: &id 10000000000000000000000000000000000000000",
        "project: &id platform\n        limit: &n 10000000000000000000000000000000000000000\n        again: *n",
        "project: [&n 10000000000000000000000000000000000000000, *n]",
    ] {
        let input = EXAMPLE.replace("project: platform", config);
        assert!(
            VALIDATOR.parse(input.as_bytes()).is_err(),
            "{config} was accepted"
        );
    }
}

#[test]
fn digits_outside_a_plain_scalar_do_not_reject_the_document() {
    for config in [
        "project: platform # x: 10000000000000000000000000000000000000000\n        limit: 1e40",
        "project: \"x: 10000000000000000000000000000000000000000\"\n        limit: 1e40",
        "project: 'x, 10000000000000000000000000000000000000000'\n        limit: 1e40",
        "project: |\n          x: 10000000000000000000000000000000000000000\n          - 10000000000000000000000000000000000000000\n        limit: 1e40",
        "project: >-\n          [10000000000000000000000000000000000000000]\n        limit: 1e40",
    ] {
        let input = EXAMPLE.replace("project: platform", config);
        assert!(
            VALIDATOR.parse(input.as_bytes()).is_ok(),
            "{config} was rejected"
        );
    }
}

#[test]
fn errors_locate_invalid_fields_without_echoing_secret_values() {
    let mut value = example();
    value["spec"]["providers"]["security"]["timeout_ms"] = json!("sensitive-value-123");
    let error = parse(&value).err().unwrap();
    assert!(error
        .to_string()
        .contains("/spec/providers/security/timeout_ms"));
    assert!(!format!("{error:?} {error}").contains("sensitive-value-123"));
    let error = VALIDATOR
        .parse(b"key: !custom sensitive-value-123")
        .err()
        .unwrap();
    assert!(!format!("{error:?} {error}").contains("sensitive-value-123"));
}

#[test]
fn document_depth_size_and_alias_expansion_are_bounded() {
    assert!(matches!(
        VALIDATOR.parse(&[0xff, 0xfe, b'a', 0]),
        Err(Error::Document { .. })
    ));
    let deep = format!(
        "{}0{}",
        "[".repeat(MAX_DEPTH + 2),
        "]".repeat(MAX_DEPTH + 2)
    );
    assert!(matches!(
        VALIDATOR.parse(deep.as_bytes()),
        Err(Error::Document { .. })
    ));
    assert!(matches!(
        VALIDATOR.parse(&vec![b' '; MAX_DOCUMENT_BYTES + 1]),
        Err(Error::Document { .. })
    ));
    let expanded = format!(
        "a: &a {}\nb: [{}]\n",
        "x".repeat(4096),
        vec!["*a"; 1100].join(",")
    );
    assert!(matches!(
        VALIDATOR.parse(expanded.as_bytes()),
        Err(Error::Document { .. })
    ));
    assert!(matches!(
        VALIDATOR.parse(b"a: &a [*a]"),
        Err(Error::Document { .. })
    ));
}
