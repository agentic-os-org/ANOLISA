//! Both JSONL producers must emit the original ID of an owned result.
#![cfg(feature = "server")]

// Exercise the exact desktop converter as a pure module on Linux.
// This does not run the native macOS collection or display path.
#[path = "../src/local/collector/converter.rs"]
pub mod local_converter;

use agentsight_atif::{AtifTrajectory, EXTRA_IS_ERROR, StepSource};
use serde_json::{Value, json};

fn fixture(calls: &[&str], echo: &str, mixed: bool) -> Vec<Value> {
    let calls: Vec<Value> = calls
        .iter()
        .map(|id| {
            json!({"type":"tool_use","id":id,"name":"synthetic_echo",
                "input":{"marker":"inert fixture data"}})
        })
        .collect();
    let mut results = vec![json!({"type":"tool_result","tool_use_id":echo,
        "content":[{"type":"text","text":"Synthetic result marker"}],
        "is_error":true})];
    if mixed {
        results.push(json!({"type":"text","text":"Synthetic user continuation"}));
    }
    vec![
        json!({"type":"runtime-config","sessionId":"synthetic-session",
            "model":"synthetic-model","version":"0","entrypoint":"synthetic-agent"}),
        json!({"type":"assistant","timestamp":"2026-01-01T00:00:01Z",
            "message":{"model":"synthetic-model","content":calls}}),
        json!({"type":"user","timestamp":"2026-01-01T00:00:02Z",
            "message":{"content":results}}),
    ]
}

fn local(events: &[Value]) -> AtifTrajectory {
    let content = events
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    local_converter::convert_jsonl_content_to_atif(&content).unwrap()
}

fn trajectory(events: &[Value]) -> AtifTrajectory {
    agentsight_trajectory_collector::atif::convert_qoder_events(events, "claude-code").unwrap()
}

fn assert_owned(document: AtifTrajectory, expected_owner: &str) {
    let serialized = serde_json::to_string(&document).unwrap();
    let document = agentsight_atif::validate_trajectory_str(&serialized).unwrap();
    let issuing_step = &document.steps[0];
    assert_eq!(issuing_step.source, StepSource::Agent);
    assert!(
        issuing_step
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .any(|call| call.tool_call_id == expected_owner)
    );
    let results = &issuing_step.observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_call_id.as_deref(), Some(expected_owner));
    assert_eq!(results[0].content, Some(json!("Synthetic result marker")));
    assert_eq!(
        results[0].extra.as_ref().unwrap().get(EXTRA_IS_ERROR),
        Some(&json!(true))
    );
}

#[test]
fn maintained_reference_local_unique_echo_emits_original_id() {
    assert_owned(
        local(&fixture(&["call_cb113b8e"], "callcb113b8e", false)),
        "call_cb113b8e",
    );
}

#[test]
fn maintained_reference_trajectory_unique_echo_emits_original_id() {
    assert_owned(
        trajectory(&fixture(&["call_cb113b8e"], "callcb113b8e", false)),
        "call_cb113b8e",
    );
}

#[test]
fn maintained_reference_local_exact_id_outranks_earlier_alias() {
    assert_owned(
        local(&fixture(&["call_ab", "callab"], "callab", false)),
        "callab",
    );
}

#[test]
fn maintained_reference_trajectory_exact_id_outranks_earlier_alias() {
    assert_owned(
        trajectory(&fixture(&["call_ab", "callab"], "callab", false)),
        "callab",
    );
}

#[test]
fn maintained_reference_local_ambiguous_alias_remains_unowned() {
    let document = local(&fixture(&["call_ab", "call_a_b"], "callab", false));
    assert!(document.steps[0].observation.is_none());
}

#[test]
fn maintained_reference_trajectory_ambiguous_alias_remains_unowned() {
    let document = trajectory(&fixture(&["call_ab", "call_a_b"], "callab", false));
    assert!(document.steps[0].observation.is_none());
}

#[test]
fn maintained_reference_trajectory_unowned_mixed_replay_retains_raw_id() {
    let document = trajectory(&fixture(&["call_ab"], "foreign-replayed-id", true));
    assert!(document.steps[0].observation.is_none());
    let user = &document.steps[1];
    assert_eq!(user.source, StepSource::User);
    assert_eq!(user.message, "Synthetic user continuation");
    let results = &user.observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].source_call_id.as_deref(),
        Some("foreign-replayed-id")
    );
    assert_eq!(results[0].content, Some(json!("Synthetic result marker")));
}
