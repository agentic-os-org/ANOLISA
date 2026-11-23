//! Refreshing an old session must not make its historical turns recent.
use agentsight::preferences::{
    api::{llm_input_turns, select_unique_turns},
    trajectory_source::load_window_rows,
};
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::{Value, json};

#[test]
fn resumed_session_respects_the_turn_window_at_both_consumers() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let user = |timestamp: &str, message: &str| {
        json!({"type":"event_msg", "timestamp":timestamp,
        "payload":{"type":"user_message", "message":message}})
    };
    let events = [
        json!({"type":"session_meta","payload":{"id":"resumed"}}),
        user("2026-10-01T00:00:00Z", "old instruction"),
        user("2026-11-01T00:00:00Z", "at boundary"),
        json!({"type":"response_item","timestamp":"2026-11-01T00:00:01Z","payload":{
            "type":"function_call","name":"shell","call_id":"boundary-tool","arguments":"{}"}}),
        user("2026-11-02T00:00:00Z", "recent instruction"),
        user("invalid", "undated instruction"),
    ];
    std::fs::write(
        sessions.join("rollout-resumed.jsonl"),
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&root.path().join("trajectories.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![sessions]),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 0,
        },
    };
    scan_once(&store, &config);
    let boundary = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap();
    let rows = load_window_rows(&store, boundary).unwrap();
    assert_eq!(
        select_unique_turns(rows.iter(), None),
        ["recent instruction", "at boundary", "undated instruction"]
    );
    assert_eq!(
        llm_input_turns(rows.iter()),
        ["undated instruction", "at boundary", "recent instruction"]
    );
    let at_boundary = rows
        .iter()
        .find(|row| row.user_text.as_deref() == Some("at boundary"))
        .unwrap();
    assert_eq!(at_boundary.tool_names, ["shell"]);
    assert!(rows.iter().any(|row| row.timestamp_ns.is_none()));
    assert_eq!(load_window_rows(&store, 0).unwrap().len(), 4);
    // Re-reading unchanged data must not move the old turn into the window.
    scan_once(&store, &config);
    assert_eq!(load_window_rows(&store, boundary).unwrap().len(), 3);
}
