use rusqlite::params;

use super::{CACHE_DIAGNOSIS_MAX_ROWS, GenAISqliteStore};

fn temp_store(name: &str) -> (GenAISqliteStore, std::path::PathBuf) {
    let path =
        std::env::temp_dir().join(format!("agentsight_cache_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store =
        GenAISqliteStore::new_with_path(&path, crate::config::PeriodicStoragePolicy::default())
            .unwrap();
    (store, path)
}

fn insert(
    store: &GenAISqliteStore,
    call_id: &str,
    start: i64,
    provider: &str,
    counters: (Option<i64>, Option<i64>, Option<i64>, i64),
    event_json: &str,
) {
    let (input, read, write, includes_cache) = counters;
    let conn = store.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO genai_events
         (event_type, status, call_id, trace_id, session_id, start_timestamp_ns,
          end_timestamp_ns, first_output_timestamp_ns, agent_name, provider, model,
          input_tokens, cache_read_tokens, cache_creation_tokens,
          input_tokens_includes_cache, output_tokens, system_instructions, event_json)
         VALUES ('llm_call', 'complete', ?1, ?1, 'sess', ?2, ?2 + 50, ?2 + 20, 'Hermes',
                 ?3, 'qwen3.8-max', ?4, ?5, ?6, ?7, 5, 'You are helpful.', ?8)",
        params![
            call_id,
            start,
            provider,
            input,
            read,
            write,
            includes_cache,
            event_json
        ],
    )
    .unwrap();
}

const TOOLS_A: &str = r#"{"LLMCall":{"request":{"messages":[{},{},{}],"tools":[{"name":"a"}]}}}"#;
const TOOLS_B: &str = r#"{"LLMCall":{"request":{"messages":[{}],"tools":[{"name":"b"}]}}}"#;

#[test]
fn reads_normalized_counters_and_request_fingerprints() {
    let (store, path) = temp_store("window");
    insert(
        &store,
        "new",
        100,
        "openai",
        (Some(1000), Some(800), None, 1),
        TOOLS_A,
    );
    // Legacy Anthropic rows kept cache counters outside input_tokens.
    insert(
        &store,
        "legacy",
        200,
        "anthropic",
        (Some(100), Some(800), Some(100), 0),
        TOOLS_B,
    );
    insert(
        &store,
        "missing",
        300,
        "openai",
        (None, None, None, 1),
        "not json",
    );
    insert(
        &store,
        "outside",
        900,
        "openai",
        (Some(1), Some(1), None, 1),
        "{}",
    );

    let window = store.get_cache_call_window(0, 500, None, None).unwrap();
    assert!(!window.truncated);
    let ids: Vec<_> = window
        .rows
        .iter()
        .map(|row| row.call_id.as_deref())
        .collect();
    assert_eq!(ids, [Some("new"), Some("legacy"), Some("missing")]);

    let [new, legacy, missing] = &window.rows[..] else {
        panic!("three rows expected");
    };
    assert_eq!(new.input_tokens, Some(1000));
    assert_eq!(new.cache_read_tokens, Some(800));
    assert_eq!(new.cache_creation_tokens, None);
    assert_eq!(new.message_count, Some(3));
    assert_eq!(new.ttft_ms, Some(0.00002));
    assert_eq!(legacy.input_tokens, Some(1000));
    assert_eq!(legacy.message_count, Some(1));
    assert_ne!(new.tools_fingerprint, legacy.tools_fingerprint);
    assert!(new.tools_fingerprint.is_some());
    assert_eq!(new.system_fingerprint, legacy.system_fingerprint);
    // Invalid event JSON keeps the row and only loses the request fingerprints.
    assert_eq!(missing.input_tokens, None);
    assert_eq!(missing.cache_read_tokens, None);
    assert_eq!(missing.tools_fingerprint, None);
    assert_eq!(missing.message_count, None);

    let filtered = store
        .get_cache_call_window(0, 500, Some("hermes"), Some("sess"))
        .unwrap();
    assert_eq!(filtered.rows.len(), 3);
    let other = store
        .get_cache_call_window(0, 500, Some("other"), None)
        .unwrap();
    assert!(other.rows.is_empty());

    drop(store);
    let _ = std::fs::remove_file(path);
}

#[test]
fn keeps_the_newest_calls_when_the_window_is_over_the_cap() {
    let (store, path) = temp_store("cap");
    {
        let mut conn = store.conn.lock().unwrap();
        let tx = conn.transaction().unwrap();
        for start in 0..=CACHE_DIAGNOSIS_MAX_ROWS as i64 {
            tx.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, start_timestamp_ns, input_tokens, event_json)
                 VALUES ('llm_call', 'complete', ?1, ?2, 10, '{}')",
                params![format!("c{start}"), start],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let window = store
        .get_cache_call_window(0, i64::MAX, None, None)
        .unwrap();
    assert!(window.truncated);
    assert_eq!(window.rows.len(), CACHE_DIAGNOSIS_MAX_ROWS);
    assert_eq!(window.rows[0].start_ns, 1);
    assert_eq!(
        window.rows.last().map(|row| row.start_ns),
        Some(CACHE_DIAGNOSIS_MAX_ROWS as i64)
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[test]
fn keeps_fully_cached_legacy_rows_and_drops_zero_usage() {
    let (store, path) = temp_store("zero_input");
    // A legacy Anthropic row counts cache tokens outside input_tokens, so a
    // fully cached call stores a zero raw input with non-zero cache counters.
    insert(
        &store,
        "cached",
        100,
        "anthropic",
        (Some(0), Some(900), Some(0), 0),
        "{}",
    );
    insert(
        &store,
        "null-input",
        200,
        "anthropic",
        (None, Some(500), Some(20), 0),
        "{}",
    );
    insert(
        &store,
        "empty",
        300,
        "anthropic",
        (Some(0), Some(0), Some(0), 1),
        "{}",
    );

    let window = store.get_cache_call_window(0, 500, None, None).unwrap();
    let inputs: Vec<_> = window
        .rows
        .iter()
        .map(|row| (row.call_id.as_deref(), row.input_tokens))
        .collect();
    assert_eq!(
        inputs,
        [
            (Some("cached"), Some(900)),
            (Some("null-input"), Some(520)),
            (Some("empty"), None),
        ]
    );

    drop(store);
    let _ = std::fs::remove_file(path);
}
