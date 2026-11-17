//! Codex result timestamps must reach the shared performance consumer.
use agentsight_opt::{AtifTrajectory, perf};
use agentsight_trajectory_collector::codex::convert_codex_events;
use serde_json::{Value, json};

fn event(kind: &str, timestamp: &str, payload: Value) -> Value {
    json!({"type":kind,"timestamp":timestamp,"payload":payload})
}

fn call(kind: &str, id: &str) -> Value {
    event(
        "response_item",
        "2026-11-12T00:00:00Z",
        json!({
            "type":kind,"call_id":id,"id":id,"name":"shell",
            "arguments":"{\"command\":\"sleep 5\"}",
            "action":{"type":"exec","command":["sleep","5"]}
        }),
    )
}

fn output(kind: &str, id: &str, timestamp: &str) -> Value {
    event(
        "response_item",
        timestamp,
        json!({"type":kind,"call_id":id,"output":"done"}),
    )
}

fn analyze(events: Vec<Value>) -> (Value, agentsight_opt::types::PerfStats) {
    let document = convert_codex_events(&events, "codex").unwrap();
    let json = serde_json::to_value(document).unwrap();
    let trajectory = AtifTrajectory::from_json(&json.to_string()).unwrap();
    (json, perf::compute_stats(&trajectory).unwrap())
}

#[test]
fn collected_session_metadata_retains_the_terminal_result_time() {
    use agentsight_trajectory_collector::{
        CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
    };
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let events = [
        call("function_call", "job"),
        output("function_call_output", "job", "2026-11-12T00:00:05Z"),
    ];
    std::fs::write(
        sessions.join("fixture.jsonl"),
        events
            .iter()
            .map(|event| format!("{event}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&temp.path().join("collected.db")).unwrap();
    scan_once(
        &store,
        &CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![sessions]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        },
    );
    let summaries = store.list_summaries(None, None, None, 10).unwrap();
    assert_eq!(summaries.len(), 1);
    let row = store.get(&summaries[0].session_id).unwrap().unwrap();
    assert_eq!(row.end_time.as_deref(), Some("2026-11-12T00:00:05Z"));
    let stats = perf::compute_stats(&AtifTrajectory::from_json(&row.atif_json).unwrap()).unwrap();
    assert_eq!(stats.wall_secs, 5.0);
    assert_eq!(stats.tool_secs, 5.0);
}

#[test]
fn terminal_results_preserve_elapsed_time_for_each_call_shape() {
    for (request, response) in [
        ("function_call", "function_call_output"),
        ("custom_tool_call", "custom_tool_call_output"),
        ("local_shell_call", "local_shell_call_output"),
    ] {
        let (doc, stats) = analyze(vec![
            call(request, "job"),
            output(response, "job", "2026-11-12T00:00:05Z"),
        ]);
        assert_eq!(stats.wall_secs, 5.0, "{request}: {doc}");
        assert_eq!(stats.tool_secs, 5.0, "{request}");
    }
}

#[test]
fn a_late_result_updates_its_original_call() {
    let (doc, stats) = analyze(vec![
        call("function_call", "job"),
        event(
            "event_msg",
            "2026-11-12T00:00:02Z",
            json!({"type":"user_message","message":"Continue when ready"}),
        ),
        output("function_call_output", "job", "2026-11-12T00:00:05Z"),
    ]);
    assert_eq!(
        doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"],
        "2026-11-12T00:00:05Z"
    );
    assert_eq!(doc["steps"].as_array().unwrap().len(), 2);
    assert_eq!(stats.tool_secs, 5.0);
    assert_eq!(stats.wall_secs, 5.0);
}

#[test]
fn parallel_results_keep_individual_completion_times() {
    let (doc, stats) = analyze(vec![
        call("function_call", "slow"),
        call("function_call", "fast"),
        output("function_call_output", "fast", "2026-11-12T00:00:02Z"),
        output("function_call_output", "slow", "2026-11-12T00:00:05Z"),
    ]);
    assert_eq!(
        doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"],
        "2026-11-12T00:00:05Z"
    );
    assert_eq!(
        doc["steps"][0]["tool_calls"][1]["extra"]["result_timestamp"],
        "2026-11-12T00:00:02Z"
    );
    assert_eq!(stats.tool_secs, 5.0);
}

#[test]
fn unknown_and_invalid_result_times_do_not_extend_known_calls() {
    for timestamp in ["invalid", "2026-11-11T23:59:59Z"] {
        let (_, stats) = analyze(vec![
            call("function_call", "job"),
            output("function_call_output", "job", timestamp),
        ]);
        assert_eq!(stats.tool_secs, 0.0);
        assert_eq!(stats.wall_secs, 0.0);
    }
    let (doc, _) = analyze(vec![
        call("function_call", "job"),
        output("function_call_output", "unknown", "2026-11-12T00:00:05Z"),
    ]);
    assert!(doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"].is_null());
}
