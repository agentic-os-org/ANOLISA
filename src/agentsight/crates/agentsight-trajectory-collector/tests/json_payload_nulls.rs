//! Persist recorded JSON arguments without changing explicit null values.

use agentsight_trajectory_collector::{
    scan_once, CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore,
};
use serde_json::{json, Value};

#[test]
fn collected_tool_arguments_keep_explicit_null_members() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "anolisa-json-payload-nulls-{}-{nonce}",
        std::process::id()
    ));
    let projects = root.join(".claude/projects");
    let project = projects.join("-data-fixture");
    std::fs::create_dir_all(&project).unwrap();
    let session = "11111111-2222-4333-8444-555555555555";
    let arguments = json!({"patch":{"optional_setting":null},"items":[{"value":null},null]});
    let events = [
        json!({"type":"user","message":{"role":"user","content":"fixture request"}}),
        json!({"type":"assistant","message":{"role":"assistant","content":[{
            "type":"tool_use","id":"call-fixture","name":"UpdateConfig","input":arguments
        }]}}),
    ];
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    std::fs::write(project.join(format!("{session}.jsonl")), content).unwrap();
    let saved = {
        let store = TrajectoryStore::new_with_path(&root.join("trajectories.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![projects]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };
        scan_once(&store, &config);
        let record = store.get(session).unwrap().unwrap();
        assert_eq!(record.source, "claude-code");
        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(
            store.get(session).unwrap().unwrap().atif_json,
            record.atif_json
        );
        record.atif_json
    };
    std::fs::remove_dir_all(root).unwrap();
    let document: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        document["steps"][1]["tool_calls"][0]["arguments"],
        arguments
    );
    assert_eq!(
        document["steps"][1]["tool_calls"][0]["tool_call_id"],
        "call-fixture"
    );
    assert!(document["steps"][1].get("timestamp").is_none());
}
