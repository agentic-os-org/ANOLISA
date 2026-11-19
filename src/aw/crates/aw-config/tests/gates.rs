//! Static configuration rules for decision gates and lifecycle events.

use aw_config::{Error, Validator};
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
fn turn_stop_accepts_decision_gate_actions_without_mutation_or_tool_selectors() {
    let mut value = example();
    value["spec"]["events"]["turn.stop"] = json!({
        "enabled": true,
        "required": true,
        "budget_ms": 1500,
        "steps": [{
            "id": "stop-gate",
            "provider": "business",
            "operation": "stop",
            "effects": ["observe", "block"],
            "on_error": "block"
        }]
    });
    assert!(parse(&value).is_ok());

    value["spec"]["events"]["turn.stop"]["steps"][0]["on_error"] = json!("report");
    assert!(parse(&value).is_ok());

    for effect in ["replace_input", "replace_result", "ask"] {
        let mut invalid = value.clone();
        invalid["spec"]["events"]["turn.stop"]["steps"][0]["effects"] = json!([effect]);
        assert!(parse(&invalid).is_err(), "{effect}");
    }

    let mut invalid = value.clone();
    invalid["spec"]["events"]["turn.stop"]["steps"][0]["on_error"] = json!("withhold_result");
    assert!(parse(&invalid).is_err());

    for pointer in [
        "/spec/events/turn.stop/match",
        "/spec/events/turn.stop/steps/0/match",
    ] {
        let mut invalid = value.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        invalid.pointer_mut(parent).unwrap()[field] = json!({"tools": ["*"]});
        assert!(parse(&invalid).is_err(), "{pointer}");
    }
}

#[test]
fn session_lifecycle_events_remain_observe_only_and_reject_selectors() {
    for event in ["session.start", "session.end"] {
        let mut value = example();
        value["spec"]["events"][event] = json!({
            "enabled": true,
            "required": false,
            "steps": [{
                "id": "lifecycle",
                "provider": "business",
                "operation": "record",
                "effects": ["observe"],
                "on_error": "report"
            }]
        });
        assert!(parse(&value).is_ok(), "{event}");

        for effect in ["block", "replace_input", "replace_result", "ask"] {
            let mut invalid = value.clone();
            invalid["spec"]["events"][event]["steps"][0]["effects"] = json!([effect]);
            assert!(parse(&invalid).is_err(), "{event}/{effect}");
        }

        for on_error in ["block", "withhold_result"] {
            let mut invalid = value.clone();
            invalid["spec"]["events"][event]["steps"][0]["on_error"] = json!(on_error);
            assert!(parse(&invalid).is_err(), "{event}/{on_error}");
        }

        let mut invalid = value.clone();
        invalid["spec"]["events"][event]["steps"][0]["match"] = json!({"tools": ["*"]});
        assert!(parse(&invalid).is_err(), "{event}/match");
    }
}

#[test]
fn non_tool_gates_reject_native_hook_steps_and_preserve_disabled_draft_selectors() {
    let mut base = example();
    base["spec"]["providers"]["raw"] = json!({
        "protocol": "native-hook/v1alpha1",
        "transport": {"type": "stdio", "location": "agent", "argv": ["/absent/hook"]},
        "timeout_ms": 100,
        "max_output_bytes": 1024,
        "config": {}
    });
    for event in ["session.start", "session.end", "turn.stop"] {
        let mut invalid = base.clone();
        invalid["spec"]["events"][event] = json!({
            "enabled": true,
            "steps": [{"id": "raw", "provider": "raw", "native": {}, "on_error": "report"}]
        });
        assert!(parse(&invalid).is_err(), "{event}/native-hook");
    }

    // Legacy disabled draft steps on reserved or lifecycle events retain compatibility.
    let mut draft = example();
    draft["spec"]["events"]["model.before_request"] = json!({
        "enabled": false,
        "required": false,
        "steps": [{
            "id": "draft-model",
            "provider": "business",
            "operation": "draft",
            "effects": ["observe"],
            "on_error": "report",
            "match": {"tools": ["bash"]}
        }]
    });
    draft["spec"]["events"]["session.start"] = json!({
        "enabled": true,
        "required": false,
        "steps": [{
            "id": "draft-start",
            "enabled": false,
            "provider": "business",
            "operation": "draft",
            "effects": ["observe"],
            "on_error": "report",
            "match": {"tools": ["bash"]}
        }]
    });
    assert!(parse(&draft).is_ok());

    draft["spec"]["events"]["session.start"]["steps"][0]["enabled"] = json!(true);
    assert!(parse(&draft).is_err());
}
