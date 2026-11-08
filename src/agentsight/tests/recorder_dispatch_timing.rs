//! The recorder knows when delegated analyses finish; exports must retain it.
use agentsight_opt::{
    AtifTrajectory,
    llm::{ChatMessage, RecordParams, TrajectoryRecorder},
    perf::compute_stats,
};

fn record(recorder: &TrajectoryRecorder, label: &str, start: &str, end: &str) {
    recorder.record(RecordParams {
        messages: &[ChatMessage::user("Inspect this fixture")],
        response: "done",
        reasoning: None,
        model: "fixture",
        input_tokens: 10,
        output_tokens: 2,
        cached_tokens: 0,
        reasoning_tokens: 0,
        start_ts: start,
        end_ts: end,
        label: Some(label),
    });
}

#[test]
fn a_single_delegated_analysis_keeps_its_recorded_duration() {
    let recorder = TrajectoryRecorder::new("fixture", "one-delegation");
    record(
        &recorder,
        "accuracy:review",
        "2026-11-08T00:00:00Z",
        "2026-11-08T00:01:00Z",
    );
    let document = recorder.to_atif();
    let stats = compute_stats(
        &AtifTrajectory::from_json(&serde_json::to_string(&document).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(stats.wall_secs, 60.0);
    assert_eq!(stats.tool_calls[0].dur, 60.0);
    assert_eq!(stats.tool_secs, 60.0);
    assert_eq!(
        document.final_metrics.unwrap().total_prompt_tokens,
        Some(10)
    );
}

#[test]
fn overlapping_labels_count_the_union_and_the_last_retry() {
    let recorder = TrajectoryRecorder::new("fixture", "parallel-analysis");
    // The shorter call completes first, so recording order differs from start order.
    record(
        &recorder,
        "perf:fast_tool",
        "2026-11-08T00:00:05Z",
        "2026-11-08T00:00:20Z",
    );
    record(
        &recorder,
        "perf:prefix_cache",
        "2026-11-08T00:00:00Z",
        "2026-11-08T00:00:40Z",
    );
    record(
        &recorder,
        "perf:prefix_cache",
        "2026-11-08T00:00:41Z",
        "2026-11-08T00:01:00Z",
    );
    let document = recorder.to_atif();
    let stats = compute_stats(
        &AtifTrajectory::from_json(&serde_json::to_string(&document).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(stats.wall_secs, 60.0);
    assert_eq!(stats.tool_secs, 60.0);
    assert_eq!(
        stats
            .tool_calls
            .iter()
            .map(|call| call.dur)
            .collect::<Vec<_>>(),
        [15.0, 60.0]
    );
    assert_eq!(document.subagent_trajectories.unwrap().len(), 2);
}

#[test]
fn completion_order_and_offsets_do_not_shorten_a_label_group() {
    let recorder = TrajectoryRecorder::new("fixture", "out-of-order");
    record(
        &recorder,
        "review",
        "2026-11-08T08:00:10+08:00",
        "2026-11-08T08:00:20+08:00",
    );
    record(
        &recorder,
        "review",
        "2026-11-08T00:00:00Z",
        "2026-11-08T00:01:00Z",
    );
    let trajectory = AtifTrajectory::from_json(&recorder.to_atif_json().unwrap()).unwrap();
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(stats.wall_secs, 60.0);
    assert_eq!(stats.tool_calls.len(), 1);
    assert_eq!(stats.tool_calls[0].dur, 60.0);
}

#[test]
fn invalid_completion_does_not_fabricate_a_negative_duration() {
    for end in ["invalid", "2026-11-07T23:00:00Z"] {
        let recorder = TrajectoryRecorder::new("fixture", "invalid-time");
        record(&recorder, "review", "2026-11-08T00:00:00Z", end);
        let document = recorder.to_atif();
        assert!(
            document.steps[0].tool_calls.as_ref().unwrap()[0]
                .extra
                .is_none()
        );
    }
}
