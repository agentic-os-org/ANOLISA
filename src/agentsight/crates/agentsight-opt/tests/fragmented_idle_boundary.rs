//! Inclusive fragmented-idle bounds through the public performance API.

use agentsight_opt::{atif::AtifTrajectory, perf::compute_stats};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

fn trajectory(idle_ms: i64) -> AtifTrajectory {
    let origin = "2026-10-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let at = |ms| (origin + Duration::milliseconds(ms)).to_rfc3339();
    AtifTrajectory::from_json(
        &json!({
            "schema_version": "ATIF-v1.7", "session_id": "idle-boundary",
            "agent": {"name": "fixture", "version": "1"},
            "steps": [
                {"step_id": 1, "source": "user", "timestamp": at(0), "message": "first"},
                {"step_id": 2, "source": "agent", "timestamp": at(1000), "message": "done",
                 "extra": {"start_timestamp": at(0)}},
                {"step_id": 3, "source": "user", "timestamp": at(1000 + idle_ms), "message": "next"},
                {"step_id": 4, "source": "agent", "timestamp": at(2000 + idle_ms), "message": "done",
                 "extra": {"start_timestamp": at(1000 + idle_ms)}}
            ]
        }).to_string(),
    ).unwrap()
}

#[test]
fn exactly_three_seconds_is_fragmented_idle_in_the_serialized_report() {
    let stats = compute_stats(&trajectory(3000)).unwrap();
    assert_eq!(stats.frag_idle_secs, 3.0);
    assert_eq!(serde_json::to_value(&stats).unwrap()["frag_idle_secs"], 3.0);
    assert!(stats.idle_gaps.is_empty());
    assert_eq!(
        (stats.wall_secs, stats.model_secs, stats.idle_secs),
        (5.0, 2.0, 3.0)
    );
}

#[test]
fn below_three_seconds_stays_outside_the_fragmented_window() {
    let stats = compute_stats(&trajectory(2999)).unwrap();
    assert_eq!(stats.frag_idle_secs, 0.0);
    assert!(stats.idle_gaps.is_empty());
}

#[test]
fn exactly_sixty_seconds_stays_fragmented_instead_of_long_idle() {
    let stats = compute_stats(&trajectory(60000)).unwrap();
    assert_eq!(stats.frag_idle_secs, 60.0);
    assert!(stats.idle_gaps.is_empty());
}

#[test]
fn above_sixty_seconds_stays_in_the_long_idle_window() {
    let stats = compute_stats(&trajectory(60001)).unwrap();
    assert_eq!(stats.frag_idle_secs, 0.0);
    assert_eq!(stats.idle_gaps.len(), 1);
    assert!((stats.idle_gaps[0].dur - 60.001).abs() < 1e-9);
}
