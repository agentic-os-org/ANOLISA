#[path = "../src/local/collector/converter.rs"]
mod local_converter;
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use local_converter::convert_jsonl_to_atif;
use serde_json::json;
use std::fs;

#[test]
fn gemini_part_unions_survive_collection_and_local_conversion() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join(".gemini/tmp");
    let chats = root.join("project/chats");
    fs::create_dir_all(&chats).unwrap();
    let fixtures = [
        (
            json!(["first", {"text":"second"}, {"inlineData":{"mimeType":"image/png","data":"SECRET_BINARY"}}]),
            "first\nsecond",
        ),
        (json!({"text":"single text part"}), "single text part"),
        (json!([{ "text":"ordinary part" }]), "ordinary part"),
        (json!("ordinary string"), "ordinary string"),
    ];
    let store = TrajectoryStore::new_with_path(&sandbox.path().join("trajectories.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![root]),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 0,
        },
    };
    for (index, (content, expected)) in fixtures.into_iter().enumerate() {
        let id = format!("session-parts-{index}");
        let events = [
            json!({"sessionId":id,"projectHash":"project"}),
            json!({"id":"u","type":"user","content":content}),
            json!({"id":"a","type":"gemini","content":content}),
        ];
        let path = chats.join(format!("{id}.jsonl"));
        fs::write(
            &path,
            events
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let local = convert_jsonl_to_atif(&path).unwrap();
        assert_eq!(local.steps.len(), 2);
        assert_eq!(local.steps[0].message, expected);
        assert_eq!(local.steps[1].message, expected);
        scan_once(&store, &config);
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.first_user_message.as_deref(), Some(expected));
        assert_eq!(row.num_steps, 2);
        assert!(!row.atif_json.contains("SECRET_BINARY"));
    }
}
