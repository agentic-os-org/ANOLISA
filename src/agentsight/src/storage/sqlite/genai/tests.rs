use super::*;
use crate::genai::semantic::{
    GenAISemanticEvent, LLMCall, LLMRequest, LLMResponse, MessagePart, OutputMessage,
};

/// Integration test: store_event (post-fix, no per-insert VACUUM) still
/// persists data correctly and the row is immediately readable.
/// Reverting the VACUUM removal does NOT make this test fail (it would just
/// be slower), but this proves the write path is functional — the
/// discriminating signal for the per-insert VACUUM removal is the latency
/// benchmark, not a correctness test.
#[test]
fn store_event_persists_without_per_insert_vacuum() {
    let path = std::env::temp_dir().join(format!(
        "test_genai_store_{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = GenAISqliteStore::new_with_path(&path).unwrap();

    let call = LLMCall::new(
        "test-call-001".to_string(),
        1_700_000_000_000_000_000,
        "openai".to_string(),
        "gpt-4".to_string(),
        LLMRequest {
            messages: vec![],
            temperature: None,
            max_tokens: None,
            frequency_penalty: None,
            presence_penalty: None,
            top_p: None,
            top_k: None,
            seed: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            raw_body: None,
        },
        1234,
        "test-agent".to_string(),
    );
    let event = GenAISemanticEvent::LLMCall(call);

    // Write via the exact code path that was modified (store_event).
    store.store_event(&event).unwrap();

    // The event has no session_id set, so list_sessions (which filters
    // session_id IS NOT NULL) won't find it — use a raw count instead.
    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM genai_events WHERE call_id = 'test-call-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "store_event must persist the row");

    drop(conn);
    // Verify wal_checkpoint doesn't panic
    store.wal_checkpoint().unwrap();

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

/// Verify busy_timeout is set on connections (create_connection is used by
/// GenAISqliteStore::new_with_path internally).
#[test]
fn connection_has_busy_timeout() {
    let path = std::env::temp_dir().join(format!(
        "test_bt_{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    let conn = store.conn.lock().unwrap();
    // PRAGMA busy_timeout returns the current value in ms
    let timeout: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 500, "busy_timeout must be 500ms");
    drop(conn);
    let _ = std::fs::remove_file(&path);
}

use super::pending::parse_output_messages_for_loop_detection;

#[test]
fn test_parse_output_none() {
    let (tools, text) = parse_output_messages_for_loop_detection(None);
    assert!(tools.is_empty());
    assert!(text.is_empty());
}

#[test]
fn test_parse_output_invalid_json() {
    let (tools, text) = parse_output_messages_for_loop_detection(Some("not json"));
    assert!(tools.is_empty());
    assert!(text.is_empty());
}

#[test]
fn test_parse_output_tool_calls_only() {
    let json = r#"[{"role":"assistant","parts":[{"type":"tool_call","name":"read_file"},{"type":"tool_call","name":"write_file"}]}]"#;
    let (tools, text) = parse_output_messages_for_loop_detection(Some(json));
    let names: Vec<&str> = tools.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(names, vec!["read_file", "write_file"]);
    // No arguments field -> no fingerprint
    assert!(tools.iter().all(|k| k.args_fingerprint.is_none()));
    assert!(text.is_empty());
}

#[test]
fn test_parse_output_tool_call_args_fingerprint() {
    // Same tool with different arguments must yield different keys (#2691),
    // while identical arguments yield identical keys.
    let json_a = r#"[{"role":"assistant","parts":[{"type":"tool_call","name":"terminal","arguments":{"command":"pip list"}}]}]"#;
    let json_b = r#"[{"role":"assistant","parts":[{"type":"tool_call","name":"terminal","arguments":{"command":"cat foo"}}]}]"#;
    let (tools_a, _) = parse_output_messages_for_loop_detection(Some(json_a));
    let (tools_a2, _) = parse_output_messages_for_loop_detection(Some(json_a));
    let (tools_b, _) = parse_output_messages_for_loop_detection(Some(json_b));
    assert!(tools_a[0].args_fingerprint.is_some());
    assert_eq!(tools_a, tools_a2);
    assert_ne!(tools_a, tools_b);
}

#[test]
fn test_parse_output_text_only() {
    let json = r#"[{"role":"assistant","parts":[{"type":"text","content":"Hello world"}]}]"#;
    let (tools, text) = parse_output_messages_for_loop_detection(Some(json));
    assert!(tools.is_empty());
    assert_eq!(text, "Hello world");
}

#[test]
fn test_parse_output_mixed() {
    let json = r#"[{"role":"assistant","parts":[{"type":"tool_call","name":"search"},{"type":"text","content":"Found results"}]}]"#;
    let (tools, text) = parse_output_messages_for_loop_detection(Some(json));
    let names: Vec<&str> = tools.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(names, vec!["search"]);
    assert_eq!(text, "Found results");
}

#[test]
fn test_parse_output_multiple_text_parts() {
    let json = r#"[{"role":"assistant","parts":[{"type":"text","content":"Part 1"},{"type":"text","content":"Part 2"}]}]"#;
    let (_tools, text) = parse_output_messages_for_loop_detection(Some(json));
    assert_eq!(text, "Part 1 Part 2");
}

#[test]
fn test_parse_output_text_truncated_at_200_chars() {
    let long_content = "a".repeat(300);
    let json = format!(
        r#"[{{"role":"assistant","parts":[{{"type":"text","content":"{long_content}"}}]}}]"#
    );
    let (_, text) = parse_output_messages_for_loop_detection(Some(&json));
    assert_eq!(text.len(), 200);
}

#[test]
fn test_parse_output_empty_parts_array() {
    let json = r#"[{"role":"assistant","parts":[]}]"#;
    let (tools, text) = parse_output_messages_for_loop_detection(Some(json));
    assert!(tools.is_empty());
    assert!(text.is_empty());
}

#[test]
fn test_parse_output_no_parts_field() {
    let json = r#"[{"role":"assistant"}]"#;
    let (tools, text) = parse_output_messages_for_loop_detection(Some(json));
    assert!(tools.is_empty());
    assert!(text.is_empty());
}

// ─── Populated test store helpers ─────────────────────────────────────────────

use rusqlite::params;

const BASE_NS: i64 = 1_700_000_000_000_000_000;
const STEP_NS: i64 = 1_000_000_000;

fn cleanup_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

/// Create a store with 6 representative rows covering multiple sessions,
/// agents, models, tool_call_ids, and a pending record.
///
/// Layout (all event_type = 'llm_call'):
///   call-1: sess-1, agent-a, gpt-4,   trace-1, conv-1, pid=100, complete, tool_call_ids
///   call-2: sess-1, agent-a, gpt-4,   trace-1, conv-1, pid=100, complete
///   call-3: sess-1, agent-a, gpt-4,   trace-2, conv-2, pid=100, complete, user_query
///   call-4: sess-2, agent-b, claude-3, trace-3, conv-3, pid=200, complete
///   call-5: sess-2, agent-b, claude-3, trace-3, conv-3, pid=200, pending
///   call-6: sess-1, agent-a, gpt-4o,  trace-1, conv-1, pid=100, complete
fn create_populated_store(suffix: &str) -> (GenAISqliteStore, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("test_genai_{suffix}_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();

    let b = BASE_NS;
    let s = STEP_NS;
    let sql = "INSERT INTO genai_events (\
               call_id, event_type, start_timestamp_ns, end_timestamp_ns, duration_ns,\
               provider, model, input_tokens, output_tokens, total_tokens,\
               session_id, trace_id, conversation_id, agent_name, pid,\
               status, tool_call_ids, event_json, process_name, user_query\
               ) VALUES (?1,'llm_call',?2,?3,?4,?5,?6,?7,?8,?9,\
               ?10,?11,?12,?13,?14,?15,?16,'{}',?17,?18)";

    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            sql,
            params![
                "call-1",
                b,
                b + s,
                s,
                "openai",
                "gpt-4",
                100_i64,
                50_i64,
                150_i64,
                "sess-1",
                "trace-1",
                "conv-1",
                "agent-a",
                100_i32,
                "complete",
                r#"["tc-1","tc-2"]"#,
                "proc-a",
                None::<&str>
            ],
        )
        .unwrap();
        conn.execute(
            sql,
            params![
                "call-2",
                b + s,
                b + 2 * s,
                s,
                "openai",
                "gpt-4",
                200_i64,
                100_i64,
                300_i64,
                "sess-1",
                "trace-1",
                "conv-1",
                "agent-a",
                100_i32,
                "complete",
                None::<&str>,
                "proc-a",
                None::<&str>
            ],
        )
        .unwrap();
        conn.execute(
            sql,
            params![
                "call-3",
                b + 2 * s,
                b + 3 * s,
                s,
                "openai",
                "gpt-4",
                150_i64,
                75_i64,
                225_i64,
                "sess-1",
                "trace-2",
                "conv-2",
                "agent-a",
                100_i32,
                "complete",
                None::<&str>,
                "proc-a",
                "what is rust"
            ],
        )
        .unwrap();
        conn.execute(
            sql,
            params![
                "call-4",
                b + 3 * s,
                b + 4 * s,
                s,
                "anthropic",
                "claude-3",
                300_i64,
                150_i64,
                450_i64,
                "sess-2",
                "trace-3",
                "conv-3",
                "agent-b",
                200_i32,
                "complete",
                None::<&str>,
                "proc-b",
                None::<&str>
            ],
        )
        .unwrap();
        conn.execute(
            sql,
            params![
                "call-5",
                b + 4 * s,
                b + 5 * s,
                s,
                "anthropic",
                "claude-3",
                250_i64,
                125_i64,
                375_i64,
                "sess-2",
                "trace-3",
                "conv-3",
                "agent-b",
                200_i32,
                "pending",
                None::<&str>,
                "proc-b",
                None::<&str>
            ],
        )
        .unwrap();
        conn.execute(
            sql,
            params![
                "call-6",
                b + 5 * s,
                b + 6 * s,
                s,
                "openai",
                "gpt-4o",
                50_i64,
                25_i64,
                75_i64,
                "sess-1",
                "trace-1",
                "conv-1",
                "agent-a",
                100_i32,
                "complete",
                None::<&str>,
                "proc-a",
                None::<&str>
            ],
        )
        .unwrap();
    }
    (store, path)
}

// ─── stats.rs tests ───────────────────────────────────────────────────────────

#[test]
fn test_get_token_timeseries_returns_buckets() {
    let (store, path) = create_populated_store("ts_buckets");
    let r = store
        .get_token_timeseries(BASE_NS, BASE_NS + 6 * STEP_NS, None, 1)
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].input_tokens, 1050); // 100+200+150+300+250+50
    assert_eq!(r[0].output_tokens, 525);
    assert_eq!(r[0].total_tokens, 1575);
    cleanup_db(&path);
}

#[test]
fn test_get_token_timeseries_empty_range() {
    let (store, path) = create_populated_store("ts_empty");
    let r = store.get_token_timeseries(0, 1, None, 1).unwrap();
    assert!(r.is_empty());
    cleanup_db(&path);
}

#[test]
fn test_get_token_timeseries_with_agent_filter() {
    let (store, path) = create_populated_store("ts_agent");
    let r = store
        .get_token_timeseries(BASE_NS, BASE_NS + 6 * STEP_NS, Some("agent-a"), 1)
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].total_tokens, 750); // 150+300+225+75
    cleanup_db(&path);
}

#[test]
fn test_get_model_timeseries_returns_model_breakdown() {
    let (store, path) = create_populated_store("model_ts");
    let r = store
        .get_model_timeseries(BASE_NS, BASE_NS + 6 * STEP_NS, None, 1)
        .unwrap();
    assert_eq!(r.len(), 3);
    let gpt4 = r.iter().find(|b| b.model == "gpt-4").unwrap();
    assert_eq!(gpt4.total_tokens, 675); // 150+300+225
    let claude = r.iter().find(|b| b.model == "claude-3").unwrap();
    assert_eq!(claude.total_tokens, 825); // 450+375
    cleanup_db(&path);
}

#[test]
fn test_get_model_timeseries_with_agent_filter() {
    let (store, path) = create_populated_store("model_ts_agent");
    let r = store
        .get_model_timeseries(BASE_NS, BASE_NS + 6 * STEP_NS, Some("agent-a"), 1)
        .unwrap();
    assert_eq!(r.len(), 2); // gpt-4, gpt-4o
    cleanup_db(&path);
}

#[test]
fn test_get_agent_token_summary() {
    let (store, path) = create_populated_store("agent_summary");
    let r = store.get_agent_token_summary().unwrap();
    assert_eq!(r.len(), 2);
    // ORDER BY total_tokens DESC
    assert_eq!(r[0].agent_name, "agent-b");
    assert_eq!(r[0].total_tokens, 825);
    assert_eq!(r[0].request_count, 2);
    assert_eq!(r[1].agent_name, "agent-a");
    assert_eq!(r[1].total_tokens, 750);
    assert_eq!(r[1].request_count, 4);
    cleanup_db(&path);
}

/// Cache accounting differs by provider, and these aggregations recompute the
/// totals in SQL instead of reading the stored `total_tokens` column. Figures
/// come from captured traffic: an Anthropic call, which reports the cache
/// counters outside `input_tokens`, and a Qoder CLI call, whose OpenAI-style
/// gateway already counts them inside `prompt_tokens`.
#[test]
fn test_token_aggregations_apply_provider_cache_rule() {
    let path =
        std::env::temp_dir().join(format!("test_genai_cache_rule_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();

    let sql = "INSERT INTO genai_events (\
               call_id, event_type, start_timestamp_ns, end_timestamp_ns, duration_ns,\
               provider, model, input_tokens, output_tokens, total_tokens,\
               cache_creation_tokens, cache_read_tokens,\
               agent_name, pid, status, event_json, process_name,\
               session_id, conversation_id\
               ) VALUES (?1,'llm_call',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'complete','{}',?14,?15,?16)";
    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            sql,
            params![
                "anthropic-call",
                BASE_NS,
                BASE_NS + STEP_NS,
                STEP_NS,
                "anthropic",
                "claude-opus-5",
                1111_i64,
                803_i64,
                26490_i64,
                0_i64,
                24576_i64,
                "claude",
                1_i32,
                "proc-claude",
                "sess-anthropic",
                "conv-anthropic"
            ],
        )
        .unwrap();
        conn.execute(
            sql,
            params![
                "openai-call",
                BASE_NS + STEP_NS,
                BASE_NS + 2 * STEP_NS,
                STEP_NS,
                "openai",
                "gpt-4o",
                74302_i64,
                170_i64,
                74472_i64,
                0_i64,
                71552_i64,
                "qodercli",
                2_i32,
                "proc-node",
                "sess-openai",
                "conv-openai"
            ],
        )
        .unwrap();
    }

    let ts = store
        .get_token_timeseries(BASE_NS, BASE_NS + 6 * STEP_NS, None, 1)
        .unwrap();
    assert_eq!(ts.len(), 1);
    // Anthropic contributes 1111 + 24576; OpenAI contributes 74302 as reported.
    assert_eq!(ts[0].input_tokens, 99_989);
    assert_eq!(ts[0].output_tokens, 973);
    // Adding the OpenAI cache read a second time would report 172514 here.
    assert_eq!(ts[0].total_tokens, 100_962);

    let by_agent = store.get_agent_token_summary().unwrap();
    assert_eq!(by_agent.len(), 2);
    assert_eq!(by_agent[0].agent_name, "qodercli");
    assert_eq!(by_agent[0].input_tokens, 74_302);
    assert_eq!(by_agent[0].total_tokens, 74_472);
    assert_eq!(by_agent[1].agent_name, "claude");
    assert_eq!(by_agent[1].input_tokens, 25_687);
    assert_eq!(by_agent[1].total_tokens, 26_490);

    let by_model = store
        .get_model_timeseries(BASE_NS, BASE_NS + 6 * STEP_NS, None, 1)
        .unwrap();
    let gpt = by_model.iter().find(|b| b.model == "gpt-4o").unwrap();
    assert_eq!(gpt.total_tokens, 74_472);
    let claude = by_model
        .iter()
        .find(|b| b.model == "claude-opus-5")
        .unwrap();
    assert_eq!(claude.total_tokens, 26_490);

    // Session-scoped queries feed the sessions list and the Token Savings page,
    // and aggregate the same way.
    let savings = store
        .list_sessions_for_savings(BASE_NS, BASE_NS + 6 * STEP_NS, None)
        .unwrap();
    let openai_session = savings
        .iter()
        .find(|s| s.session_id == "sess-openai")
        .unwrap();
    assert_eq!(openai_session.total_input_tokens, 74_302);
    let anthropic_session = savings
        .iter()
        .find(|s| s.session_id == "sess-anthropic")
        .unwrap();
    assert_eq!(anthropic_session.total_input_tokens, 25_687);

    let single = store
        .get_session_for_savings("sess-openai")
        .unwrap()
        .expect("session should exist");
    assert_eq!(single.total_input_tokens, 74_302);

    let sessions = store
        .list_sessions(BASE_NS, BASE_NS + 6 * STEP_NS, true)
        .unwrap();
    let listed = sessions
        .iter()
        .find(|s| s.session_id == "sess-openai")
        .unwrap();
    assert_eq!(listed.total_input_tokens, 74_302);

    let traces = store
        .list_traces_by_session("sess-openai", None, None, true)
        .unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].total_input_tokens, 74_302);

    cleanup_db(&path);
}

#[test]
fn test_get_agent_token_summary_empty() {
    let path = std::env::temp_dir().join(format!("test_genai_ats_empty_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    assert!(store.get_agent_token_summary().unwrap().is_empty());
    cleanup_db(&path);
}

// ─── session.rs tests ─────────────────────────────────────────────────────────

#[test]
fn test_list_sessions() {
    let (store, path) = create_populated_store("list_sess");
    let r = store
        .list_sessions(BASE_NS, BASE_NS + 6 * STEP_NS, true)
        .unwrap();
    assert_eq!(r.len(), 2);
    // sess-1 last_seen=base+5s > sess-2 base+4s
    assert_eq!(r[0].session_id, "sess-1");
    assert_eq!(r[0].conversation_count, 2);
    assert_eq!(r[0].total_input_tokens, 500);
    // Only call-3 in sess-1 carries a user_query, so it is both preview ends.
    assert_eq!(r[0].first_user_query.as_deref(), Some("what is rust"));
    assert_eq!(r[0].last_user_query.as_deref(), Some("what is rust"));
    assert_eq!(r[1].session_id, "sess-2");
    assert_eq!(r[1].total_input_tokens, 550);
    assert_eq!(r[1].first_user_query, None);
    assert_eq!(r[1].last_user_query, None);
    cleanup_db(&path);
}

#[test]
fn test_list_sessions_for_savings() {
    let (store, path) = create_populated_store("savings_no_agent");
    let r = store
        .list_sessions_for_savings(BASE_NS, BASE_NS + 6 * STEP_NS, None)
        .unwrap();
    assert_eq!(r.len(), 2);
    cleanup_db(&path);
}

#[test]
fn test_list_sessions_for_savings_with_agent_filter() {
    let (store, path) = create_populated_store("savings_agent");
    let r = store
        .list_sessions_for_savings(BASE_NS, BASE_NS + 6 * STEP_NS, Some("agent-b"))
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].session_id, "sess-2");
    assert_eq!(r[0].request_count, 2);
    cleanup_db(&path);
}

#[test]
fn test_get_session_for_savings() {
    let (store, path) = create_populated_store("get_savings");
    let s = store.get_session_for_savings("sess-1").unwrap().unwrap();
    assert_eq!(s.session_id, "sess-1");
    assert_eq!(s.total_input_tokens, 500);
    assert_eq!(s.total_output_tokens, 250);
    assert_eq!(s.request_count, 4);
    cleanup_db(&path);
}

#[test]
fn test_get_session_for_savings_not_found() {
    let (store, path) = create_populated_store("get_savings_404");
    assert!(
        store
            .get_session_for_savings("nonexistent")
            .unwrap()
            .is_none()
    );
    cleanup_db(&path);
}

#[test]
fn test_get_call_turn_indices() {
    let (store, path) = create_populated_store("call_turns");
    let m = store.get_call_turn_indices(&["sess-1"]).unwrap();
    assert_eq!(m.len(), 4);
    assert_eq!(m["call-1"], 1);
    assert_eq!(m["call-2"], 2);
    assert_eq!(m["call-3"], 3);
    assert_eq!(m["call-6"], 4);
    cleanup_db(&path);
}

#[test]
fn test_get_tool_call_turn_indices() {
    let (store, path) = create_populated_store("tc_turns");
    let m = store.get_tool_call_turn_indices(&["sess-1"]).unwrap();
    assert_eq!(m["tc-1"].turn_index, 1);
    assert_eq!(m["tc-1"].session_id, "sess-1");
    assert_eq!(m["tc-2"].turn_index, 1);
    assert!(m.contains_key("call-1"));
    cleanup_db(&path);
}

#[test]
fn test_list_traces_by_session() {
    let (store, path) = create_populated_store("traces");
    let r = store
        .list_traces_by_session("sess-1", None, None, true)
        .unwrap();
    assert_eq!(r.len(), 2);
    let c1 = r.iter().find(|t| t.conversation_id == "conv-1").unwrap();
    assert_eq!(c1.call_count, 3);
    assert_eq!(c1.total_input_tokens, 350); // 100+200+50
    let c2 = r.iter().find(|t| t.conversation_id == "conv-2").unwrap();
    assert_eq!(c2.call_count, 1);
    assert_eq!(c2.user_query.as_deref(), Some("what is rust"));
    cleanup_db(&path);
}

#[test]
fn test_list_traces_by_session_with_time_range() {
    let (store, path) = create_populated_store("traces_range");
    let r = store
        .list_traces_by_session("sess-1", Some(BASE_NS), Some(BASE_NS + STEP_NS), true)
        .unwrap();
    assert_eq!(r.len(), 1); // only conv-1
    assert_eq!(r[0].call_count, 2); // call-1, call-2
    cleanup_db(&path);
}

#[test]
fn test_list_agent_names() {
    let (store, path) = create_populated_store("agent_names");
    let r = store
        .list_agent_names(BASE_NS, BASE_NS + 6 * STEP_NS)
        .unwrap();
    assert_eq!(r, vec!["agent-a", "agent-b"]);
    cleanup_db(&path);
}

#[test]
fn test_lookup_session_for_pid() {
    let (store, path) = create_populated_store("lookup_pid");
    assert_eq!(
        store.lookup_session_for_pid(100).unwrap().as_deref(),
        Some("sess-1")
    );
    assert!(store.lookup_session_for_pid(999).unwrap().is_none());
    cleanup_db(&path);
}

#[test]
fn test_update_session_id() {
    let (store, path) = create_populated_store("update_sess");
    store.update_session_id("call-1", "sess-new").unwrap();
    let conn = store.conn.lock().unwrap();
    let sid: String = conn
        .query_row(
            "SELECT session_id FROM genai_events WHERE call_id = 'call-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sid, "sess-new");
    drop(conn);
    cleanup_db(&path);
}

// ─── events.rs tests ──────────────────────────────────────────────────────────

#[test]
fn test_get_trace_events() {
    let (store, path) = create_populated_store("trace_events");
    let r = store.get_trace_events("trace-1").unwrap();
    assert_eq!(r.len(), 3); // call-1, call-2, call-6
    assert_eq!(r[0].call_id.as_deref(), Some("call-1"));
    assert_eq!(r[0].input_tokens, 100);
    assert_eq!(r[2].call_id.as_deref(), Some("call-6"));
    cleanup_db(&path);
}

/// trace_id (per-call response id) and conversation_id are distinct
/// namespaces: the trace-detail query must never match a conversation id.
#[test]
fn test_get_trace_events_never_matches_conversation_id() {
    let (store, path) = create_populated_store("trace_ns_separation");
    assert!(!store.get_trace_events("trace-1").unwrap().is_empty());
    assert!(store.get_trace_events("conv-1").unwrap().is_empty());
    cleanup_db(&path);
}

#[test]
fn test_get_events_by_conversation() {
    let (store, path) = create_populated_store("conv_events");
    let r = store.get_events_by_conversation("conv-3").unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].call_id.as_deref(), Some("call-4"));
    assert_eq!(r[1].call_id.as_deref(), Some("call-5"));
    cleanup_db(&path);
}

#[test]
fn test_get_events_by_session() {
    let (store, path) = create_populated_store("sess_events");
    let r = store.get_events_by_session("sess-2").unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].model.as_deref(), Some("claude-3"));
    cleanup_db(&path);
}

#[test]
fn test_get_events_in_time_range() {
    let (store, path) = create_populated_store("range_events");
    let r = store
        .get_events_in_time_range(BASE_NS + 2 * STEP_NS, BASE_NS + 3 * STEP_NS, None)
        .unwrap();
    assert_eq!(r.len(), 2); // call-3, call-4
    cleanup_db(&path);
}

#[test]
fn test_get_events_in_time_range_with_agent_filter() {
    let (store, path) = create_populated_store("range_agent");
    let r = store
        .get_events_in_time_range(BASE_NS, BASE_NS + 6 * STEP_NS, Some("agent-b"))
        .unwrap();
    assert_eq!(r.len(), 2); // call-4, call-5
    cleanup_db(&path);
}

// ─── pending.rs tests ─────────────────────────────────────────────────────────

#[test]
fn test_insert_pending() {
    let path = std::env::temp_dir().join(format!("test_genai_ins_pend_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    let info = PendingCallInfo {
        call_id: "p-001".to_string(),
        trace_id: Some("t-p".to_string()),
        conversation_id: Some("c-p".to_string()),
        session_id: Some("s-p".to_string()),
        start_timestamp_ns: BASE_NS as u64,
        pid: 42,
        process_name: "test-proc".to_string(),
        agent_name: Some("test-agent".to_string()),
        http_method: Some("POST".to_string()),
        http_path: Some("/v1/chat".to_string()),
        input_messages: None,
        system_instructions: None,
        user_query: Some("hello".to_string()),
        is_sse: true,
        model: Some("gpt-4".to_string()),
        provider: Some("openai".to_string()),
        call_kind: "main".to_string(),
        pending_origin: PendingOrigin::RequestCapture,
        pending_match_key: None,
    };
    store.insert_pending(&info).unwrap();
    let conn = store.conn.lock().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM genai_events WHERE call_id = 'p-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_insert_pending_records_idle_origin_and_match_key() {
    let path =
        std::env::temp_dir().join(format!("test_genai_idle_origin_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    let info = PendingCallInfo {
        call_id: "idle-temp".to_string(),
        trace_id: None,
        conversation_id: Some("c-idle".to_string()),
        session_id: Some("s-idle".to_string()),
        start_timestamp_ns: BASE_NS as u64,
        pid: 42,
        process_name: "claude".to_string(),
        agent_name: Some("claude".to_string()),
        http_method: Some("POST".to_string()),
        http_path: Some("/v1/messages".to_string()),
        input_messages: None,
        system_instructions: None,
        user_query: Some("hello".to_string()),
        is_sse: true,
        model: Some("claude-sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        call_kind: "main".to_string(),
        pending_origin: PendingOrigin::IdleDrain,
        pending_match_key: Some("match-idle-1".to_string()),
    };
    store.insert_pending(&info).unwrap();

    let conn = store.conn.lock().unwrap();
    let (origin, match_key): (String, String) = conn
        .query_row(
            "SELECT pending_origin, pending_match_key FROM genai_events WHERE call_id = 'idle-temp'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, "idle_drain");
    assert_eq!(match_key, "match-idle-1");
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_complete_pending_promotes_idle_snapshot_by_match_key() {
    let path =
        std::env::temp_dir().join(format!("test_genai_idle_promote_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    let info = PendingCallInfo {
        call_id: "idle-temp".to_string(),
        trace_id: None,
        conversation_id: Some("c-idle".to_string()),
        session_id: Some("s-idle".to_string()),
        start_timestamp_ns: BASE_NS as u64,
        pid: 42,
        process_name: "claude".to_string(),
        agent_name: Some("claude".to_string()),
        http_method: Some("POST".to_string()),
        http_path: Some("/v1/messages".to_string()),
        input_messages: None,
        system_instructions: None,
        user_query: Some("hello".to_string()),
        is_sse: true,
        model: Some("claude-sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        call_kind: "main".to_string(),
        pending_origin: PendingOrigin::IdleDrain,
        pending_match_key: Some("match-idle-2".to_string()),
    };
    store.insert_pending(&info).unwrap();

    let request = LLMRequest {
        messages: vec![],
        temperature: None,
        max_tokens: None,
        frequency_penalty: None,
        presence_penalty: None,
        top_p: None,
        top_k: None,
        seed: None,
        stop_sequences: None,
        stream: true,
        tools: None,
        raw_body: None,
    };
    let mut call = LLMCall::new(
        "real-response-id".to_string(),
        BASE_NS as u64,
        "anthropic".to_string(),
        "claude-sonnet".to_string(),
        request,
        42,
        "claude".to_string(),
    );
    call.set_response(
        LLMResponse {
            messages: vec![OutputMessage {
                role: "assistant".to_string(),
                parts: vec![MessagePart::Text {
                    content: "done".to_string(),
                }],
                name: None,
                finish_reason: Some("stop".to_string()),
            }],
            streamed: true,
            raw_body: None,
        },
        (BASE_NS + STEP_NS) as u64,
    );
    call.metadata
        .insert("response_id".to_string(), "real-response-id".to_string());
    call.metadata
        .insert("pending_match_key".to_string(), "match-idle-2".to_string());
    call.metadata
        .insert("conversation_id".to_string(), "c-real".to_string());
    call.metadata
        .insert("session_id".to_string(), "s-real".to_string());
    call.metadata
        .insert("status_code".to_string(), "200".to_string());
    call.metadata
        .insert("sse_event_count".to_string(), "2".to_string());
    call.metadata
        .insert("call_kind".to_string(), "main".to_string());
    call.metadata.insert(
        "first_output_timestamp_ns".to_string(),
        (BASE_NS + STEP_NS / 2).to_string(),
    );

    store
        .complete_pending(&GenAISemanticEvent::LLMCall(call))
        .unwrap();

    let conn = store.conn.lock().unwrap();
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM genai_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 1, "complete must update the idle snapshot row");

    let (status, call_id, trace_id, origin, first_output): (
        String,
        String,
        String,
        String,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT status, call_id, trace_id, pending_origin, first_output_timestamp_ns
             FROM genai_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(status, "complete");
    assert_eq!(call_id, "real-response-id");
    assert_eq!(trace_id, "real-response-id");
    assert_eq!(origin, "idle_drain");
    assert_eq!(first_output, Some(BASE_NS + STEP_NS / 2));
    drop(conn);
    cleanup_db(&path);
}

/// Regression: `complete_pending` must backfill `is_sse` from the observed
/// metadata value so that protocols whose streaming switch lives in request
/// headers (e.g. DashScope native `X-DashScope-SSE: enable`) instead of the
/// body `stream` field are recorded as streaming calls (#3129).
///
/// Without the fix the UPDATE leaves the request-side `is_sse=0` in place
/// while `sse_event_count` is correctly updated — the two columns contradict
/// each other and `streaming_call_count` misses the call entirely.
#[test]
fn test_complete_pending_backfills_is_sse_from_observed_metadata() {
    let path =
        std::env::temp_dir().join(format!("test_genai_sse_backfill_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    // Simulate a native DashScope streaming request: body has no "stream"
    // field so the request-side capture writes is_sse=false.
    let info = PendingCallInfo {
        call_id: "native-sse".to_string(),
        trace_id: None,
        conversation_id: Some("c-native".to_string()),
        session_id: Some("s-native".to_string()),
        start_timestamp_ns: BASE_NS as u64,
        pid: 42,
        process_name: "test-proc".to_string(),
        agent_name: Some("test-agent".to_string()),
        http_method: Some("POST".to_string()),
        http_path: Some("/api/v1/services/aigc/text-generation/generation".to_string()),
        input_messages: None,
        system_instructions: None,
        user_query: Some("hello".to_string()),
        is_sse: false,
        model: Some("qwen-flash".to_string()),
        provider: Some("dashscope".to_string()),
        call_kind: "main".to_string(),
        pending_origin: PendingOrigin::RequestCapture,
        pending_match_key: None,
    };
    store.insert_pending(&info).unwrap();

    let request = LLMRequest {
        messages: vec![],
        temperature: None,
        max_tokens: None,
        frequency_penalty: None,
        presence_penalty: None,
        top_p: None,
        top_k: None,
        seed: None,
        stop_sequences: None,
        stream: false,
        tools: None,
        raw_body: None,
    };
    let mut call = LLMCall::new(
        "native-sse".to_string(),
        BASE_NS as u64,
        "dashscope".to_string(),
        "qwen-flash".to_string(),
        request,
        42,
        "test-agent".to_string(),
    );
    call.set_response(
        LLMResponse {
            messages: vec![OutputMessage {
                role: "assistant".to_string(),
                parts: vec![MessagePart::Text {
                    content: "1, 2, 3".to_string(),
                }],
                name: None,
                finish_reason: Some("stop".to_string()),
            }],
            streamed: true,
            raw_body: None,
        },
        (BASE_NS + STEP_NS) as u64,
    );
    call.metadata
        .insert("response_id".to_string(), "native-sse".to_string());
    call.metadata
        .insert("conversation_id".to_string(), "c-native".to_string());
    call.metadata
        .insert("session_id".to_string(), "s-native".to_string());
    call.metadata
        .insert("status_code".to_string(), "200".to_string());
    call.metadata
        .insert("sse_event_count".to_string(), "6".to_string());
    call.metadata
        .insert("is_sse".to_string(), "true".to_string());
    call.metadata
        .insert("call_kind".to_string(), "main".to_string());

    store
        .complete_pending(&GenAISemanticEvent::LLMCall(call))
        .unwrap();

    let conn = store.conn.lock().unwrap();
    let (status, is_sse, sse_count): (String, i64, Option<i64>) = conn
        .query_row(
            "SELECT status, is_sse, sse_event_count FROM genai_events WHERE call_id = 'native-sse'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "complete");
    assert_eq!(
        is_sse, 1,
        "complete_pending must backfill is_sse from observed metadata (#3129)"
    );
    assert_eq!(sse_count, Some(6));
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_mark_interrupted_stale() {
    let (store, path) = create_populated_store("mark_stale");
    // call-5 is pending at BASE_NS + 4s, well in the past relative to now
    let updated = store.mark_interrupted_stale(1).unwrap();
    assert_eq!(updated, 1);
    let conn = store.conn.lock().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM genai_events WHERE call_id = 'call-5'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "interrupted");
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_list_pending_for_pid() {
    let (store, path) = create_populated_store("pend_pid");
    let r = store.list_pending_for_pid(200).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, "call-5");
    cleanup_db(&path);
}

#[test]
fn test_list_pending_for_pids() {
    let (store, path) = create_populated_store("pend_pids");
    let r = store.list_pending_for_pids(&[200]).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, "call-5");
    assert!(store.list_pending_for_pids(&[]).unwrap().is_empty());
    cleanup_db(&path);
}

#[test]
fn test_mark_pending_interrupted_for_pid() {
    let (store, path) = create_populated_store("mark_pid");
    let n = store
        .mark_pending_interrupted_for_pid(200, "agent_crash")
        .unwrap();
    assert_eq!(n, 1);
    let conn = store.conn.lock().unwrap();
    let (st, it): (String, String) = conn
        .query_row(
            "SELECT status, interruption_type FROM genai_events WHERE call_id = 'call-5'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(st, "interrupted");
    assert_eq!(it, "agent_crash");
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_crash_sweep_ignores_idle_drain_pending() {
    let path =
        std::env::temp_dir().join(format!("test_genai_idle_sweep_{}.db", std::process::id()));
    cleanup_db(&path);
    let store = GenAISqliteStore::new_with_path(&path).unwrap();

    for (call_id, origin) in [
        ("dead-drain", PendingOrigin::DeadPidDrain),
        ("idle-drain", PendingOrigin::IdleDrain),
    ] {
        let info = PendingCallInfo {
            call_id: call_id.to_string(),
            trace_id: None,
            conversation_id: Some("c-sweep".to_string()),
            session_id: Some("s-sweep".to_string()),
            start_timestamp_ns: BASE_NS as u64,
            pid: 42,
            process_name: "claude".to_string(),
            agent_name: Some("claude".to_string()),
            http_method: Some("POST".to_string()),
            http_path: Some("/v1/messages".to_string()),
            input_messages: None,
            system_instructions: None,
            user_query: Some("hello".to_string()),
            is_sse: true,
            model: Some("claude-sonnet".to_string()),
            provider: Some("anthropic".to_string()),
            call_kind: "main".to_string(),
            pending_origin: origin,
            pending_match_key: Some(format!("match-{call_id}")),
        };
        store.insert_pending(&info).unwrap();
    }

    let listed = store.list_pending_for_pid(42).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "dead-drain");

    let updated = store
        .mark_pending_interrupted_for_pid(42, "agent_crash")
        .unwrap();
    assert_eq!(updated, 1);

    let conn = store.conn.lock().unwrap();
    let idle_status: String = conn
        .query_row(
            "SELECT status FROM genai_events WHERE call_id = 'idle-drain'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idle_status, "pending");
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_enrich_pending_from_sse() {
    let (store, path) = create_populated_store("enrich_sse");
    let e = SseEnrichment {
        model: Some("gpt-4-turbo".to_string()),
        trace_id: Some("trace-enriched".to_string()),
        provider: Some("openai-e".to_string()),
        output_messages: Some(r#"[{"role":"assistant"}]"#.to_string()),
        sse_event_count: Some(42),
        input_tokens: Some(999),
        output_tokens: Some(888),
    };
    store.enrich_pending_from_sse("call-5", &e).unwrap();
    let conn = store.conn.lock().unwrap();
    let (model, tid, it, ot): (String, String, i64, i64) = conn
        .query_row(
            "SELECT model, trace_id, input_tokens, output_tokens \
             FROM genai_events WHERE call_id = 'call-5'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(model, "gpt-4-turbo");
    assert_eq!(tid, "trace-enriched");
    assert_eq!(it, 999);
    assert_eq!(ot, 888);
    drop(conn);
    cleanup_db(&path);
}

// ─── schema.rs tests ──────────────────────────────────────────────────────────

#[test]
fn test_check_and_prune_if_needed_below_threshold() {
    let (store, path) = create_populated_store("prune_check");
    // Tiny test DB is well below the 200 MB default threshold
    store.check_and_prune_if_needed().unwrap();
    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM genai_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 6); // no pruning occurred
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_prune_old_records() {
    let (store, path) = create_populated_store("prune");
    store.prune_old_records().unwrap();
    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM genai_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 5); // 5% of 6 ≈ 1 record deleted
    drop(conn);
    cleanup_db(&path);
}

#[test]
fn test_wal_checkpoint_method() {
    let (store, path) = create_populated_store("wal_ckpt");
    store.wal_checkpoint().unwrap();
    cleanup_db(&path);
}

// ─── list_sessions / list_traces call_kind filter tests ──────────────────────

fn make_store_with_pending(records: &[(&str, &str, &str, &str, i64)]) -> GenAISqliteStore {
    let path = std::env::temp_dir().join(format!(
        "test_ck_{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    for (call_id, sid, cid, kind, ts) in records {
        let info = PendingCallInfo {
            call_id: call_id.to_string(),
            trace_id: None,
            conversation_id: Some(cid.to_string()),
            session_id: Some(sid.to_string()),
            start_timestamp_ns: *ts as u64,
            pid: 1,
            process_name: "test".to_string(),
            agent_name: Some("test-agent".to_string()),
            http_method: None,
            http_path: None,
            input_messages: None,
            system_instructions: None,
            user_query: None,
            is_sse: false,
            model: Some("gpt-4".to_string()),
            provider: Some("openai".to_string()),
            call_kind: kind.to_string(),
            pending_origin: PendingOrigin::RequestCapture,
            pending_match_key: None,
        };
        store.insert_pending(&info).unwrap();
    }
    store
}

#[test]
fn test_list_sessions_excludes_auxiliary() {
    let store = make_store_with_pending(&[
        ("c1", "sess-a", "conv-1", "main", 1000),
        ("c2", "sess-a", "conv-2", "recap", 2000),
        ("c3", "sess-b", "conv-3", "web_search", 3000),
        ("c4", "sess-b", "conv-4", "main", 4000),
    ]);
    let sessions = store.list_sessions(0, 10000, false).unwrap();
    assert_eq!(sessions.len(), 2);
    for s in &sessions {
        assert!(s.conversation_count >= 1);
    }
    let sessions_all = store.list_sessions(0, 10000, true).unwrap();
    assert_eq!(sessions_all.len(), 2);
    let total_convs: i64 = sessions_all.iter().map(|s| s.conversation_count).sum();
    assert_eq!(total_convs, 4);
}

#[test]
fn test_list_sessions_only_auxiliary_hidden() {
    let store = make_store_with_pending(&[
        ("c1", "sess-recap", "conv-1", "recap", 1000),
        ("c2", "sess-recap", "conv-2", "web_search", 2000),
    ]);
    let sessions = store.list_sessions(0, 10000, false).unwrap();
    assert!(
        sessions.is_empty(),
        "sessions with only auxiliary calls should be hidden"
    );
    let sessions_all = store.list_sessions(0, 10000, true).unwrap();
    assert_eq!(sessions_all.len(), 1);
}

#[test]
fn test_list_sessions_preview_honors_call_kind_filter() {
    let store = make_store_with_pending(&[
        ("c1", "sess-a", "conv-1", "recap", 1000),
        ("c2", "sess-a", "conv-2", "main", 2000),
    ]);
    // Give both calls a user_query: the auxiliary one must not become the
    // preview when auxiliary calls are excluded.
    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "UPDATE genai_events SET user_query = 'recap query' WHERE call_id = 'c1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE genai_events SET user_query = 'main query' WHERE call_id = 'c2'",
            [],
        )
        .unwrap();
    }
    let sessions = store.list_sessions(0, 10000, false).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_user_query.as_deref(), Some("main query"));
    assert_eq!(sessions[0].last_user_query.as_deref(), Some("main query"));

    // With auxiliary included, the earlier recap query becomes the first.
    let sessions_all = store.list_sessions(0, 10000, true).unwrap();
    assert_eq!(
        sessions_all[0].first_user_query.as_deref(),
        Some("recap query")
    );
    assert_eq!(
        sessions_all[0].last_user_query.as_deref(),
        Some("main query")
    );
}

#[test]
fn test_list_traces_by_session_excludes_auxiliary() {
    let store = make_store_with_pending(&[
        ("c1", "sess-x", "conv-main", "main", 1000),
        ("c2", "sess-x", "conv-recap", "recap", 2000),
        ("c3", "sess-x", "conv-main", "main", 3000),
    ]);
    let traces = store
        .list_traces_by_session("sess-x", None, None, false)
        .unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].conversation_id, "conv-main");
    assert_eq!(traces[0].call_count, 2);
    let traces_all = store
        .list_traces_by_session("sess-x", None, None, true)
        .unwrap();
    assert_eq!(traces_all.len(), 2);
}

#[test]
fn test_list_traces_call_kind_filter_with_time_range() {
    let store = make_store_with_pending(&[
        ("c1", "sess-t", "conv-a", "main", 1000),
        ("c2", "sess-t", "conv-b", "recap", 2000),
        ("c3", "sess-t", "conv-c", "main", 5000),
    ]);
    let traces = store
        .list_traces_by_session("sess-t", Some(0), Some(3000), false)
        .unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].conversation_id, "conv-a");
    let traces_all = store
        .list_traces_by_session("sess-t", Some(0), Some(3000), true)
        .unwrap();
    assert_eq!(traces_all.len(), 2);
}

// ─── poison-recovery tests ─────────────────────────────────────────────────

/// After intentionally poisoning the conn mutex, methods that use
/// `unwrap_or_else(|e| e.into_inner())` should still operate correctly.
#[test]
fn poison_recovery_conn_still_operational() {
    let (store, path) = create_populated_store("poison_conn");

    // Intentionally poison the conn mutex by panicking while holding the lock
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = store.conn.lock().unwrap();
        panic!("intentional poison");
    }));
    assert!(result.is_err(), "Mutex should be poisoned");

    // Exercise the poison-recovery path: get_events_by_session locks conn
    // and should recover via unwrap_or_else(|e| e.into_inner())
    let events = store.get_events_by_session("sess-1").unwrap();
    assert!(
        !events.is_empty(),
        "Should still read after conn poison recovery"
    );

    // Also exercise a write path (schema.rs: wal_checkpoint via VACUUM)
    store.wal_checkpoint().unwrap();

    cleanup_db(&path);
}

/// After intentionally poisoning the pending and last_flush mutexes,
/// flush() should still operate correctly via poison recovery.
#[test]
fn poison_recovery_flush_still_operational() {
    let (store, path) = create_populated_store("poison_flush");

    // Insert a pending event (normal path) to populate the pending buffer
    let info = PendingCallInfo {
        call_id: "poison-flush-1".to_string(),
        trace_id: None,
        conversation_id: Some("c-pf".to_string()),
        session_id: Some("s-pf".to_string()),
        start_timestamp_ns: BASE_NS as u64,
        pid: 99,
        process_name: "test-proc".to_string(),
        agent_name: Some("test-agent".to_string()),
        http_method: None,
        http_path: None,
        input_messages: None,
        system_instructions: None,
        user_query: Some("hello".to_string()),
        is_sse: false,
        model: Some("gpt-4".to_string()),
        provider: Some("openai".to_string()),
        call_kind: "main".to_string(),
        pending_origin: PendingOrigin::RequestCapture,
        pending_match_key: None,
    };
    store.insert_pending(&info).unwrap();

    // Also add to the pending event buffer (for flush)
    {
        let mut pending = store.pending.lock().unwrap();
        use crate::genai::semantic::{GenAISemanticEvent, LLMCall, LLMRequest};
        let call = LLMCall::new(
            "poison-flush-2".to_string(),
            BASE_NS as u64,
            "openai".to_string(),
            "gpt-4".to_string(),
            LLMRequest {
                messages: vec![],
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: None,
                raw_body: None,
            },
            99,
            "test-agent".to_string(),
        );
        pending.push(GenAISemanticEvent::LLMCall(call));
    }

    // Poison both the pending and last_flush mutexes
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _g1 = store.pending.lock().unwrap();
        let _g2 = store.last_flush.lock().unwrap();
        panic!("intentional poison");
    }));
    assert!(result.is_err(), "Mutexes should be poisoned");

    // flush() exercises:
    //   - pending.lock().unwrap_or_else(|e| e.into_inner())  (mod.rs)
    //   - last_flush.lock().unwrap_or_else(|e| e.into_inner())  (mod.rs)
    store.flush();

    cleanup_db(&path);
}

// ─── update_fallback_session_id (retroactive session fix-up, issue #2059) ─────────────

/// Read the current session_id of a call directly from the table.
fn session_id_of(store: &GenAISqliteStore, call_id: &str) -> Option<String> {
    let conn = store.conn.lock().unwrap();
    conn.query_row(
        "SELECT session_id FROM genai_events WHERE call_id = ?1",
        rusqlite::params![call_id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn test_update_fallback_session_id_replaces_hash_fallback() {
    // 32-hex fallback (id_resolver shape) must be replaced by the real UUID.
    let store = make_store_with_pending(&[(
        "c1",
        "0123456789abcdef0123456789abcdef",
        "conv-1",
        "main",
        1000,
    )]);
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let updated = store.update_fallback_session_id("c1", uuid).unwrap();
    assert_eq!(updated, 1, "hash-shaped session_id must be updated");
    assert_eq!(session_id_of(&store, "c1").as_deref(), Some(uuid));
}

#[test]
fn test_update_fallback_session_id_keeps_uuid_session() {
    // A 36-char session_id is a trusted mapper/metadata UUID — never overwrite.
    let existing = "11111111-2222-4333-8444-555555555555";
    let store = make_store_with_pending(&[("c1", existing, "conv-1", "main", 1000)]);
    let updated = store
        .update_fallback_session_id("c1", "550e8400-e29b-41d4-a716-446655440000")
        .unwrap();
    assert_eq!(
        updated, 0,
        "existing UUID session_id must not be overwritten"
    );
    assert_eq!(session_id_of(&store, "c1").as_deref(), Some(existing));
}

#[test]
fn test_update_fallback_session_id_unknown_call_id() {
    let store = make_store_with_pending(&[(
        "c1",
        "0123456789abcdef0123456789abcdef",
        "conv-1",
        "main",
        1000,
    )]);
    let updated = store
        .update_fallback_session_id("no-such-call", "550e8400-e29b-41d4-a716-446655440000")
        .unwrap();
    assert_eq!(updated, 0, "unknown call_id must update nothing");
}

#[test]
fn test_update_fallback_session_id_keeps_other_lengths() {
    // Anything that is not the 32-hex fallback shape — e.g. a 20-char custom
    // session id — must be left alone, not just 36-char UUIDs.
    let existing = "custom-session-20ch!";
    assert_eq!(existing.len(), 20);
    let store = make_store_with_pending(&[("c1", existing, "conv-1", "main", 1000)]);
    let updated = store
        .update_fallback_session_id("c1", "550e8400-e29b-41d4-a716-446655440000")
        .unwrap();
    assert_eq!(updated, 0, "non-32-char session_id must not be overwritten");
    assert_eq!(session_id_of(&store, "c1").as_deref(), Some(existing));
}

#[test]
fn test_update_fallback_session_id_keeps_non_hex_32_chars() {
    // 32 chars but not lowercase hex ('Z' and uppercase) — not a fallback
    // shape, must be left alone. Reverting the NOT GLOB guard fails this.
    for existing in [
        "Z123456789abcdef0123456789abcdef",
        "0123456789ABCDEF0123456789abcdef",
    ] {
        assert_eq!(existing.len(), 32);
        let store = make_store_with_pending(&[("c1", existing, "conv-1", "main", 1000)]);
        let updated = store
            .update_fallback_session_id("c1", "550e8400-e29b-41d4-a716-446655440000")
            .unwrap();
        assert_eq!(
            updated, 0,
            "non-hex 32-char session_id must not be overwritten"
        );
        assert_eq!(session_id_of(&store, "c1").as_deref(), Some(existing));
    }
}

// ---------------------------------------------------------------------------
// Size-based pruning (check_and_prune_if_needed / startup cleanup)
// ---------------------------------------------------------------------------

/// Cap the genai database at 1MB for size-limit tests.
///
/// All tests set the same value, so parallel execution never observes
/// conflicting limits, and other tests' databases stay far below the
/// resulting 0.9MB prune threshold.
fn set_test_db_limit() {
    unsafe { std::env::set_var("AGENTSIGHT_GENAI_DB_MAX_SIZE_MB", "1") };
}

fn unique_size_test_db(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "test_genai_size_{label}_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn cleanup_size_test_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

/// Insert `rows` records of roughly `payload` bytes each via direct SQL,
/// bypassing store_event's size checks so the test controls the file size.
fn grow_db(store: &GenAISqliteStore, rows: usize, payload: usize) {
    let blob = "x".repeat(payload);
    {
        let conn = store.conn.lock().unwrap();
        for i in 0..rows {
            conn.execute(
                "INSERT INTO genai_events (event_type, start_timestamp_ns, event_json)
                 VALUES ('llm_call', ?1, ?2)",
                rusqlite::params![i as i64, blob],
            )
            .unwrap();
        }
    }
    store.wal_checkpoint().unwrap();
}

fn row_count(store: &GenAISqliteStore) -> i64 {
    let conn = store.conn.lock().unwrap();
    conn.query_row("SELECT COUNT(*) FROM genai_events", [], |r| r.get(0))
        .unwrap()
}

/// Covers all three adaptive prune branches: overshoot 1-2x (10% per
/// iteration), 2-5x (25%), and 5x+ (50%). Each scenario must converge
/// below the prune threshold within the iteration bound.
#[test]
fn check_and_prune_converges_for_all_overshoot_levels() {
    set_test_db_limit();
    // (label, rows x 10KB) => ~1.5MB / ~3MB / ~6MB against a 1MB cap.
    for (label, rows) in [("low", 150), ("mid", 300), ("high", 600)] {
        let path = unique_size_test_db(label);
        let store = GenAISqliteStore::new_with_path(&path).unwrap();
        grow_db(&store, rows, 10 * 1024);
        let threshold = super::schema::get_prune_threshold();
        assert!(
            store.get_total_db_size() > threshold,
            "{label}: setup must exceed the prune threshold"
        );

        store.check_and_prune_if_needed().unwrap();

        // Convergence is on logical size: the physical file keeps its peak
        // size (freed pages stay on the freelist, #2888).
        assert!(
            store.effective_db_size() < threshold,
            "{label}: logical size must drop below threshold, got {} bytes",
            store.effective_db_size()
        );
        assert!(
            row_count(&store) < rows as i64,
            "{label}: oldest records must have been deleted"
        );
        drop(store);
        cleanup_size_test_db(&path);
    }
}

/// A no-op when the database is already within the threshold.
#[test]
fn check_and_prune_noop_below_threshold() {
    set_test_db_limit();
    let path = unique_size_test_db("noop");
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    grow_db(&store, 10, 64);

    store.check_and_prune_if_needed().unwrap();

    assert_eq!(
        row_count(&store),
        10,
        "below-threshold prune must not delete"
    );
    drop(store);
    cleanup_size_test_db(&path);
}

/// When another connection holds a read snapshot, the truncating WAL
/// checkpoint returns busy and the prune loop must stop instead of deleting
/// round after round against a size that cannot converge (only the WAL keeps
/// growing) — under the pre-fix behavior the loop deleted until the table was
/// empty.
#[test]
fn check_and_prune_stops_when_wal_checkpoint_busy() {
    set_test_db_limit();
    let path = unique_size_test_db("busy");
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    grow_db(&store, 300, 10 * 1024);
    store.wal_checkpoint().unwrap();

    // Hold a read snapshot on a separate connection: this blocks
    // `PRAGMA wal_checkpoint(TRUNCATE)` from completing (busy).
    let reader = rusqlite::Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT COUNT(*) FROM genai_events;")
        .unwrap();

    store.check_and_prune_if_needed().unwrap();

    let rows = row_count(&store);
    assert!(
        rows > 0,
        "prune must stop once the WAL cannot be truncated, not delete everything \
         (remaining: {rows})"
    );
    drop(reader);
    drop(store);
    cleanup_size_test_db(&path);
}

/// Reopening an oversized database triggers the startup cleanup path in
/// `new_with_path_and_batch`.
#[test]
fn startup_cleanup_prunes_oversized_db() {
    set_test_db_limit();
    let path = unique_size_test_db("startup");
    {
        let store = GenAISqliteStore::new_with_path(&path).unwrap();
        grow_db(&store, 300, 10 * 1024); // ~3MB > 1MB cap
    }

    let store = GenAISqliteStore::new_with_path(&path).unwrap();

    let threshold = super::schema::get_prune_threshold();
    assert!(
        store.effective_db_size() < threshold,
        "startup cleanup must bring the logical size below the threshold"
    );
    drop(store);
    cleanup_size_test_db(&path);
}

#[test]
fn agent_activity_summaries_group_names_and_aggregate_calls() {
    let path = unique_size_test_db("agent_activity");
    let store = GenAISqliteStore::new_with_path(&path).unwrap();
    {
        let conn = store.conn.lock().unwrap();
        // `provider` is set explicitly because the aggregated total depends on
        // it: Anthropic reports the cache counters outside input_tokens, so
        // they add to the total, while OpenAI-style gateways do not.
        conn.execute(
            "INSERT INTO genai_events
             (event_type, start_timestamp_ns, end_timestamp_ns, agent_name, provider,
              input_tokens, output_tokens, cache_read_tokens, event_json)
             VALUES ('llm_call', 100, 150, 'Claude', 'anthropic', 10, 5, 2, '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO genai_events
             (event_type, start_timestamp_ns, end_timestamp_ns, agent_name, provider,
              input_tokens, output_tokens, cache_creation_tokens, event_json)
             VALUES ('llm_call', 200, 0, 'claude', 'anthropic', 20, 10, 3, '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO genai_events
             (event_type, start_timestamp_ns, agent_name, input_tokens,
              output_tokens, event_json)
             VALUES ('tool_use', 300, 'Claude', 999, 999, '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO genai_events
             (event_type, start_timestamp_ns, process_name, provider, input_tokens,
              output_tokens, event_json)
             VALUES ('llm_call', 250, 'Codex', 'openai', 7, 3, '{}')",
            [],
        )
        .unwrap();
    }

    let summaries = store.list_agent_activity_summaries().unwrap();

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].agent_name, "Codex");
    assert_eq!(summaries[0].last_seen_ns, 250);
    assert_eq!(summaries[0].total_calls, 1);
    assert_eq!(summaries[0].total_tokens, 10);
    assert_eq!(summaries[1].agent_name, "Claude");
    assert_eq!(summaries[1].last_seen_ns, 200);
    assert_eq!(summaries[1].total_calls, 2);
    // Anthropic: (10 + 5 + 2) + (20 + 10 + 3).
    assert_eq!(summaries[1].total_tokens, 50);

    drop(store);
    cleanup_size_test_db(&path);
}
