//! Grouped agent steps distinguish content completion from tool dispatch and results.
use agentsight_opt::{perf, AtifTrajectory};
use serde_json::{json, Value};

fn ts(second: u32) -> String {
    format!("2026-01-01T00:{:02}:{:02}Z", second / 60, second % 60)
}

fn grouped(start: u32, end: u32) -> Value {
    json!({"source":"agent","timestamp":ts(start),"message":"answer","extra":{"end_timestamp":ts(end)}})
}

fn call(start: u32, end: u32) -> Value {
    json!({"function_name":"shell","extra":{"start_timestamp":ts(start),"result_timestamp":ts(end)}})
}

fn trajectory(mut steps: Vec<Value>) -> AtifTrajectory {
    for (i, step) in steps.iter_mut().enumerate() {
        step["step_id"] = json!(i + 1);
    }
    serde_json::from_value(json!({"schema_version":"ATIF-v1.7","steps":steps})).unwrap()
}

#[test]
fn grouped_tool_wall_time_includes_both_results_and_later_answers() {
    for (end, wall, idle) in [(10, 13.0, 8.0), (20, 20.0, 15.0)] {
        let mut step = grouped(0, end);
        step["tool_calls"] = json!([call(0, 2), call(10, 13)]);
        let stats = perf::compute_stats(&trajectory(vec![step])).unwrap();
        assert_eq!(stats.wall_secs, wall);
        assert_eq!(stats.tool_secs, 5.0);
        assert_eq!(stats.model_secs, 0.0);
        assert_eq!(stats.idle_secs, idle);
    }
}

#[test]
fn first_toolless_step_produces_a_model_turn() {
    let candidates = perf::extract_perf_candidates(&trajectory(vec![grouped(1, 4)])).unwrap();
    assert_eq!(candidates.wall_secs, 3.0);
    assert_eq!(candidates.model_secs, 3.0);
    assert_eq!(candidates.idle_secs, 0.0);
    assert_eq!(candidates.model_turn_secs, [3.0]);
}

#[test]
fn user_idle_starts_at_answer_completion() {
    for delay in [2, 5, 70] {
        let user = json!({"source":"user","message":"next","timestamp":ts(71 + delay)});
        let stats = perf::compute_stats(&trajectory(vec![grouped(1, 71), user])).unwrap();
        assert_eq!(stats.model_secs, 70.0);
        assert_eq!(stats.frag_idle_secs, if delay == 5 { 5.0 } else { 0.0 });
        if delay == 70 {
            assert_eq!(stats.idle_gaps.len(), 1);
            let gap = &stats.idle_gaps[0];
            assert_eq!((gap.start, gap.end, gap.dur), (70.0, 140.0, 70.0));
        } else {
            assert!(stats.idle_gaps.is_empty());
        }
    }
}

#[test]
fn following_agent_turn_starts_after_prior_activity_and_tools() {
    for (calls, turns) in [
        (json!([]), vec![10.0, 5.0]),
        (json!([call(0, 4)]), vec![5.0]),
        (json!([call(0, 12)]), vec![3.0]),
        (json!([call(0, 4), {}]), vec![]),
    ] {
        let mut first = grouped(0, 10);
        first["tool_calls"] = calls;
        let next = json!({"source":"agent","timestamp":ts(15)});
        let candidates = perf::extract_perf_candidates(&trajectory(vec![first, next])).unwrap();
        assert_eq!(candidates.model_turn_secs, turns);
    }
}

#[test]
fn invalid_and_inapplicable_end_times_do_not_extend_activity() {
    for (source, copied, end) in [
        ("agent", false, json!(ts(0))),
        ("agent", false, json!("invalid")),
        ("agent", false, json!(12)),
        ("user", false, json!(ts(12))),
        ("system", false, json!(ts(12))),
        ("agent", true, json!(ts(12))),
    ] {
        let step = json!({"source":source,"is_copied_context":copied,
                          "timestamp":ts(1),"extra":{"end_timestamp":end}});
        let anchor = json!({"source":"user","timestamp":ts(1)});
        let doc = trajectory(vec![anchor, step]);
        assert_eq!(doc.origin_ts(), doc.steps[0].end_ts());
        assert_eq!(doc.last_ts(), doc.origin_ts());
    }
}

#[test]
fn completion_alone_sets_a_known_boundary_without_inventing_duration() {
    for timestamp in [Value::Null, json!("invalid")] {
        let mut first = grouped(0, 10);
        first["timestamp"] = timestamp;
        let doc = trajectory(vec![first, json!({"source":"agent","timestamp":ts(15)})]);
        assert_eq!(
            doc.origin_ts().unwrap(),
            ts(10).parse::<chrono::DateTime<chrono::Utc>>().unwrap()
        );
        let candidates = perf::extract_perf_candidates(&doc).unwrap();
        assert_eq!(candidates.wall_secs, 5.0);
        assert_eq!(candidates.model_turn_secs, [5.0]);
    }
}

#[test]
fn untimed_answer_cannot_anchor_user_idle() {
    let steps = vec![
        json!({"source":"user","timestamp":ts(0),"message":"question"}),
        json!({"source":"agent","message":"answer","extra":{"end_timestamp":"invalid"}}),
        json!({"source":"user","timestamp":ts(90),"message":"follow-up"}),
    ];
    let stats = perf::compute_stats(&trajectory(steps)).unwrap();
    assert!(stats.idle_gaps.is_empty());
    assert_eq!(stats.frag_idle_secs, 0.0);
}
