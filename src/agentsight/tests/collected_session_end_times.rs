//! Session-list metadata must agree with the recorded trajectory's activity.
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::{Value, json};

fn collect(events: Vec<Value>) -> agentsight_trajectory_collector::TrajectoryRecord {
    let root = tempfile::tempdir().unwrap();
    let projects = root.path().join(".claude/projects");
    let project = projects.join("fixture-project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("11111111-2222-3333-4444-555555555555.jsonl"),
        events
            .iter()
            .map(|event| format!("{event}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&root.path().join("collection.db")).unwrap();
    scan_once(
        &store,
        &CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![projects]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        },
    );
    store
        .get("11111111-2222-3333-4444-555555555555")
        .unwrap()
        .unwrap()
}

fn call() -> Value {
    json!({"type":"assistant","timestamp":"2026-11-08T00:00:01Z","message":{"content":[
        {"type":"tool_use","id":"slow","name":"Bash","input":{"command":"run job"}},
        {"type":"tool_use","id":"fast","name":"Read","input":{"path":"file"}}
    ]}})
}
fn result(id: &str, timestamp: &str) -> Value {
    json!({"type":"user","timestamp":timestamp,"message":{"content":[{"type":"tool_result","tool_use_id":id,"content":"done"}]}})
}

#[test]
fn terminal_tool_result_is_the_session_last_activity() {
    let row = collect(vec![call(), result("slow", "2026-11-08T00:01:01Z")]);
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:01:01Z"));
    let trajectory = agentsight_opt::AtifTrajectory::from_json(&row.atif_json).unwrap();
    assert_eq!(
        agentsight_opt::perf::compute_stats(&trajectory)
            .unwrap()
            .wall_secs,
        60.0
    );
}

#[test]
fn completion_times_use_chronological_order_across_offsets() {
    let row = collect(vec![
        call(),
        result("slow", "2026-11-08T00:01:01Z"),
        result("fast", "2026-11-08T08:00:04+08:00"),
    ]);
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:01:01Z"));
}

#[test]
fn a_later_answer_remains_the_last_activity() {
    let row = collect(vec![
        call(),
        result("slow", "2026-11-08T00:01:01Z"),
        json!({"type":"assistant","timestamp":"2026-11-08T00:01:05Z","message":{"content":"Finished"}}),
    ]);
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:01:05Z"));
}

#[test]
fn invalid_result_times_cannot_move_activity_backwards() {
    for timestamp in ["invalid", "2026-11-07T23:00:00Z"] {
        let row = collect(vec![call(), result("slow", timestamp)]);
        assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:00:01Z"));
    }
}

#[test]
fn known_completion_survives_an_untimed_issuing_step() {
    let mut issuing = call();
    issuing.as_object_mut().unwrap().remove("timestamp");
    let row = collect(vec![issuing, result("slow", "2026-11-08T00:01:01Z")]);
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:01:01Z"));
    assert!(
        row.start_time.is_none(),
        "a known finish must not invent a start"
    );
}
