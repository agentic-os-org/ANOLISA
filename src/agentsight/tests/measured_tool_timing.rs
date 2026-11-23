#![cfg(feature = "server")]

use agentsight_opt::{
    atif::AtifTrajectory,
    perf::{compute_stats, extract_perf_candidates},
    trace::build_inventory,
};
use agentsight_trajectory_collector::atif::convert_qoder_events;
use serde_json::{Value, json};

fn converted(parallel: bool) -> AtifTrajectory {
    let mut calls =
        vec![json!({"type":"tool_use","id":"a","name":"Bash","input":{"command":"slow"}})];
    if parallel {
        calls.push(json!({"type":"tool_use","id":"b","name":"Read","input":{"path":"file"}}));
    }
    let mut events = vec![
        json!({"type":"user","timestamp":"2026-10-01T00:00:00Z","message":{"content":"Run the tools"}}),
        json!({"type":"assistant","timestamp":"2026-10-01T00:00:01Z","message":{"content":calls}}),
    ];
    if parallel {
        events.push(json!({"type":"user","timestamp":"2026-10-01T00:00:04Z","message":{"content":[{"type":"tool_result","tool_use_id":"b","content":"file"}]}}));
    }
    events.push(json!({"type":"user","timestamp":"2026-10-01T00:00:08Z","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"done"}]}}));
    let exported = convert_qoder_events(&events, "claude-code")
        .unwrap()
        .to_json_value()
        .unwrap();
    AtifTrajectory::from_json(&exported.to_string()).unwrap()
}

#[test]
fn terminal_result_preserves_measured_tool_and_session_duration() {
    let trajectory = converted(false);
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(stats.wall_secs, 8.0);
    assert_eq!(stats.tool_secs, 7.0);
    assert_eq!(stats.tool_calls[0].dur, 7.0);
    assert_eq!(build_inventory(&trajectory).tool_calls[0].dur, 7.0);
    assert_eq!(
        extract_perf_candidates(&trajectory).unwrap().top_tools[0].dur,
        7.0
    );
}

#[test]
fn parallel_result_times_keep_individual_durations_without_double_counting_wall_time() {
    let trajectory = converted(true);
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(stats.tool_calls[0].dur, 7.0);
    assert_eq!(stats.tool_calls[1].dur, 3.0);
    assert_eq!(stats.tool_secs, 7.0);
    assert_eq!(stats.wall_secs, 8.0);
    assert_eq!(stats.top_slow[0].call_id, "a");
    assert_eq!(stats.model_secs, 1.0);
    assert_eq!(stats.idle_secs, 0.0);
}

fn manual(result: Value) -> AtifTrajectory {
    AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","steps":[
        {"step_id":1,"source":"agent","timestamp":"2026-10-01T00:00:01Z",
            "tool_calls":[{"tool_call_id":"a","function_name":"Bash","arguments":{},"extra":{"result_timestamp":result}}]},
        {"step_id":2,"source":"agent","timestamp":"2026-10-01T00:00:12Z",
            "extra":{"start_timestamp":"2026-10-01T00:00:10Z"},"message":"Done"}
    ]}).to_string()).unwrap()
}

#[test]
fn measured_result_excludes_delay_before_the_next_request() {
    let stats = compute_stats(&manual(json!("2026-10-01T00:00:04Z"))).unwrap();
    assert_eq!(stats.tool_secs, 3.0);
    assert_eq!(stats.tool_calls[0].dur, 3.0);
    assert_eq!(stats.idle_secs, 6.0);
}

#[test]
fn absent_invalid_and_reversed_result_times_keep_legacy_estimation() {
    for result in [Value::Null, json!("bad"), json!("2026-09-30T23:59:59Z")] {
        let stats = compute_stats(&manual(result)).unwrap();
        assert_eq!(stats.tool_secs, 9.0);
        assert_eq!(stats.tool_calls[0].dur, 9.0);
        assert_eq!(stats.wall_secs, 11.0);
    }
    let mut raw = serde_json::to_value(manual(Value::Null)).unwrap();
    raw["steps"][0]["tool_calls"][0]
        .as_object_mut()
        .unwrap()
        .remove("extra");
    let trajectory = AtifTrajectory::from_json(&raw.to_string()).unwrap();
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(stats.tool_secs, 9.0);
    assert_eq!(stats.tool_calls[0].dur, 9.0);
}

#[test]
fn an_instantaneous_recorded_result_does_not_gain_an_estimated_duration() {
    let stats = compute_stats(&manual(json!("2026-10-01T00:00:01Z"))).unwrap();
    assert_eq!(stats.tool_secs, 0.0);
    assert_eq!(stats.tool_calls[0].dur, 0.0);
}

#[test]
fn completed_tools_bound_model_fallback_when_request_start_is_missing() {
    let mut trajectory = manual(json!("2026-10-01T00:00:04Z"));
    trajectory.steps[1].extra = None;
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(stats.tool_secs, 3.0);
    assert_eq!(stats.model_secs, 8.0);
    assert_eq!(stats.idle_secs, 0.0);
}

#[test]
fn overlapping_tool_windows_across_steps_are_not_added_twice() {
    let trajectory = AtifTrajectory::from_json(
        &json!({"schema_version":"ATIF-v1.7","steps":[
            {"step_id":1,"source":"agent","timestamp":"2026-10-01T00:00:01Z",
                "tool_calls":[{"tool_call_id":"a","function_name":"Bash","arguments":{},
                    "extra":{"result_timestamp":"2026-10-01T00:00:08Z"}}]},
            {"step_id":2,"source":"agent","timestamp":"2026-10-01T00:00:02Z",
                "tool_calls":[{"tool_call_id":"b","function_name":"Read","arguments":{},
                    "extra":{"result_timestamp":"2026-10-01T00:00:05Z"}}]}
        ]})
        .to_string(),
    )
    .unwrap();
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(
        stats.tool_calls.iter().map(|call| call.dur).sum::<f64>(),
        10.0
    );
    assert_eq!(stats.tool_secs, 7.0);
    assert_eq!(stats.wall_secs, 7.0);
}
