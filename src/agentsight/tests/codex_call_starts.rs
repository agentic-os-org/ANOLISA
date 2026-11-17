//! Tool timing must use each rollout item's dispatch time, not its turn's first item.
use agentsight_opt::{AtifTrajectory, perf};
use agentsight_trajectory_collector::codex::convert_codex_events;
use serde_json::{Value, json};

fn event(second: u32, payload: Value) -> Value {
    json!({"type":"response_item", "timestamp":format!("2026-11-13T00:00:{second:02}Z"), "payload":payload})
}

fn call(second: u32, id: &str) -> Value {
    event(
        second,
        json!({"type":"function_call","name":"shell","call_id":id,"arguments":"{}"}),
    )
}

fn result(second: u32, id: &str) -> Value {
    event(
        second,
        json!({"type":"function_call_output","call_id":id,"output":"ok"}),
    )
}

fn stats(events: &[Value]) -> agentsight_opt::types::PerfStats {
    let doc = convert_codex_events(events, "codex").unwrap();
    let parsed = AtifTrajectory::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
    perf::compute_stats(&parsed).unwrap()
}

#[test]
fn reasoning_before_dispatch_is_not_tool_execution() {
    let stats = stats(&[
        event(
            0,
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]}),
        ),
        call(10, "one"),
        result(12, "one"),
    ]);
    assert_eq!(stats.tool_calls[0].start, 10.0);
    assert_eq!(stats.tool_calls[0].dur, 2.0);
    assert_eq!(stats.tool_secs, 2.0);
    assert_eq!(stats.wall_secs, 12.0);
}

#[test]
fn sequential_calls_do_not_claim_the_gap_between_them() {
    let stats = stats(&[
        call(0, "one"),
        result(2, "one"),
        call(10, "two"),
        result(13, "two"),
    ]);
    assert_eq!(
        stats.tool_calls.iter().map(|c| c.dur).collect::<Vec<_>>(),
        vec![2.0, 3.0]
    );
    assert_eq!(stats.tool_calls[1].start, 10.0);
    assert_eq!(stats.tool_secs, 5.0);
    assert_eq!(stats.idle_secs, 8.0);
}

#[test]
fn staggered_parallel_calls_contribute_their_union() {
    let stats = stats(&[
        call(0, "one"),
        call(2, "two"),
        result(5, "one"),
        result(8, "two"),
    ]);
    assert_eq!(
        stats.tool_calls.iter().map(|c| c.dur).collect::<Vec<_>>(),
        vec![5.0, 6.0]
    );
    assert_eq!(stats.tool_secs, 8.0);
}

#[test]
fn legacy_documents_keep_the_step_time_fallback() {
    let doc = AtifTrajectory::from_json(
        &json!({"schema_version":"ATIF-v1.7","steps":[
            {"step_id":1,"source":"agent","timestamp":"2026-11-13T00:00:00Z",
             "tool_calls":[{"tool_call_id":"old","function_name":"shell","arguments":{},
               "extra":{"result_timestamp":"2026-11-13T00:00:03Z"}}]}
        ]})
        .to_string(),
    )
    .unwrap();
    let stats = perf::compute_stats(&doc).unwrap();
    assert_eq!(stats.tool_calls[0].dur, 3.0);
    assert_eq!(stats.tool_secs, 3.0);
}

#[test]
fn an_unfinished_later_call_keeps_its_dispatch_without_borrowing_an_earlier_result() {
    let stats = stats(&[
        call(0, "finished"),
        result(2, "finished"),
        call(10, "pending"),
    ]);
    assert_eq!(stats.wall_secs, 10.0);
    assert_eq!(stats.tool_calls[1].start, 10.0);
    assert_eq!(stats.tool_calls[1].dur, 0.0);
    assert_eq!(stats.tool_secs, 2.0);
}

#[test]
fn persisted_session_end_includes_the_last_dispatch() {
    use agentsight_trajectory_collector::{
        CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
    };
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let events = [
        json!({"type":"session_meta","payload":{"id":"dispatch"}}),
        call(0, "done"),
        result(2, "done"),
        call(10, "pending"),
    ];
    std::fs::write(
        sessions.join("rollout-dispatch.jsonl"),
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&root.path().join("collection.db")).unwrap();
    scan_once(
        &store,
        &CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![sessions]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        },
    );
    let row = store.get("rollout-dispatch").unwrap().unwrap();
    assert_eq!(row.end_time.as_deref(), Some("2026-11-13T00:00:10Z"));
    let parsed = AtifTrajectory::from_json(&row.atif_json).unwrap();
    assert_eq!(perf::compute_stats(&parsed).unwrap().wall_secs, 10.0);
}
