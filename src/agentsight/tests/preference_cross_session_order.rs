use agentsight::preferences::api::{llm_input_turns, select_unique_turns};
use agentsight::preferences::trajectory_source::load_window_rows;
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::json;
use std::fs;

#[test]
fn interleaved_sessions_keep_the_newest_turn_at_the_limit() {
    let sandbox = tempfile::tempdir().unwrap();
    let sessions = sandbox.path().join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let write = |id: &str, messages: &[(&str, &str)]| {
        let mut events = vec![json!({"type":"session_meta","payload":{"id":id}})];
        events.extend(messages.iter().map(|(timestamp, message)| {
            json!({
                "type":"event_msg", "timestamp":timestamp,
                "payload":{"type":"user_message","message":message}
            })
        }));
        fs::write(
            sessions.join(format!("rollout-{id}.jsonl")),
            events
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    };
    write(
        "a",
        &[
            ("2026-11-01T10:00:00Z", "old a"),
            ("2026-11-01T12:00:00Z", "newest a"),
        ],
    );
    let db = sandbox.path().join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&db).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![sessions.clone()]),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 0,
        },
    };
    scan_once(&store, &config);
    // Session B is discovered later even though its turns interleave with A.
    write(
        "b",
        &[
            ("2026-11-01T09:00:00Z", "oldest b"),
            ("2026-11-01T11:00:00Z", "middle b"),
        ],
    );
    scan_once(&store, &config);
    let rows = load_window_rows(&store, 0).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        select_unique_turns(rows.iter(), Some(2)),
        ["newest a", "middle b"]
    );
    assert_eq!(
        llm_input_turns(rows.iter()),
        ["oldest b", "old a", "middle b", "newest a"]
    );
    // Each row retains its provenance after global ordering.
    let newest = rows
        .iter()
        .find(|r| r.user_text.as_deref() == Some("newest a"))
        .unwrap();
    assert_eq!(newest.session_id.as_deref(), Some("rollout-a"));
    write("c", &[("", "undated first"), ("invalid", "undated last")]);
    scan_once(&store, &config);
    let rows = load_window_rows(&store, 0).unwrap();
    assert_eq!(
        select_unique_turns(rows.iter(), Some(2)),
        ["newest a", "middle b"]
    );
    let unknown: Vec<_> = rows
        .iter()
        .filter(|row| row.timestamp_ns.is_none())
        .map(|row| row.user_text.as_deref().unwrap())
        .collect();
    assert_eq!(unknown, ["undated last", "undated first"]);
    assert_eq!(rows.len(), 6);
}
