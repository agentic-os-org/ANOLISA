use super::*;

fn test_store(label: &str) -> (std::path::PathBuf, GenAISqliteStore) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "agentsight-resource-{label}-{}-{nonce}.db",
        std::process::id()
    ));
    let store =
        GenAISqliteStore::new_with_path(&path, crate::config::InsertStoragePolicy::default())
            .expect("resource test store");
    (path, store)
}

fn cleanup_store(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

fn insert_call(
    store: &GenAISqliteStore,
    call_id: &str,
    start_ns: i64,
    end_ns: i64,
    tool_call_ids: Option<&str>,
    input_messages: Option<&str>,
) {
    let conn = store.conn.lock().unwrap_or_else(|error| error.into_inner());
    conn.execute(
        "INSERT INTO genai_events
         (event_type, call_id, session_id, start_timestamp_ns,
          end_timestamp_ns, pid, tool_call_ids, input_messages, event_json)
         VALUES ('llm_call', ?1, 'session-1', ?2, ?3, 42, ?4, ?5, '{}')",
        rusqlite::params![call_id, start_ns, end_ns, tool_call_ids, input_messages],
    )
    .expect("insert LLM call");
}

#[test]
fn parses_nested_tool_responses() {
    let ids = parse_tool_response_ids(Some(
        r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"call-1","response":{}}]}]"#,
    ));
    assert!(ids.contains("call-1"));
}

#[test]
fn derives_tool_phase_between_matching_calls() {
    let calls = vec![
        SessionCall {
            start_ns: 100,
            end_ns: 200,
            pid: 1,
            tool_call_ids: vec!["call-1".to_string()],
            tool_response_ids: HashSet::new(),
        },
        SessionCall {
            start_ns: 500,
            end_ns: 600,
            pid: 1,
            tool_call_ids: Vec::new(),
            tool_response_ids: HashSet::from(["call-1".to_string()]),
        },
    ];
    let phases = derive_phases(&calls, 100, 600);
    assert!(phases.contains(&SessionPhase {
        kind: "tool_call".to_string(),
        start_timestamp_ns: 200,
        end_timestamp_ns: 500,
        tool_call_id: Some("call-1".to_string()),
    }));
    assert!(!phases.iter().any(|phase| phase.kind == "idle"));
}

#[test]
fn derives_idle_phase_for_unoccupied_time() {
    let calls = vec![
        SessionCall {
            start_ns: 100,
            end_ns: 200,
            pid: 1,
            tool_call_ids: Vec::new(),
            tool_response_ids: HashSet::new(),
        },
        SessionCall {
            start_ns: 500,
            end_ns: 600,
            pid: 1,
            tool_call_ids: Vec::new(),
            tool_response_ids: HashSet::new(),
        },
    ];
    let phases = derive_phases(&calls, 100, 600);
    assert!(phases.contains(&SessionPhase {
        kind: "idle".to_string(),
        start_timestamp_ns: 200,
        end_timestamp_ns: 500,
        tool_call_id: None,
    }));
}

#[test]
fn unmatched_tool_call_leaves_gap_idle() {
    let calls = vec![
        SessionCall {
            start_ns: 100,
            end_ns: 200,
            pid: 1,
            tool_call_ids: vec!["missing-result".to_string()],
            tool_response_ids: HashSet::new(),
        },
        SessionCall {
            start_ns: 500,
            end_ns: 600,
            pid: 1,
            tool_call_ids: Vec::new(),
            tool_response_ids: HashSet::new(),
        },
    ];
    let phases = derive_phases(&calls, 100, 600);
    assert!(!phases.iter().any(|phase| phase.kind == "tool_call"));
    assert!(phases.contains(&SessionPhase {
        kind: "idle".to_string(),
        start_timestamp_ns: 200,
        end_timestamp_ns: 500,
        tool_call_id: None,
    }));
}

#[test]
fn stores_and_queries_session_resource_timeline() {
    let (path, store) = test_store("timeline");
    insert_call(&store, "call-1", 100, 200, Some(r#"["tool-1"]"#), None);
    insert_call(
        &store,
        "call-2",
        500,
        600,
        None,
        Some(
            r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"tool-1","response":"ok"}]}]"#,
        ),
    );
    store
        .insert_resource_samples(&[
            ResourceSample {
                timestamp_ns: 150,
                pid: 42,
                agent_name: Some("TestAgent".to_string()),
                cpu_percent: 25.0,
                memory_bytes: 128 * 1024 * 1024,
            },
            ResourceSample {
                timestamp_ns: 550,
                pid: 42,
                agent_name: Some("TestAgent".to_string()),
                cpu_percent: 50.0,
                memory_bytes: 130 * 1024 * 1024,
            },
        ])
        .expect("insert samples");

    let timeline = store
        .get_session_resource_timeline("session-1", None, None, 100)
        .expect("query timeline")
        .expect("known session");

    assert_eq!(timeline.samples.len(), 2);
    assert!(timeline.phases.contains(&SessionPhase {
        kind: "tool_call".to_string(),
        start_timestamp_ns: 200,
        end_timestamp_ns: 500,
        tool_call_id: Some("tool-1".to_string()),
    }));
    drop(store);
    cleanup_store(&path);
}

#[test]
fn query_downsamples_and_rejects_unknown_session() {
    let (path, store) = test_store("downsample");
    insert_call(&store, "call-1", 100, 1_000, None, None);
    let samples: Vec<ResourceSample> = (1..=10)
        .map(|index| ResourceSample {
            timestamp_ns: index * 100,
            pid: 42,
            agent_name: None,
            cpu_percent: index as f64,
            memory_bytes: index * 1024,
        })
        .collect();
    store
        .insert_resource_samples(&samples)
        .expect("insert samples");

    let timeline = store
        .get_session_resource_timeline("session-1", None, None, 3)
        .expect("query timeline")
        .expect("known session");
    assert!(timeline.samples.len() <= 3);
    assert!(
        store
            .get_session_resource_timeline("missing", None, None, 3)
            .expect("query missing session")
            .is_none()
    );
    drop(store);
    cleanup_store(&path);
}
