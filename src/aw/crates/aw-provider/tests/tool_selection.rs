//! Exact selectors use the admitted event identity and never infer aliases.

use aw_provider::admission::{preflight, AdapterCapabilities};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn selected(
    event_tools: Option<Value>,
    step_tools: Option<Value>,
    adapter: &str,
) -> Option<aw_provider::admission::AdmittedStep> {
    let validator = aw_config::Validator::new().unwrap();
    let mut value = validator
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    for adapter in ["claude-sdk", "pi"] {
        value["spec"]["agents"][adapter] = json!({"adapter":adapter,"argv":["/bin/true"]});
    }
    value["spec"]["providers"]["check"] = json!({
        "transport":{"type":"stdio","location":"agent","argv":["/bin/true"]},
        "timeout_ms":1000,"max_output_bytes":32
    });
    value["spec"]["events"]["tool.before"]["steps"] = json!([
        {"id":"check","provider":"check","effects":["block"],"on_error":"block"}
    ]);
    if let Some(tools) = event_tools {
        value["spec"]["events"]["tool.before"]["match"] = json!({"tools":tools});
    }
    if let Some(tools) = step_tools {
        value["spec"]["events"]["tool.before"]["steps"][0]["match"] = json!({"tools":tools});
    }
    let config = validator
        .parse(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let caps = AdapterCapabilities {
        failure_actions: Default::default(),
        adapter: adapter.into(),
        version: "fixture".into(),
        entrypoint: "fixture".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
    };
    preflight(&config, adapter, &caps)
        .unwrap()
        .into_iter()
        .find(|step| step.step_id == "check")
}

#[test]
fn selectors_intersect_and_match_only_exact_identity() {
    let event = json!({"agent":{"adapter":"claude-sdk"},
        "tool":{"name":"bash","native_name":"Bash"}});
    for (outer, inner, expected) in [
        (None, None, true),
        (Some(json!(["*"])), Some(json!(["bash"])), true),
        (
            Some(json!(["bash", "file_read"])),
            Some(json!(["native:claude-sdk:Bash"])),
            true,
        ),
        (Some(json!(["file_read"])), Some(json!(["bash"])), false),
        (Some(json!(["bash"])), Some(json!(["file_read"])), false),
        (None, Some(json!(["native:qoder:Bash"])), false),
        (None, Some(json!(["native:claude-sdk:bash"])), false),
        (None, Some(json!(["native:claude-sdk:Bash:extra"])), false),
    ] {
        assert_eq!(
            selected(outer, inner, "claude-sdk").is_some_and(|step| step.matches_tool(&event)),
            expected
        );
    }
    // The CLI keeps its native tool name; an SDK's canonical mapping is not inferred.
    let cli = json!({"agent":{"adapter":"qoder"},"tool":{"name":"Bash","native_name":"Bash"}});
    assert!(!selected(None, Some(json!(["bash"])), "qoder")
        .unwrap()
        .matches_tool(&cli));
    assert!(selected(None, Some(json!(["native:qoder:Bash"])), "qoder")
        .unwrap()
        .matches_tool(&cli));
    let custom = json!({"agent":{"adapter":"pi"},"tool":{"name":"x:y","native_name":"x:y"}});
    assert!(selected(None, Some(json!(["native:pi:x:y"])), "pi")
        .unwrap()
        .matches_tool(&custom));
}
