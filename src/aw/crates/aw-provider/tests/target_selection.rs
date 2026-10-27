//! A shared configuration admits only provably applicable target requirements.

use aw_provider::admission::{
    preflight, projected_out_steps, AdapterCapabilities, ProjectedOutStep,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn document() -> Value {
    let mut value = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["agents"] = json!({
        "sdk": {"adapter":"qoder-sdk", "argv":["/bin/true"]},
        "app": {"adapter":"qwenpaw", "argv":["/bin/true"]}
    });
    value["spec"]["providers"]["policy"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":["/not-executed"]},
        "timeout_ms":1000,"max_output_bytes":4096,"config":{}
    });
    value["spec"]["events"]["tool.after"]["steps"] = json!([
        {"id":"app-result","provider":"policy","operation":"check",
         "effects":["replace_result"],"on_error":"withhold_result",
         "match":{"tools":["native:qwenpaw:execute_shell_command"]}},
        {"id":"sdk-observe","provider":"policy","operation":"check",
         "effects":["observe"],"on_error":"report",
         "match":{"tools":["native:qoder-sdk:Bash"]}}
    ]);
    value
}

fn capabilities(adapter: &str) -> AdapterCapabilities {
    let mut caps = AdapterCapabilities {
        adapter: adapter.into(),
        version: "fixture".into(),
        entrypoint: "fixture".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
        failure_actions: BTreeMap::new(),
    };
    if adapter == "qwenpaw" {
        caps.events
            .get_mut("tool.after")
            .unwrap()
            .push("replace_result".into());
        caps.failure_actions
            .insert("tool.after".into(), vec!["withhold_result".into()]);
    }
    caps
}

fn selected(value: &Value, target: &str, adapter: &str) -> Vec<String> {
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(value).unwrap())
        .unwrap();
    preflight(&config, target, &capabilities(adapter))
        .unwrap()
        .into_iter()
        .map(|step| step.step_id)
        .collect()
}

#[test]
fn mixed_target_requirements_and_replacement_order_use_the_same_projection() {
    let value = document();
    assert_eq!(selected(&value, "sdk", "qoder-sdk"), ["sdk-observe"]);
    assert_eq!(selected(&value, "app", "qwenpaw"), ["app-result"]);
}

#[test]
fn projection_diagnostics_exclude_only_proven_inactive_enabled_steps() {
    let mut value = document();
    let event = &mut value["spec"]["events"]["tool.after"];
    event["match"] = json!({"tools":["bash"]});
    let mut ambiguous = event["steps"][1].clone();
    ambiguous["id"] = json!("sdk-unknown-alias");
    ambiguous["match"] = json!({"tools":["native:qoder-sdk:UnknownAlias"]});
    let mut disabled = event["steps"][0].clone();
    disabled["id"] = json!("disabled-app-result");
    disabled["enabled"] = json!(false);
    event["steps"]
        .as_array_mut()
        .unwrap()
        .extend([ambiguous, disabled]);
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    assert_eq!(
        projected_out_steps(&config, "sdk").unwrap(),
        [ProjectedOutStep {
            event: "tool.after".into(),
            step_id: "app-result".into(),
        }]
    );
    assert_eq!(
        selected(&value, "sdk", "qoder-sdk"),
        ["sdk-observe", "sdk-unknown-alias"]
    );
    assert!(projected_out_steps(&config, "missing").is_err());
    value["spec"]["events"]["tool.after"]["enabled"] = json!(false);
    value["spec"]["events"]["tool.after"]["required"] = json!(false);
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    assert!(projected_out_steps(&config, "sdk").unwrap().is_empty());
}

#[test]
fn event_and_step_intersection_can_prove_a_step_inactive() {
    for (outer, inner) in [
        (
            json!(["native:qwenpaw:execute_shell_command"]),
            json!(["*"]),
        ),
        (json!(["bash"]), json!(["file_read"])),
        (
            json!(["native:qoder-sdk:Bash"]),
            json!(["native:qoder-sdk:Read"]),
        ),
    ] {
        let mut value = document();
        value["spec"]["events"]["tool.after"]["match"] = json!({"tools":outer});
        value["spec"]["events"]["tool.after"]["steps"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        value["spec"]["events"]["tool.after"]["steps"][0]["match"] = json!({"tools":inner});
        assert!(selected(&value, "sdk", "qoder-sdk").is_empty());
        let config = aw_config::Validator::new()
            .unwrap()
            .parse(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert_eq!(
            projected_out_steps(&config, "sdk").unwrap(),
            [ProjectedOutStep {
                event: "tool.after".into(),
                step_id: "app-result".into(),
            }]
        );
    }
}

#[test]
fn wildcard_and_unknown_native_aliases_keep_capability_checks_strict() {
    for (outer, inner) in [
        (json!(["*"]), json!(["*"])),
        (json!(["bash"]), json!(["native:qoder-sdk:UnknownAlias"])),
        (json!(["native:qoder-sdk:Bash"]), json!(["file_read"])),
        (
            json!(["native:qwenpaw:execute_shell_command", "bash"]),
            json!(["bash"]),
        ),
    ] {
        let mut value = document();
        value["spec"]["events"]["tool.after"]["match"] = json!({"tools":outer});
        value["spec"]["events"]["tool.after"]["steps"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        value["spec"]["events"]["tool.after"]["steps"][0]["match"] = json!({"tools":inner});
        let config = aw_config::Validator::new()
            .unwrap()
            .parse(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(preflight(&config, "sdk", &capabilities("qoder-sdk")).is_err());
    }
}

#[test]
fn inactive_target_steps_still_require_globally_valid_configuration() {
    for (pointer, replacement) in [
        (
            "/spec/events/tool.after/steps/0/match/tools",
            json!(["native:qwenpa:tool"]),
        ),
        ("/spec/events/tool.after/steps/0/provider", json!("missing")),
        ("/spec/events/tool.after/steps/0/on_error", json!("block")),
    ] {
        let mut value = document();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(aw_config::Validator::new()
            .unwrap()
            .parse(&serde_json::to_vec(&value).unwrap())
            .is_err());
    }
}
