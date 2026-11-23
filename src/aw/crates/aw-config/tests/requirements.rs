//! Desired requirements preserve empty events, selector scope and policy ordering.

use aw_config::{
    requirements::{project, requires_service},
    Validator,
};
use serde_json::{json, Value};

fn document() -> Value {
    let validator = Validator::new().unwrap();
    let mut value = validator
        .parse(include_bytes!("../examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    value["spec"]["agents"]["pi"] = json!({"adapter":"pi","argv":["/missing/agent"]});
    value["spec"]["providers"]["check"] = json!({
        "transport":{"type":"stdio","location":"agent","argv":["/missing/provider"]},
        "timeout_ms":1000,"max_output_bytes":32
    });
    value
}

fn step(id: &str, tools: Value) -> Value {
    json!({"id":id,"provider":"check","effects":["observe"],
        "on_error":"report","match":{"tools":tools}})
}

#[test]
fn target_names_scope_empty_events_and_intersect_with_step_and_tool_selectors() {
    let mut value = document();
    // A second target using the same Adapter must still have an independent policy.
    value["spec"]["agents"]["second"] = value["spec"]["agents"]["qoder"].clone();
    value["spec"]["events"] = json!({
        "session.start": {"enabled": true, "match": {"agents": ["qoder"]}, "steps": []},
        "input.submit": {"enabled": true, "steps": [{
            "id": "prompt", "provider": "check", "effects": ["observe"], "on_error": "report",
            "match": {"agents": ["second"]}
        }]},
        "tool.before": {"enabled": true, "match": {"agents": ["qoder", "second"], "tools": ["bash"]},
            "steps": [
                {"id": "shell", "provider": "check", "effects": ["observe"], "on_error": "report",
                 "match": {"agents": ["second", "pi"], "tools": ["bash"]}},
                {"id": "read", "provider": "check", "effects": ["observe"], "on_error": "report",
                 "match": {"agents": ["second"], "tools": ["file_read"]}}
            ]}
    });
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let qoder = project(&config, "qoder").unwrap();
    assert_eq!(qoder.len(), 1);
    assert_eq!(qoder[0].event, "session.start");
    assert!(qoder[0].steps.is_empty());
    let second = project(&config, "second").unwrap();
    assert_eq!(second.len(), 2);
    for (event, id) in [("input.submit", "prompt"), ("tool.before", "shell")] {
        let steps = &second.iter().find(|req| req.event == event).unwrap().steps;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step_id, id);
    }
    assert!(project(&config, "pi").unwrap().is_empty());
}

#[test]
fn service_availability_follows_target_selector_projection() {
    for (outer, steps, applicable) in [
        (json!(["*"]), json!([]), true),
        (json!(["native:pi:Shell"]), json!([]), false),
        (json!(["native:qoder:Shell"]), json!([]), true),
        (
            json!(["*"]),
            json!([step("foreign", json!(["native:pi:Shell"]))]),
            false,
        ),
        (
            json!(["bash"]),
            json!([step("disjoint", json!(["file_read"]))]),
            false,
        ),
        (
            json!(["bash"]),
            json!([step("alias", json!(["native:qoder:Shell"]))]),
            true,
        ),
        (
            json!(["native:qoder:Shell"]),
            json!([step("alias", json!(["bash"]))]),
            true,
        ),
        (
            json!(["*"]),
            json!([
                step("foreign", json!(["native:pi:Shell"])),
                step("own", json!(["native:qoder:Shell"]))
            ]),
            true,
        ),
        (
            json!(["*"]),
            json!([{
                "id":"disabled", "enabled":false, "provider":"check",
                "effects":["observe"], "on_error":"report", "match":{"tools":["native:pi:Shell"]}
            }]),
            true,
        ),
    ] {
        for required in [false, true] {
            let mut value = document();
            value["spec"]["events"] = json!({"tool.before":{
                "enabled":true,"required":required,"match":{"tools":outer},"steps":steps
            }});
            let config = Validator::new()
                .unwrap()
                .parse(&serde_json::to_vec(&value).unwrap())
                .unwrap();
            assert_eq!(
                requires_service(&config, "qoder").unwrap(),
                required && applicable,
                "outer={outer}, steps={steps}, required={required}"
            );
        }
    }
}

#[test]
fn service_availability_keeps_required_input_and_rejects_unknown_targets() {
    for (enabled, required) in [(false, false), (true, false), (true, true)] {
        let mut value = document();
        value["spec"]["events"] = json!({"input.submit":{
            "enabled":enabled,"required":required,"steps":[]
        }});
        let config = Validator::new()
            .unwrap()
            .parse(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        for target in ["qoder", "pi"] {
            assert_eq!(
                requires_service(&config, target).unwrap(),
                enabled && required
            );
        }
        assert!(requires_service(&config, "missing").is_err());
    }
}

#[test]
fn empty_and_disabled_only_events_retain_capability_requirements() {
    let mut value = document();
    value["spec"]["events"]["input.submit"] = json!({
        "enabled":true,"required":false,"budget_ms":37,"steps":[]
    });
    value["spec"]["events"]["tool.before"]["steps"] = json!([{
        "enabled":false,"id":"disabled","provider":"check",
        "effects":["observe"],"on_error":"report"
    }]);
    value["spec"]["events"]["session.start"] = json!({"enabled":false,"steps":[]});
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let requirements = project(&config, "qoder").unwrap();
    assert_eq!(requirements.len(), 3);
    let input = requirements
        .iter()
        .find(|event| event.event == "input.submit")
        .unwrap();
    assert!(!input.required);
    assert_eq!(input.budget_ms, 37);
    assert!(input.steps.is_empty());
    let before = requirements
        .iter()
        .find(|event| event.event == "tool.before")
        .unwrap();
    assert_eq!(before.budget_ms, 5000);
    assert!(before.steps.is_empty());
    assert_eq!(before.event_tools, ["*"]);
    assert!(project(&config, "missing").is_err());
}

#[test]
fn projection_keeps_order_budgets_effects_and_failure_actions_separate() {
    let mut value = document();
    value["spec"]["events"]["tool.after"]["steps"] = json!([
        {"id":"record","provider":"check","effects":["observe"],"on_error":"withhold_result"},
        {"id":"disabled","enabled":false,"provider":"check","effects":["observe"],"on_error":"report"},
        {"id":"audit","provider":"check","effects":["observe"],"on_error":"report"}
    ]);
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let requirements = project(&config, "qoder").unwrap();
    let after = requirements
        .iter()
        .find(|event| event.event == "tool.after")
        .unwrap();
    assert_eq!(
        after
            .steps
            .iter()
            .map(|step| step.step_id)
            .collect::<Vec<_>>(),
        ["record", "audit"]
    );
    assert_eq!(after.steps[0].effects, ["observe"]);
    assert_eq!(after.steps[0].on_error, "withhold_result");
    assert_eq!(after.steps[0].step_tools, ["*"]);
    assert_eq!(
        after.steps[0].configuration(),
        &value["spec"]["events"]["tool.after"]["steps"][0]
    );
    assert_eq!(
        after.configuration(),
        &value["spec"]["events"]["tool.after"]
    );
}

#[test]
fn selectors_project_only_proven_disjoint_steps_without_reordering() {
    for (outer, inner, target, included) in [
        (json!(["*"]), json!(["bash"]), "qoder", true),
        (json!(["bash"]), json!(["file_read"]), "qoder", false),
        (
            json!(["bash"]),
            json!(["native:qoder:Shell"]),
            "qoder",
            true,
        ),
        (
            json!(["native:qoder:Shell"]),
            json!(["bash"]),
            "qoder",
            true,
        ),
        (
            json!(["native:qoder:Shell"]),
            json!(["native:qoder:Read"]),
            "qoder",
            false,
        ),
        (json!(["*"]), json!(["native:qoder:Shell"]), "pi", false),
        (json!(["native:qoder:Shell"]), json!(["*"]), "pi", false),
    ] {
        let mut value = document();
        value["spec"]["events"]["tool.before"]["match"] = json!({"tools":outer});
        value["spec"]["events"]["tool.before"]["steps"] = json!([step("selected", inner)]);
        let config = Validator::new()
            .unwrap()
            .parse(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        let requirements = project(&config, target).unwrap();
        assert_eq!(
            requirements
                .iter()
                .any(|event| event.event == "tool.before"),
            included,
            "{outer} for {target}"
        );
    }
}

#[test]
fn native_steps_retain_raw_semantics_without_inventing_provider_effects() {
    let mut value = document();
    value["spec"]["providers"]["check"]["protocol"] = json!("native-hook/v1alpha1");
    value["spec"]["providers"]["check"]["config"] = json!({});
    value["spec"]["events"]["tool.before"]["steps"] = json!([
        {"id":"native","provider":"check","native":{},"on_error":"block"}
    ]);
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let requirements = project(&config, "qoder").unwrap();
    let before = requirements
        .iter()
        .find(|event| event.event == "tool.before")
        .unwrap();
    assert!(before.steps[0].effects.is_empty());
    assert_eq!(before.steps[0].on_error, "block");
    assert_eq!(before.steps[0].configuration()["native"], json!({}));
}

#[test]
fn integral_decimal_budgets_preserve_default_and_event_override() {
    let mut value = document();
    value["spec"]["execution"]["default_event_budget_ms"] = json!(5000.0);
    value["spec"]["events"]["tool.before"]["budget_ms"] = json!(37.0);
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let requirements = project(&config, "qoder").unwrap();
    assert_eq!(
        requirements
            .iter()
            .find(|event| event.event == "tool.before")
            .unwrap()
            .budget_ms,
        37
    );
    assert_eq!(
        requirements
            .iter()
            .find(|event| event.event == "tool.after")
            .unwrap()
            .budget_ms,
        5000
    );
}

#[test]
fn budget_limits_match_the_hosts_integral_u32_domain() {
    for limit in [1.0, f64::from(u32::MAX)] {
        let mut value = document();
        value["spec"]["execution"]["default_event_budget_ms"] = json!(limit);
        value["spec"]["events"]["tool.before"]["budget_ms"] = json!(limit);
        let config = Validator::new()
            .unwrap()
            .parse(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(project(&config, "qoder")
            .unwrap()
            .iter()
            .all(|event| event.budget_ms == limit as u64));
    }
    for limit in [0.0, 1.5, f64::from(u32::MAX) + 1.0] {
        for event_override in [false, true] {
            let mut value = document();
            if event_override {
                value["spec"]["events"]["tool.before"]["budget_ms"] = json!(limit);
            } else {
                value["spec"]["execution"]["default_event_budget_ms"] = json!(limit);
            }
            assert!(Validator::new()
                .unwrap()
                .parse(&serde_json::to_vec(&value).unwrap())
                .is_err());
        }
    }
}
