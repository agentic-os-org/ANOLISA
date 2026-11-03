//! Optional user timestamps retain the documented recorded-request fallback.

use agentsight_opt::{atif::AtifTrajectory, perf::compute_stats};
use serde_json::{json, Value};

fn trajectory(user_time: Option<&str>, request_start: Option<&str>) -> AtifTrajectory {
    let mut user = json!({"step_id": 2, "source": "user", "message": "next fixture question"});
    if let Some(timestamp) = user_time {
        user["timestamp"] = json!(timestamp);
    }
    let mut extra = json!({});
    if let Some(timestamp) = request_start {
        extra["start_timestamp"] = json!(timestamp);
    }
    let document = json!({
        "schema_version": "ATIF-v1.7",
        "session_id": "fixture-session",
        "agent": {"name": "fixture-agent", "version": "1"},
        "steps": [
            {"step_id":1,"source":"agent","message":"fixture response complete",
             "timestamp":"2026-10-12T00:00:01Z",
             "extra":{"start_timestamp":"2026-10-12T00:00:00Z"}},
            user,
            {"step_id":3,"source":"agent","message":"fixture next response",
             "timestamp":"2026-10-12T00:01:15Z", "extra":extra}
        ]
    });
    let shared: agentsight_atif::AtifTrajectory = serde_json::from_value(document).unwrap();
    shared.validate_step_ids().unwrap();
    AtifTrajectory::from_json(&serde_json::to_string(&shared).unwrap()).unwrap()
}

#[test]
fn missing_user_time_uses_recorded_next_request_start_for_long_idle() {
    let stats = compute_stats(&trajectory(None, Some("2026-10-12T00:01:11Z"))).unwrap();
    assert_eq!(stats.idle_gaps.len(), 1);
    assert_eq!(stats.idle_gaps[0].dur, 70.0);
    assert_eq!(stats.idle_gaps[0].start, 1.0);
    assert_eq!(stats.idle_gaps[0].end, 71.0);
    assert_eq!(stats.model_secs, 5.0);
}

#[test]
fn recorded_user_time_takes_precedence_over_later_request_start() {
    let stats = compute_stats(&trajectory(
        Some("2026-10-12T00:01:06Z"),
        Some("2026-10-12T00:01:11Z"),
    ))
    .unwrap();
    assert_eq!(stats.idle_gaps[0].dur, 65.0);
}

#[test]
fn missing_both_times_does_not_invent_idle_from_response_end() {
    let stats = compute_stats(&trajectory(None, None)).unwrap();
    assert!(stats.idle_gaps.is_empty());
    assert_eq!(stats.frag_idle_secs, 0.0);
}

#[test]
fn no_next_request_does_not_invent_an_idle_endpoint() {
    let mut data: Value = serde_json::from_str(
        &serde_json::to_string(&trajectory(None, Some("2026-10-12T00:01:11Z"))).unwrap(),
    )
    .unwrap();
    data["steps"].as_array_mut().unwrap().pop();
    let stats = compute_stats(&AtifTrajectory::from_json(&data.to_string()).unwrap()).unwrap();
    assert!(stats.idle_gaps.is_empty());
}

#[test]
fn first_request_without_a_start_does_not_borrow_a_later_request() {
    let mut data: Value =
        serde_json::from_str(&serde_json::to_string(&trajectory(None, None)).unwrap()).unwrap();
    data["steps"].as_array_mut().unwrap().push(json!({
        "step_id":4,"source":"agent","message":"later fixture response",
        "timestamp":"2026-10-12T00:03:05Z",
        "extra":{"start_timestamp":"2026-10-12T00:03:00Z"}
    }));
    let stats = compute_stats(&AtifTrajectory::from_json(&data.to_string()).unwrap()).unwrap();
    assert!(stats.idle_gaps.is_empty());
}

#[test]
fn recorded_reversed_user_time_is_not_replaced_by_request_start() {
    let stats = compute_stats(&trajectory(
        Some("2026-10-12T00:00:00Z"),
        Some("2026-10-12T00:01:11Z"),
    ))
    .unwrap();
    assert!(stats.idle_gaps.is_empty());
}
