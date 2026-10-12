//! Optional schema fields and explicit JSON payload nulls are distinct.

use agentsight_atif::AtifTrajectory;
use serde_json::{json, Value};

fn document() -> AtifTrajectory {
    serde_json::from_value(json!({
        "schema_version":"ATIF-v1.7","session_id":"fixture-session",
        "agent":{"name":"fixture","version":"1"},
        "steps":[{"step_id":1,"source":"agent","message":"recorded fixture",
            "tool_calls":[{"tool_call_id":"call-a","function_name":"UpdateConfig",
                "arguments":{"patch":{"optional_setting":null},"items":[{"value":null},null]}}],
            "observation":{"results":[{"source_call_id":"call-a",
                "content":{"value":null,"present":true},"extra":{"provider_detail":null}}]},
            "extra":{"recorded_field":null}
        }],"extra":{"extension_payload":{"nullable_field":null}}
    }))
    .unwrap()
}

#[test]
fn explicit_null_tool_arguments_survive_json_value_serialization() {
    let trajectory = document();
    let expected = trajectory.steps[0].tool_calls.as_ref().unwrap()[0]
        .arguments
        .clone();
    let encoded = trajectory.to_json_value().unwrap();
    assert_eq!(encoded["steps"][0]["tool_calls"][0]["arguments"], expected);
}

#[test]
fn explicit_null_result_and_extension_members_survive() {
    let trajectory = document();
    let encoded = trajectory.to_json_value().unwrap();
    assert_eq!(
        encoded["steps"][0]["observation"]["results"][0]["content"],
        json!({"value":null,"present":true})
    );
    assert_eq!(encoded["steps"][0]["extra"], json!({"recorded_field":null}));
    assert_eq!(
        encoded["extra"],
        json!({"extension_payload":{"nullable_field":null}})
    );
    assert_eq!(
        encoded["steps"][0]["observation"]["results"][0]["extra"],
        json!({"provider_detail":null})
    );
}

#[test]
fn embedded_child_payload_and_tool_definition_nulls_survive() {
    let mut trajectory = document();
    trajectory.agent.tool_definitions = Some(vec![json!({"parameters":{"default":null}})]);
    trajectory.subagent_trajectories = Some(vec![document()]);
    let encoded = trajectory.to_json_value().unwrap();
    assert_eq!(
        encoded["agent"]["tool_definitions"],
        json!([{"parameters":{"default":null}}])
    );
    assert_eq!(
        encoded["subagent_trajectories"][0]["steps"][0]["tool_calls"][0]["arguments"],
        json!({"patch":{"optional_setting":null},"items":[{"value":null},null]})
    );
    assert!(encoded["subagent_trajectories"][0].get("notes").is_none());
}

#[test]
fn absent_optional_schema_fields_are_still_omitted() {
    let encoded = document().to_json_value().unwrap();
    assert!(encoded.get("notes").is_none());
    assert!(encoded["agent"].get("model_name").is_none());
    assert!(encoded["steps"][0].get("timestamp").is_none());
    assert!(encoded["steps"][0].get("metrics").is_none());
    assert!(encoded["steps"][0]["tool_calls"][0].get("extra").is_none());
}

#[test]
fn ordinary_nonnull_payload_and_array_nulls_remain_unchanged() {
    let mut trajectory = document();
    trajectory.steps[0].tool_calls.as_mut().unwrap()[0].arguments =
        json!({"enabled":true,"array":[null,2],"name":"fixture"});
    let encoded = trajectory.to_json_value().unwrap();
    assert_eq!(
        encoded["steps"][0]["tool_calls"][0]["arguments"],
        json!({"enabled":true,"array":[Value::Null,2],"name":"fixture"})
    );
}
