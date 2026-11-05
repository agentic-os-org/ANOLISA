//! Tool selectors share syntax across events and every step protocol.

use aw_config::{Error, Validator};
use serde_json::{json, Value};

fn document() -> Value {
    let mut value = Validator::new()
        .unwrap()
        .parse(include_bytes!("../examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["providers"]["check"] = json!({
        "transport":{"type":"stdio","location":"agent","argv":["/bin/true"]},
        "timeout_ms":1000,"max_output_bytes":32
    });
    value["spec"]["events"]["tool.before"]["steps"] = json!([
        {"id":"check","provider":"check","effects":["block"],"on_error":"block"}
    ]);
    value
}

#[test]
fn selectors_preserve_explicit_configuration_for_every_step_protocol() {
    let validator = Validator::new().unwrap();
    for (protocol, step) in [
        ("aw-hook/v1alpha1", json!({"effects":["block"]})),
        (
            "aw-provider/v1alpha1",
            json!({"operation":"check","effects":["block"]}),
        ),
        ("native-hook/v1alpha1", json!({"native":{}})),
    ] {
        for selectors in [
            json!(["*"]),
            json!(["bash", "file_read", "file_write"]),
            json!([
                "native:pi:custom:name",
                "native:claude-sdk:mcp__files__send"
            ]),
        ] {
            let mut value = document();
            for adapter in ["pi", "claude-sdk"] {
                value["spec"]["agents"][adapter] = json!({"adapter":adapter,"argv":["/bin/true"]});
            }
            value["spec"]["providers"]["check"]["protocol"] = json!(protocol);
            value["spec"]["providers"]["check"]["config"] = json!({});
            let mut selected = step.clone();
            selected["id"] = json!("check");
            selected["provider"] = json!("check");
            selected["on_error"] = json!("block");
            selected["match"] = json!({"tools":selectors});
            value["spec"]["events"]["tool.before"]["steps"] = json!([selected]);
            let parsed = validator
                .parse(&serde_json::to_vec(&value).unwrap())
                .unwrap();
            assert_eq!(parsed.as_value(), &value);
        }
    }
    let value = document();
    let parsed = validator
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    assert!(
        parsed.as_value()["spec"]["events"]["tool.before"]["steps"][0]
            .get("match")
            .is_none()
    );
}

#[test]
fn native_selectors_reject_undeclared_adapters_at_event_and_step_levels() {
    let validator = Validator::new().unwrap();
    for path in [
        "/spec/events/tool.before",
        "/spec/events/tool.before/steps/0",
    ] {
        for enabled in [true, false] {
            let mut value = document();
            let selected = value.pointer_mut(path).unwrap();
            selected["enabled"] = json!(enabled);
            if path == "/spec/events/tool.before" {
                selected["required"] = json!(enabled);
            }
            selected["match"] = json!({"tools":["bash", "native:openclaw:exec"]});
            let error = validator
                .parse(&serde_json::to_vec(&value).unwrap())
                .err()
                .expect("undeclared Adapter must fail validation");
            assert!(matches!(
                &error,
                Error::Invariant { path: actual, reason }
                    if actual == &format!("{path}/match/tools/1")
                    && *reason == "native tool selector requires an Adapter declared in spec.agents"
            ));
            assert_eq!(
                error.to_string(),
                format!(
                    "AW configuration field {path}/match/tools/1: \
                     native tool selector requires an Adapter declared in spec.agents"
                )
            );
        }
    }
}

#[test]
fn native_selectors_use_adapter_union_across_shared_agent_targets() {
    let validator = Validator::new().unwrap();
    let mut value = document();
    value["spec"]["agents"] = json!({
        "terminal":{"adapter":"qoder","argv":["/bin/true"]},
        "gateway":{"adapter":"openclaw","argv":["/bin/true"]}
    });
    value["spec"]["events"]["tool.before"]["match"] =
        json!({"tools":["native:qoder:Bash", "native:openclaw:exec"]});
    let mut qoder = value["spec"]["events"]["tool.before"]["steps"][0].clone();
    qoder["id"] = json!("check-qoder");
    qoder["match"] = json!({"tools":["native:qoder:Bash"]});
    let mut openclaw = qoder.clone();
    openclaw["id"] = json!("check-openclaw");
    openclaw["match"] = json!({"tools":["native:openclaw:exec"]});
    value["spec"]["events"]["tool.before"]["steps"] = json!([qoder, openclaw]);
    let parsed = validator
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    assert_eq!(parsed.as_value(), &value);
}

#[test]
fn invalid_selectors_are_rejected_even_on_disabled_steps() {
    let validator = Validator::new().unwrap();
    for enabled in [true, false] {
        for selector in [
            json!({}),
            json!({"tools":[]}),
            json!({"tools":["*","bash"]}),
            json!({"tools":["bash","bash"]}),
            json!({"tools":["shell"]}),
            json!({"tools":["native:unknown:run"]}),
            json!({"tools":["native:pi:"]}),
            json!({"tools":["native:pi:run*"]}),
            json!({"tools":["native:pi:run\n"]}),
            json!({"tools":["bash"],"extra":true}),
            json!({"tools":"bash"}),
        ] {
            let mut value = document();
            value["spec"]["events"]["tool.before"]["steps"][0]["enabled"] = json!(enabled);
            value["spec"]["events"]["tool.before"]["steps"][0]["match"] = selector.clone();
            assert!(
                validator
                    .parse(&serde_json::to_vec(&value).unwrap())
                    .is_err(),
                "accepted selector {selector}"
            );
        }
    }
}
