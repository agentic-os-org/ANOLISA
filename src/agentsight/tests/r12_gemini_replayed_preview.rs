// Local collection is a desktop module; compile its real source on Linux too.
// Existing desktop-only legacy fallback predates the let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
pub(crate) mod converter;
// Match the desktop module path used by discovery's existing parity tests.
mod local {
    pub(crate) mod collector {
        pub(crate) use crate::converter;
    }
}
#[allow(dead_code)] // The public HOME wrapper is not exercised with real user files.
#[path = "../src/local/collector/discovery.rs"]
mod discovery;
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::json;

#[test]
fn legacy_main_and_subagent_are_collected_and_details_keep_their_steps() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join(".gemini/tmp");
    let chats = root.join("project/chats");
    let child = chats.join("parent");
    std::fs::create_dir_all(&child).unwrap();
    let document = json!({"sessionId":"record-id","projectHash":"p",
        "messages":[{"id":"u","type":"user","content":"legacy input"},
                    {"id":"a","type":"gemini","content":"legacy answer",
                     "toolCalls":[{"id":"read","name":"read_file","args":{},
                       "result":[{"functionResponse":{"response":{"paths":["file.rs"]}}}]}]}]});
    let content = format!(
        "\u{feff}{}",
        serde_json::to_string_pretty(&document).unwrap()
    );
    for path in [chats.join("session-legacy.json"), child.join("child.json")] {
        std::fs::write(&path, &content).unwrap();
        let local = converter::convert_jsonl_to_atif(&path).unwrap();
        assert_eq!(local.steps.len(), 2);
        assert_eq!(local.steps[0].message, "legacy input");
    }
    let store = TrajectoryStore::new_with_path(&directory.path().join("store.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![root]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };
    scan_once(&store, &config);
    assert_eq!(store.count().unwrap(), 2);
    for id in ["session-legacy", "parent:subagent:child"] {
        let row = store.get(id).unwrap().unwrap();
        let trajectory: serde_json::Value = serde_json::from_str(&row.atif_json).unwrap();
        assert_eq!(trajectory["steps"][0]["message"], "legacy input");
        assert_eq!(row.source, "gemini-cli");
    }
    scan_once(&store, &config);
    assert_eq!(
        store.count().unwrap(),
        2,
        "unchanged files are not duplicated"
    );
}
