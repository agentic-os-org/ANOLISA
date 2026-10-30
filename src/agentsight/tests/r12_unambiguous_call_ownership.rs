use agentsight_opt::{AtifTrajectory, cost, perf};
use agentsight_trajectory_collector::atif::convert_qoder_events;
use serde_json::{Value, json};

fn analyze(ids: &[&str], echo: &str) -> AtifTrajectory {
    let calls: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            json!({"tool_call_id":id,
        "function_name":format!("tool{i}"),"arguments":{"path":format!("file{i}")}})
        })
        .collect();
    AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","agent":{"name":"fixture","version":"1"},
        "steps":[{"step_id":1,"source":"agent","timestamp":"2026-01-01T00:00:00Z","tool_calls":calls,
        "observation":{"results":[{"source_call_id":echo,"content":"failed","extra":{"is_error":true}}]}},
        {"step_id":2,"source":"agent","message":"done","timestamp":"2026-01-01T00:00:01Z"}]}).to_string()).unwrap()
}

#[test]
fn exact_failure_does_not_mark_a_similarly_named_sibling_failed() {
    for (ids, expected) in [
        (["call_x", "callx"], [false, true]),
        (["callx", "call_x"], [true, false]),
        (["CALL", "call"], [false, true]),
    ] {
        let echo = if ids[0] == "CALL" { "call" } else { "callx" };
        let trajectory = analyze(&ids, echo);
        let stats = perf::compute_stats(&trajectory).unwrap();
        assert_eq!(
            stats.tool_calls.iter().map(|c| c.err).collect::<Vec<_>>(),
            expected
        );
        assert!(
            !cost::compute_cost(&trajectory).unwrap().calls[0].removable_turn,
            "one failed call cannot cover both parallel calls"
        );
    }
}

#[test]
fn ambiguous_echo_is_unattributed_but_unique_echo_is_preserved() {
    let ambiguous = analyze(&["call_x", "call-x"], "callx");
    assert!(
        perf::compute_stats(&ambiguous)
            .unwrap()
            .tool_calls
            .iter()
            .all(|c| !c.err)
    );
    assert!(!cost::compute_cost(&ambiguous).unwrap().calls[0].removable_turn);
    let unique = analyze(&["call_x"], "callx");
    assert!(perf::compute_stats(&unique).unwrap().tool_calls[0].err);
    assert!(cost::compute_cost(&unique).unwrap().calls[0].removable_turn);
}

fn call(id: &str) -> Value {
    json!({"type":"assistant","message":{"content":[
    {"type":"tool_use","id":id,"name":"Read","input":{"path":id}}]}})
}

#[test]
fn collector_chooses_exact_older_owner_before_newer_alias() {
    let events = [
        call("callx"),
        json!({"type":"user","message":{"content":"continue"}}),
        call("call_x"),
        json!({"type":"user","timestamp":"2026-01-01T00:00:02Z","message":{"content":[
            {"type":"tool_result","tool_use_id":"callx","content":"old result"},
            {"type":"text","text":"additional instruction"}]}}),
    ];
    let trajectory = convert_qoder_events(&events, "claude-code").unwrap();
    assert_eq!(
        trajectory.steps[0].observation.as_ref().unwrap().results[0].content,
        Some(json!("old result"))
    );
    assert!(trajectory.steps[2].observation.is_none());
    assert_eq!(
        trajectory.steps[0].tool_calls.as_ref().unwrap()[0]
            .extra
            .as_ref()
            .unwrap()["result_timestamp"],
        json!("2026-01-01T00:00:02Z")
    );
    assert!(
        trajectory.steps[2].tool_calls.as_ref().unwrap()[0]
            .extra
            .as_ref()
            .is_none_or(|e| !e.contains_key("result_timestamp"))
    );
}

#[test]
fn collector_does_not_guess_between_two_aliases() {
    let events = [
        call("call_x"),
        json!({"type":"user","message":{"content":"continue"}}),
        call("call-x"),
        json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"callx","content":"unowned"},
            {"type":"text","text":"additional instruction"}]}}),
    ];
    let trajectory = convert_qoder_events(&events, "claude-code").unwrap();
    assert!(trajectory.steps[0].observation.is_none());
    assert!(trajectory.steps[2].observation.is_none());
    assert_eq!(
        trajectory.steps[3].observation.as_ref().unwrap().results[0].content,
        Some(json!("unowned")),
        "keep mixed user replay context without inventing ownership"
    );
}

#[test]
fn exported_exact_reply_leaves_the_idless_reply_for_its_actual_sibling() {
    use agentsight::atif::converter::convert_trace_to_atif;
    use agentsight::storage::sqlite::genai::TraceEventDetail;
    let event = |id: i64, input, output| TraceEventDetail {
        id,
        call_id: Some(format!("call-{id}")),
        start_timestamp_ns: id * 1_000_000_000,
        end_timestamp_ns: Some(id * 1_000_000_000 + 1),
        model: Some("fixture".into()),
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        input_messages: input,
        output_messages: output,
        system_instructions: None,
        agent_name: Some("fixture".into()),
        process_name: None,
        pid: Some(1),
        user_query: None,
        event_json: None,
        trace_id: Some("trace".into()),
        conversation_id: None,
        cache_read_tokens: None,
        status: Some("complete".into()),
        interruption_type: None,
    };
    // Repeat both orders because the exporter uses a HashMap of original IDs.
    for ids in [["call_x", "callx"], ["callx", "call_x"]] {
        let output = json!([{"role":"assistant","parts":ids.iter().map(|id|json!({
            "type":"tool_call","id":id,"name":"Read","arguments":{"path":id}})).collect::<Vec<_>>(),
            "finish_reason":"tool_call"}])
        .to_string();
        let input = json!([{"role":"tool","parts":[
            {"type":"tool_call_response","id":"callx","response":"exact"},
            {"type":"tool_call_response","response":"idless"}]}])
        .to_string();
        let trajectory = convert_trace_to_atif(
            "trace",
            vec![event(1, None, Some(output)), event(2, Some(input), None)],
        )
        .unwrap();
        let results = &trajectory.steps[0].observation.as_ref().unwrap().results;
        assert_eq!(results.len(), 2);
        for (id, expected) in [("callx", "exact"), ("call_x", "idless")] {
            assert_eq!(
                results
                    .iter()
                    .find(|r| r.source_call_id.as_deref() == Some(id))
                    .unwrap()
                    .content,
                Some(json!(expected))
            );
        }
    }
}
