use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::json;
use std::fs;

#[test]
fn ui_only_usage_updates_refresh_a_finished_task_and_then_settle() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox
        .path()
        .join(".config/Code/User/globalStorage/saoudrizwan.claude-dev");
    let task = root.join("tasks/task-1");
    fs::create_dir_all(&task).unwrap();
    let api = task.join("api_conversation_history.json");
    let history = json!([
        {"role":"user","content":"inspect fixture"},
        {"role":"assistant","content":"inspection complete"}
    ])
    .to_string();
    fs::write(&api, &history).unwrap();
    let original_mtime = fs::metadata(&api).unwrap().modified().unwrap();
    let ui = task.join("ui_messages.json");
    let database = sandbox.path().join("trajectories.db");
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![root.clone()]),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 0,
        },
    };
    let store = TrajectoryStore::new_with_path(&database).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("task-1").unwrap().unwrap().total_prompt_tokens,
        None
    );
    let request = |tokens| {
        json!([{
            "ts": 1763071201000i64, "type":"say", "say":"api_req_started",
            "text":json!({"tokensIn":tokens,"tokensOut":5,"cost":0.1}).to_string()
        }])
        .to_string()
    };
    fs::write(&ui, request(10)).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("task-1").unwrap().unwrap().total_prompt_tokens,
        Some(10)
    );
    // Cline's asynchronous usage callback only saves UI/history metadata.
    fs::write(&ui, request(120)).unwrap();
    drop(store);
    let store = TrajectoryStore::new_with_path(&database).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("task-1").unwrap().unwrap().total_prompt_tokens,
        Some(120)
    );
    let previous_ui = fs::metadata(&ui).unwrap();
    fs::write(&ui, request(121)).unwrap();
    fs::File::options()
        .write(true)
        .open(&ui)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(previous_ui.modified().unwrap() + std::time::Duration::from_secs(1)),
        )
        .unwrap();
    assert_eq!(fs::metadata(&ui).unwrap().len(), previous_ui.len());
    scan_once(&store, &config);
    assert_eq!(
        store.get("task-1").unwrap().unwrap().total_prompt_tokens,
        Some(121)
    );
    let collected = store.list_summaries(None, None, None, 10).unwrap()[0].collected_at_ns;
    // Maintenance may remove redundant per-path bookkeeping. The trajectory
    // row must retain the same fingerprint, including its UI dependency.
    store
        .maintain(TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 0,
        })
        .unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.list_summaries(None, None, None, 10).unwrap()[0].collected_at_ns,
        collected
    );
    fs::remove_file(&ui).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("task-1").unwrap().unwrap().total_prompt_tokens,
        None
    );
    fs::create_dir_all(root.join("state")).unwrap();
    fs::write(
        root.join("state/taskHistory.json"),
        json!([{
            "id":"task-1", "cwdOnTaskInitialization":"/workspace/renamed-project"
        }])
        .to_string(),
    )
    .unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("task-1").unwrap().unwrap().project,
        "renamed-project"
    );
    assert_eq!(fs::read_to_string(&api).unwrap(), history);
    assert_eq!(
        fs::metadata(&api).unwrap().modified().unwrap(),
        original_mtime
    );
    assert_eq!(store.count().unwrap(), 1);
    // A database created by the previous collector stores the primary byte
    // count. It must refresh once even if neither source file changes now.
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute("DELETE FROM skipped_files", []).unwrap();
    connection
        .execute(
            "UPDATE collected_trajectories SET file_size = ?1, collected_at_ns = 1",
            [history.len() as i64],
        )
        .unwrap();
    scan_once(&store, &config);
    let refreshed = store.list_summaries(None, None, None, 10).unwrap()[0].collected_at_ns;
    assert!(refreshed > 1);
    scan_once(&store, &config);
    assert_eq!(
        store.list_summaries(None, None, None, 10).unwrap()[0].collected_at_ns,
        refreshed
    );
}
