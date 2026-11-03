//! Schema-valid informational session ids must not discard analysis input.

use agentsight_opt::{atif::AtifTrajectory, perf::compute_stats, trace::build_inventory};
use serde_json::{json, Value};

fn document() -> Value {
    json!({
        "schema_version":"ATIF-v1.7", "session_id":null,
        "agent":{"name":"fixture-agent","version":"1"},
        "steps":[
            {"step_id":1,"source":"user","timestamp":"2026-10-01T00:00:00Z","message":"read fixture text"},
            {"step_id":2,"source":"agent","timestamp":"2026-10-01T00:00:01Z","message":"reading",
             "extra":{"start_timestamp":"2026-10-01T00:00:00Z"},
             "tool_calls":[{"tool_call_id":"fixture-read","function_name":"Read","arguments":{"file_path":"/fixture/input.txt"}}],
             "observation":{"results":[{"source_call_id":"fixture-read","content":"fixture text","extra":{"is_error":false}}]}},
            {"step_id":3,"source":"agent","timestamp":"2026-10-01T00:00:03Z","message":"done",
             "extra":{"start_timestamp":"2026-10-01T00:00:02Z"}}
        ]
    })
}

fn assert_analysis(doc: Value) {
    let shared: agentsight_atif::AtifTrajectory = serde_json::from_value(doc.clone()).unwrap();
    shared.validate_step_ids().unwrap();
    let parsed = AtifTrajectory::from_json(&doc.to_string())
        .expect("an informational null session id is schema-valid");
    let inventory = build_inventory(&parsed);
    assert_eq!(inventory.tool_calls.len(), 1);
    assert_eq!(inventory.tool_calls[0].call_id, "fixture-read");
    assert_eq!(
        inventory.tool_calls[0].target.as_deref(),
        Some("/fixture/input.txt")
    );
    assert!(!inventory.tool_calls[0].err);
    let stats = compute_stats(&parsed).unwrap();
    assert_eq!(
        (
            stats.tool_count,
            stats.wall_secs,
            stats.model_secs,
            stats.tool_secs
        ),
        (1, 3.0, 2.0, 1.0)
    );
}

#[test]
fn explicit_null_session_id_keeps_the_valid_steps_and_analysis() {
    assert_analysis(document());
}

#[test]
fn omitted_session_id_keeps_the_existing_default() {
    let mut doc = document();
    doc.as_object_mut().unwrap().remove("session_id");
    assert_analysis(doc);
}

#[test]
fn named_session_id_keeps_its_value() {
    let mut doc = document();
    doc["session_id"] = json!("fixture-session");
    assert_analysis(doc.clone());
    assert_eq!(
        AtifTrajectory::from_json(&doc.to_string())
            .unwrap()
            .session_id,
        "fixture-session"
    );
}

#[test]
fn numeric_session_id_stays_invalid() {
    let mut doc = document();
    doc["session_id"] = json!(42);
    assert!(serde_json::from_value::<agentsight_atif::AtifTrajectory>(doc.clone()).is_err());
    assert!(AtifTrajectory::from_json(&doc.to_string()).is_err());
}
