//! Local exports must retain recorded tool completion times for offline analysis.
#[allow(clippy::collapsible_if)] // Existing desktop-only converter fallback.
#[path = "../src/local/collector/converter.rs"]
mod converter;
use agentsight_opt::{AnalyzePipeline, AtifTrajectory};
use converter::convert_jsonl_to_atif;
use serde_json::{Value, json};

fn event(kind: &str, second: u32, content: Value) -> Value {
    json!({"type":kind,"timestamp":format!("2026-01-01T00:00:{second:02}Z"),
        "sessionId":"local-timing","message":{"content":content}})
}

fn call(id: &str) -> Value {
    json!({"type":"tool_use","id":id,"name":"Bash","input":{"command":"pwd"}})
}

fn result(id: &str) -> Value {
    json!({"type":"tool_result","tool_use_id":id,"content":"/project"})
}

fn convert(events: Vec<Value>) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(
        &path,
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    serde_json::to_value(convert_jsonl_to_atif(&path).unwrap()).unwrap()
}

fn perf(document: &Value) -> agentsight_opt::types::PerfStats {
    AnalyzePipeline::run_perf(&AtifTrajectory::from_json(&document.to_string()).unwrap()).unwrap()
}

#[test]
fn terminal_result_keeps_session_end_and_tool_duration() {
    let doc = convert(vec![
        event("user", 0, json!("inspect")),
        event("assistant", 2, json!([call("one")])),
        event("user", 7, json!([result("one")])),
    ]);
    let stats = perf(&doc);
    assert_eq!(stats.wall_secs, 7.0);
    assert_eq!(stats.tool_calls[0].dur, 5.0);
    assert_eq!(
        doc["steps"].as_array().unwrap().len(),
        2,
        "results do not invent model turns"
    );
}

#[test]
fn independent_results_stop_each_tool_before_the_following_answer() {
    let doc = convert(vec![
        event("assistant", 2, json!([call("one"), call("two")])),
        event("user", 4, json!([result("one")])),
        event(
            "user",
            7,
            json!([result("two"), {"type":"text","text":"also inspect docs"}]),
        ),
        event("assistant", 20, json!([{"type":"text","text":"done"}])),
    ]);
    let stats = perf(&doc);
    assert_eq!(
        stats
            .tool_calls
            .iter()
            .map(|call| call.dur)
            .collect::<Vec<_>>(),
        vec![2.0, 5.0]
    );
    assert_eq!(doc["steps"][1]["message"], "also inspect docs");
}

#[test]
fn a_late_result_updates_its_owner_instead_of_the_latest_agent() {
    let doc = convert(vec![
        event("assistant", 1, json!([call("old")])),
        event("assistant", 3, json!([call("new")])),
        event("user", 8, json!([result("old")])),
        event("user", 9, json!([result("new")])),
    ]);
    assert_eq!(
        doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"],
        "2026-01-01T00:00:08Z"
    );
    assert_eq!(
        doc["steps"][1]["tool_calls"][0]["extra"]["result_timestamp"],
        "2026-01-01T00:00:09Z"
    );
}

#[test]
fn missing_timestamps_and_unmatched_results_do_not_fabricate_times() {
    let mut without_time = event("user", 8, json!([result("one")]));
    without_time.as_object_mut().unwrap().remove("timestamp");
    let doc = convert(vec![
        event("assistant", 1, json!([call("one")])),
        without_time,
        event("user", 12, json!([result("unknown")])),
    ]);
    assert!(doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"].is_null());
    assert_eq!(
        doc["steps"][0]["observation"]["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn provider_echoes_keep_canonical_owners_and_ambiguous_echoes_change_neither_call() {
    let doc = convert(vec![
        event("assistant", 1, json!([call("call_one")])),
        event("user", 4, json!([result("callone")])),
    ]);
    assert_eq!(
        doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"],
        "2026-01-01T00:00:04Z"
    );
    assert_eq!(
        doc["steps"][0]["observation"]["results"][0]["source_call_id"],
        "call_one"
    );

    let ambiguous = convert(vec![
        event("assistant", 1, json!([call("call_a-1"), call("call_a_1")])),
        event("user", 8, json!([result("calla1")])),
    ]);
    for call in ambiguous["steps"][0]["tool_calls"].as_array().unwrap() {
        assert!(call["extra"]["result_timestamp"].is_null());
    }
    assert!(ambiguous["steps"][0]["observation"].is_null());
}
