use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn collect(calls: Value, replies: &[(&str, &str)]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("state.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY);
        INSERT INTO sessions VALUES ('session');
        CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT,
        role TEXT, content TEXT, tool_calls TEXT, tool_call_id TEXT,
        tool_name TEXT, timestamp REAL);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO messages (session_id,role,tool_calls,timestamp)
        VALUES ('session','assistant',?1,1)",
        [calls.to_string()],
    )
    .unwrap();
    for (id, text) in replies {
        let id = (!id.is_empty()).then_some(*id);
        db.execute(
            "INSERT INTO messages (session_id,role,content,tool_call_id,tool_name,timestamp)
            VALUES ('session','tool',?1,?2,'read_file',2)",
            params![text, id],
        )
        .unwrap();
    }
    let output = dir.path().join("output.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    scan_once(
        &store,
        &CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![dir.path().join("state.db")]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        },
    );
    let raw: String = Connection::open(output)
        .unwrap()
        .query_row("SELECT atif_json FROM collected_trajectories", [], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn idless_unique_result_uses_the_existing_call_id() {
    let doc = collect(
        json!([{"id":"known", "name":"read_file", "arguments":{}}]),
        &[("", "known output")],
    );
    assert_eq!(doc["steps"][0]["tool_calls"][0]["tool_call_id"], "known");
    assert_eq!(
        doc["steps"][0]["observation"]["results"][0]["source_call_id"],
        "known"
    );
    assert_eq!(
        doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"],
        "1970-01-01T00:00:02.000Z"
    );
}

#[test]
fn unmatched_explicit_id_does_not_rename_a_different_call() {
    let doc = collect(
        json!([{"id":"known", "name":"read_file", "arguments":{"path":"a"}}]),
        &[("missing", "orphan output"), ("known", "known output")],
    );
    let call = &doc["steps"][0]["tool_calls"][0];
    assert_eq!(call["tool_call_id"], "known");
    let results = doc["steps"][0]["observation"]["results"]
        .as_array()
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "unmatched IDs must not borrow a same-name call"
    );
    assert_eq!(results[0]["content"], "known output");
    assert_eq!(
        doc["steps"][1]["observation"]["results"][0]["content"],
        "orphan output"
    );
}

#[test]
fn ambiguous_missing_ids_preserve_calls_and_outputs_without_guessing() {
    let doc = collect(
        json!([
            {"name":"read_file", "arguments":{"path":"a"}},
            {"name":"read_file", "arguments":{"path":"b"}}
        ]),
        &[("result-b", "B"), ("result-a", "A")],
    );
    let calls = doc["steps"][0]["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["tool_call_id"], "hermes-1-0");
    assert_eq!(calls[1]["tool_call_id"], "hermes-1-1");
    assert!(doc["steps"][0].get("observation").is_none());
    assert_eq!(
        doc["steps"][1]["observation"]["results"][0]["source_call_id"],
        "result-b"
    );
    assert_eq!(
        doc["steps"][2]["observation"]["results"][0]["source_call_id"],
        "result-a"
    );
}

#[test]
fn unique_missing_id_keeps_legacy_name_fallback() {
    let doc = collect(
        json!([{"name":"read_file", "arguments":{"path":"a"}}]),
        &[("result-a", "A")],
    );
    assert_eq!(doc["steps"][0]["tool_calls"][0]["tool_call_id"], "result-a");
    assert_eq!(
        doc["steps"][0]["observation"]["results"][0]["source_call_id"],
        "result-a"
    );
    assert_eq!(
        doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"],
        "1970-01-01T00:00:02.000Z"
    );
}

#[test]
fn exact_ids_still_pair_reversed_parallel_results() {
    let doc = collect(
        json!([
            {"id":"a", "name":"read_file", "arguments":{"path":"a"}},
            {"id":"b", "name":"read_file", "arguments":{"path":"b"}}
        ]),
        &[("b", "B"), ("a", "A")],
    );
    let calls = doc["steps"][0]["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["tool_call_id"], "a");
    assert_eq!(calls[1]["tool_call_id"], "b");
    assert_eq!(
        doc["steps"][0]["observation"]["results"][0]["source_call_id"],
        "b"
    );
    assert_eq!(
        doc["steps"][0]["observation"]["results"][1]["source_call_id"],
        "a"
    );
}
