//! Grouped Codex activity must survive collection into the SQLite session summary.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use agentsight_trajectory_collector::{
    scan_once, CollectorConfig, TrajectoryRecord, TrajectoryStore,
};
use serde_json::{json, Value};

struct TempRoot(PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn collect(events: &[Value]) -> TrajectoryRecord {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("codex-end-time-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let _cleanup = TempRoot(root.clone());

    let sessions = root.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("rollout-grouped.jsonl"),
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&root.join("collection.db")).unwrap();
    scan_once(
        &store,
        &CollectorConfig {
            scan_dirs: Some(vec![sessions]),
            scan_interval_secs: 1,
            maintenance: Default::default(),
        },
    );
    store.get("rollout-grouped").unwrap().unwrap()
}

fn response(timestamp: &str, payload: Value) -> Value {
    json!({"type":"response_item","timestamp":timestamp,"payload":payload})
}

fn answer(timestamp: &str) -> Value {
    response(
        timestamp,
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}),
    )
}

#[test]
fn final_answer_after_tool_output_ends_the_session() {
    let row = collect(&[
        response(
            "2026-11-08T00:00:01Z",
            json!({"type":"reasoning","summary":[{"text":"plan"}]}),
        ),
        response(
            "2026-11-08T00:00:02Z",
            json!({"type":"function_call","call_id":"job","name":"shell","arguments":"{}"}),
        ),
        response(
            "2026-11-08T00:00:03Z",
            json!({"type":"function_call_output","call_id":"job","output":"ok"}),
        ),
        answer("2026-11-08T00:00:04Z"),
    ]);
    assert_eq!(row.start_time.as_deref(), Some("2026-11-08T00:00:01Z"));
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:00:04Z"));
}

#[test]
fn tool_less_turn_keeps_the_latest_valid_answer() {
    let row = collect(&[
        json!({"type":"event_msg","timestamp":"2026-11-08T00:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"output_tokens":3}}}}),
        answer("2026-11-08T00:00:04Z"),
        answer("2026-11-08T08:00:02+08:00"),
        answer("invalid"),
    ]);
    assert_eq!(row.start_time.as_deref(), Some("2026-11-08T00:00:01Z"));
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:00:04Z"));
}
