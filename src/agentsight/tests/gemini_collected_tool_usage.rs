//! Gemini's recorded tool-input usage must survive the local ATIF pipeline.
use agentsight_opt::{AtifTrajectory, cost::compute_cost};
use agentsight_trajectory_collector::gemini::convert_gemini_events;
use serde_json::{Value, json};

fn check(tokens: Value, expected: Option<u64>) {
    let doc = convert_gemini_events(
        &[
            json!({"sessionId":"fixture", "projectHash":"project"}),
            json!({"id":"answer", "type":"gemini", "content":"done", "tokens":tokens}),
        ],
        "gemini-cli",
    )
    .unwrap();
    let metrics = doc.steps[0].metrics.as_ref().unwrap();
    assert_eq!(metrics.prompt_tokens, expected);
    assert_eq!(
        doc.final_metrics.as_ref().unwrap().total_prompt_tokens,
        expected.filter(|n| *n > 0)
    );
    let parsed = AtifTrajectory::from_json(&doc.to_json_value().unwrap().to_string()).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    if let Some(count) = expected {
        assert_eq!(cost.total_real_input_tok as u64, count);
    }
}

#[test]
fn tool_inputs_reach_the_optimizer_without_double_counting_cached_input() {
    check(
        json!({"input":100, "tool":40, "cached":20, "output":8, "thoughts":2, "total":150}),
        Some(140),
    );
}

#[test]
fn tool_only_measurements_are_not_dropped() {
    check(json!({"tool":40,"output":2}), Some(40));
}

#[test]
fn absent_and_invalid_tool_usage_keep_legacy_input_counts() {
    for tool in [Value::Null, json!(-1), json!("40"), json!(0)] {
        check(json!({"input":100,"tool":tool,"output":2}), Some(100));
    }
    check(json!({"input":100,"output":2}), Some(100));
}

#[test]
fn replayed_message_replaces_the_whole_usage_measurement() {
    let doc = convert_gemini_events(
        &[
            json!({"id":"a","type":"gemini","content":"done","tokens":{"input":100,"tool":20}}),
            json!({"id":"a","type":"gemini","content":"done","tokens":{"input":100,"tool":40}}),
        ],
        "gemini-cli",
    )
    .unwrap();
    assert_eq!(doc.steps.len(), 1);
    assert_eq!(doc.final_metrics.unwrap().total_prompt_tokens, Some(140));
}

#[test]
fn collection_persists_the_corrected_summary_and_analysis_input() {
    use agentsight_trajectory_collector::{
        CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
    };
    let root = tempfile::tempdir().unwrap();
    let scan = root.path().join(".gemini/tmp");
    let chats = scan.join("project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    let events = [
        json!({"sessionId":"usage","projectHash":"project"}),
        json!({"id":"u","type":"user","content":"Inspect files"}),
        json!({"id":"a","type":"gemini","content":"done","tokens":{"input":100,"tool":40,"cached":20,"output":10}}),
    ];
    std::fs::write(
        chats.join("session-usage.jsonl"),
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&root.path().join("collection.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![scan]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };
    scan_once(&store, &config);
    let row = store.get("session-usage").unwrap().unwrap();
    assert_eq!(row.total_prompt_tokens, Some(140));
    let parsed = AtifTrajectory::from_json(&row.atif_json).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.total_real_input_tok, 140);
    assert_eq!(cost.total_real_cached_tok, 20);
    scan_once(&store, &config);
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(
        store
            .get("session-usage")
            .unwrap()
            .unwrap()
            .total_prompt_tokens,
        Some(140)
    );
    // Recreate the previous collector's persisted state without touching the
    // input file: upgrading must also correct completed, unchanged sessions.
    use sha2::{Digest, Sha256};
    let mut old = store.get("session-usage").unwrap().unwrap();
    let bytes = std::fs::metadata(&old.file_path).unwrap().len() as i64;
    let mut digest = Sha256::new();
    digest.update(b"agentsight-gemini-project-v1\0");
    digest.update(bytes.to_le_bytes());
    digest.update(old.file_mtime_ns.to_le_bytes());
    digest.update(old.project.as_bytes());
    let hash = digest.finalize();
    old.file_size = i64::from_le_bytes(hash[..8].try_into().unwrap()) | i64::MIN;
    old.total_prompt_tokens = Some(100);
    let mut previous: Value = serde_json::from_str(&old.atif_json).unwrap();
    previous["steps"][1]["metrics"]["prompt_tokens"] = json!(100);
    previous["final_metrics"]["total_prompt_tokens"] = json!(100);
    old.atif_json = previous.to_string();
    store.upsert_trajectory(&old).unwrap();
    scan_once(&store, &config);
    let refreshed = store.get("session-usage").unwrap().unwrap();
    assert_eq!(refreshed.total_prompt_tokens, Some(140));
    assert_ne!(refreshed.file_size, old.file_size);
    assert_eq!(refreshed.file_mtime_ns, old.file_mtime_ns);
    assert_eq!(
        compute_cost(&AtifTrajectory::from_json(&refreshed.atif_json).unwrap())
            .unwrap()
            .total_real_input_tok,
        140
    );
}
