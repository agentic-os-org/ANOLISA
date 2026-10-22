//! Real protocol/SQLite coverage across fresh CLI processes sharing a stash.
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn invoke(data: &Path, operation: &str, input: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokenless"))
        .arg("compress")
        .env("TOKENLESS_DATA_DIR", data)
        .env_remove("TOKENLESS_STATS_DB")
        .env_remove("TOKENLESS_STASH_DB")
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "1")
        .env("TOKENLESS_STASH_ENABLED", "1")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = json!({"protocol_version":2,"operation":operation,
        "attribution":{"agent_id":"schema-refresh-test","session_id":"test"},"input":input});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"].clone()
}

fn tool(description: &str) -> Value {
    json!({"type":"function","function":{"name":"read","description":description,
        "parameters":{"type":"object","properties":{}}}})
}

fn before_model(data: &Path, tools: Vec<Value>) -> Value {
    invoke(
        data,
        "before_model",
        json!({"tools":tools,"visible_context":{"messages":[]},
        "capabilities":{"replace_tools":true,"recovery":{"kind":"tool","name":"tokenless_retrieve"}}}),
    )
}

fn recorded(data: &Path) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(data.join("stats.db")).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT recoverability,
        (SELECT COUNT(*) FROM compression_artifacts a WHERE a.stats_id=s.id)
        FROM stats s ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn refreshed_schema_marker_retains_artifact_and_recovery_classification() {
    let temp = tempfile::tempdir().unwrap();
    let description = "Read a document and return its complete contents. ".repeat(100);
    let first = before_model(temp.path(), vec![tool(&description)]);
    let second = before_model(temp.path(), vec![tool(&description)]);
    assert_eq!(first, second);
    assert_eq!(first["visible_markers"].as_array().unwrap().len(), 1);
    let key = first["visible_markers"][0].clone();
    let retrieved = invoke(
        temp.path(),
        "retrieve",
        json!({"hash_or_marker":key,"visible_markers":[key]}),
    );
    assert_eq!(retrieved["payload"], description);
    assert_eq!(
        recorded(temp.path()),
        vec![("retrievable".into(), 1), ("retrievable".into(), 1)]
    );
}

#[test]
fn mixed_new_and_refreshed_schema_keys_record_each_distinct_marker() {
    let temp = tempfile::tempdir().unwrap();
    let cached = "Describe an existing cached document for the caller. ".repeat(100);
    let fresh = "Look up a different record and explain all of its fields. ".repeat(100);
    before_model(temp.path(), vec![tool(&cached)]);
    let result = before_model(
        temp.path(),
        vec![tool(&cached), tool(&cached), tool(&fresh)],
    );
    assert_eq!(result["visible_markers"].as_array().unwrap().len(), 2);
    assert_eq!(
        recorded(temp.path()),
        vec![("retrievable".into(), 1), ("retrievable".into(), 2)]
    );
    for key in result["visible_markers"].as_array().unwrap() {
        let retrieved = invoke(
            temp.path(),
            "retrieve",
            json!({"hash_or_marker":key,"visible_markers":[key]}),
        );
        assert!(
            [cached.as_str(), fresh.as_str()].contains(&retrieved["payload"].as_str().unwrap())
        );
    }
}
