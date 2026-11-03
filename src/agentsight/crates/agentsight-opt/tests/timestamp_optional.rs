use agentsight_opt::atif::AtifTrajectory;
use agentsight_opt::perf::compute_stats;
use agentsight_opt::trace::{build_inventory, collect_tool_calls};
use serde_json::{json, Value};

fn document() -> Value {
    json!({
        "schema_version":"ATIF-v1.7", "session_id":"fixture-session",
        "agent":{"name":"fixture-agent","version":"1"},
        "steps":[
            {"step_id":1,"source":"user","message":"read the fixture file and report the result"},
            {"step_id":2,"source":"agent","message":"checking file", "tool_calls":[
                {"tool_call_id":"read-one","function_name":"Read","arguments":{"file_path":"fixture-input.txt"}},
                {"tool_call_id":"write-one","function_name":"Write","arguments":{"file_path":"fixture-output.txt","content":"fixture output"}}
            ], "observation":{"results":[
                {"source_call_id":"read-one","content":"normal fixture text", "extra":{"is_error":false}},
                {"source_call_id":"write-one","content":"write was rejected", "extra":{"is_error":true}}
            ]}},
            {"step_id":3,"source":"agent","message":"fixture result"}
        ]
    })
}

fn trajectory(doc: Value) -> AtifTrajectory {
    AtifTrajectory::from_json(&doc.to_string()).unwrap()
}

#[test]
fn missing_optional_timestamps_preserve_structured_tool_inventory() {
    let document = document();
    let schema: agentsight_atif::AtifTrajectory = serde_json::from_value(document.clone()).unwrap();
    assert!(schema.steps.iter().all(|step| step.timestamp.is_none()));
    let inventory = build_inventory(&trajectory(serde_json::to_value(&schema).unwrap()));
    assert_eq!(
        inventory.tool_calls.len(),
        2,
        "missing time is not an empty tool sequence"
    );
    assert_eq!(inventory.tool_calls[0].call_id, "read-one");
    assert_eq!(
        inventory.tool_calls[0].target.as_deref(),
        Some("fixture-input.txt")
    );
    assert!(!inventory.tool_calls[0].err);
    assert!(
        inventory.tool_calls[1].err,
        "the provider's structured error flag survives"
    );
    assert!(inventory
        .tool_calls
        .iter()
        .all(|call| call.start == 0.0 && call.dur == 0.0));
    assert_eq!(inventory.user_turns.len(), 1);
    assert_eq!(inventory.final_answer, "fixture result");
}

#[test]
fn missing_optional_timestamps_keep_performance_counts_with_zero_timing() {
    let statistics = compute_stats(&trajectory(document())).unwrap();
    assert_eq!(statistics.tool_count, 2);
    assert_eq!(statistics.tool_calls.len(), 2);
    assert_eq!(statistics.top_slow.len(), 2);
    assert_eq!(statistics.wall_secs, 0.0);
    assert_eq!(statistics.model_secs, 0.0);
    assert_eq!(statistics.tool_secs, 0.0);
    assert_eq!(statistics.idle_secs, 0.0);
    assert!(statistics.idle_gaps.is_empty());
}

#[test]
fn one_available_timestamp_preserves_both_known_and_untimed_steps() {
    let mut doc = document();
    doc["steps"][0]["timestamp"] = json!("2026-10-10T00:00:00Z");
    let calls = collect_tool_calls(&trajectory(doc));
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|call| call.start == 0.0 && call.dur == 0.0));
    assert!(calls[1].err);
}

#[test]
fn fully_recorded_timing_keeps_the_existing_tool_window() {
    let mut doc = document();
    doc["steps"][0]["timestamp"] = json!("2026-10-10T00:00:00Z");
    doc["steps"][1]["timestamp"] = json!("2026-10-10T00:00:02Z");
    doc["steps"][1]["extra"] = json!({"start_timestamp":"2026-10-10T00:00:01Z"});
    doc["steps"][2]["timestamp"] = json!("2026-10-10T00:00:07Z");
    doc["steps"][2]["extra"] = json!({"start_timestamp":"2026-10-10T00:00:06Z"});
    let statistics = compute_stats(&trajectory(doc)).unwrap();
    assert_eq!(statistics.tool_count, 2);
    assert!(statistics
        .tool_calls
        .iter()
        .all(|call| call.start == 2.0 && call.dur == 2.0));
    assert_eq!(statistics.wall_secs, 7.0);
    assert_eq!(statistics.tool_secs, 4.0);
    assert_eq!(statistics.model_secs, 2.0);
    assert_eq!(statistics.idle_secs, 1.0);
}
