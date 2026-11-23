//! Wall-clock model time counts overlapping inference windows only once.

use agentsight_opt::perf::extract_perf_candidates;
use agentsight_opt::{AnalyzePipeline, AtifTrajectory};
use serde_json::{json, Value};

fn agent(id: usize, start: &str, end: &str) -> Value {
    json!({"step_id":id,"source":"agent","message":"response",
           "timestamp":format!("2026-01-01T00:00:{end}Z"),
           "extra":{"start_timestamp":format!("2026-01-01T00:00:{start}Z")}})
}

fn trajectory(steps: Vec<Value>) -> AtifTrajectory {
    AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","steps":steps}).to_string())
        .unwrap()
}

#[test]
fn overlapping_requests_do_not_exceed_wall_time() {
    let input = trajectory(vec![agent(1, "00", "10"), agent(2, "05", "15")]);
    let stats = AnalyzePipeline::run_perf(&input).unwrap();
    assert_eq!(stats.wall_secs, 15.0);
    assert_eq!(stats.model_secs, 15.0);
    assert_eq!(stats.idle_secs, 0.0);
}

#[test]
fn nested_requests_are_counted_once_even_when_completion_order_differs() {
    let input = trajectory(vec![agent(1, "05", "10"), agent(2, "00", "20")]);
    let stats = AnalyzePipeline::run_perf(&input).unwrap();
    assert_eq!(stats.model_secs, 20.0);
    assert_eq!(stats.wall_secs, 20.0);
}

#[test]
fn sequential_requests_preserve_idle_gaps() {
    let input = trajectory(vec![agent(1, "00", "05"), agent(2, "10", "15")]);
    let stats = AnalyzePipeline::run_perf(&input).unwrap();
    assert_eq!(stats.model_secs, 10.0);
    assert_eq!(stats.wall_secs, 15.0);
    assert_eq!(stats.idle_secs, 5.0);
}

#[test]
fn per_request_durations_remain_available_to_cache_analysis() {
    let input = trajectory(vec![agent(1, "00", "10"), agent(2, "05", "15")]);
    let candidates = extract_perf_candidates(&input).unwrap();
    assert_eq!(candidates.model_secs, 15.0);
    assert_eq!(candidates.model_turn_secs, vec![10.0, 10.0]);
}

#[test]
fn duplicate_touching_and_invalid_windows_preserve_the_busy_union() {
    let input = trajectory(vec![
        agent(1, "00", "05"),
        agent(2, "00", "05"),
        agent(3, "05", "10"),
        agent(4, "12", "12"),
        agent(5, "14", "13"),
    ]);
    let stats = AnalyzePipeline::run_perf(&input).unwrap();
    assert_eq!(stats.model_secs, 10.0);
    assert_eq!(
        extract_perf_candidates(&input).unwrap().model_turn_secs,
        vec![5.0, 5.0, 5.0]
    );
}
