//! System snapshots emitted during a session apply from that point onward.
use agentsight_opt::{AtifTrajectory, cost::compute_cost};
use serde_json::{Value, json};

fn systems(steps: Vec<Value>) -> Vec<usize> {
    let steps: Vec<Value> = steps
        .into_iter()
        .enumerate()
        .map(|(index, mut step)| {
            step["step_id"] = json!(index + 1);
            step
        })
        .collect();
    let trajectory =
        AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","steps":steps}).to_string())
            .unwrap();
    compute_cost(&trajectory)
        .unwrap()
        .calls
        .iter()
        .map(|call| call.system_prompt)
        .collect()
}

fn system(chars: usize) -> Value {
    json!({"source":"system","message":"s".repeat(chars),"extra":{"system_prompt_snapshot":true}})
}
fn agent() -> Value {
    json!({"source":"agent","message":"done"})
}

#[test]
fn a_later_instruction_snapshot_does_not_change_earlier_calls() {
    assert_eq!(
        systems(vec![system(400), agent(), system(800), agent()]),
        vec![100, 200]
    );
}

#[test]
fn repeating_a_request_prompt_does_not_accumulate_copies() {
    assert_eq!(
        systems(vec![
            system(400),
            agent(),
            system(400),
            agent(),
            system(400),
            agent()
        ]),
        vec![100, 100, 100]
    );
}

#[test]
fn an_instruction_added_later_does_not_leak_into_the_first_request() {
    assert_eq!(systems(vec![agent(), system(400), agent()]), vec![0, 100]);
}

#[test]
fn consecutive_instruction_blocks_share_one_request_prefix() {
    assert_eq!(
        systems(vec![
            system(400),
            json!({"source":"system","message":"s".repeat(200)}),
            agent(),
            agent()
        ]),
        vec![150, 150]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_session_export_prompt_replacement_reaches_cost_analysis() {
    use agentsight::atif::convert_session_to_atif;
    use agentsight::storage::sqlite::genai::TraceEventDetail;
    let events = [400, 800]
        .into_iter()
        .enumerate()
        .map(|(i, chars)| TraceEventDetail {
            id: i as i64,
            call_id: Some(format!("call-{i}")),
            start_timestamp_ns: 1_800_000_000_000_000_000 + i as i64 * 2_000_000_000,
            end_timestamp_ns: None,
            model: Some("fixture".into()),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: None,
            system_instructions: Some(json!("s".repeat(chars)).to_string()),
            agent_name: Some("fixture".into()),
            process_name: None,
            pid: Some(1),
            user_query: Some(format!("question {i}")),
            event_json: None,
            trace_id: Some(format!("trace-{i}")),
            conversation_id: Some(format!("turn-{i}")),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        })
        .collect();
    let doc = convert_session_to_atif("session", events).unwrap();
    let trajectory = AtifTrajectory::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
    let cost = compute_cost(&trajectory).unwrap();
    assert_eq!(
        cost.calls
            .iter()
            .map(|c| c.system_prompt)
            .collect::<Vec<_>>(),
        vec![100, 200]
    );
}

#[test]
fn ordinary_system_messages_remain_additive_and_do_not_leak_backwards() {
    assert_eq!(
        systems(vec![
            json!({"source":"system","message":"s".repeat(400)}),
            agent(),
            json!({"source":"system","message":"s".repeat(200)}),
            agent(),
        ]),
        vec![100, 150]
    );
}

#[test]
fn explicit_empty_snapshot_clears_the_previous_prompt() {
    assert_eq!(
        systems(vec![system(400), agent(), system(0), agent()]),
        vec![100, 0]
    );
}

#[test]
fn recorded_prompt_replacement_preserves_measured_usage_calibration() {
    use agentsight_opt::llm::{ChatMessage, RecordParams, TrajectoryRecorder};
    let recorder = TrajectoryRecorder::new("fixture-model", "fixture-analysis");
    for (chars, input_tokens) in [(400, 100), (800, 210)] {
        recorder.record(RecordParams {
            messages: &[
                ChatMessage::system("s".repeat(chars)),
                ChatMessage::user(""),
            ],
            response: "done",
            reasoning: None,
            model: "fixture-model",
            input_tokens,
            output_tokens: 10,
            cached_tokens: 0,
            reasoning_tokens: 0,
            start_ts: "2026-11-12T00:00:00Z",
            end_ts: "2026-11-12T00:00:01Z",
            label: None,
        });
    }
    let doc = recorder.to_atif();
    let parsed = AtifTrajectory::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    // These complete requests contain no previous answer. Calibration must
    // assign the measured input to the current prompt, not inherited history.
    assert_eq!(cost.calls.len(), 2);
    assert!(cost.calls.iter().all(|call| call.assistant_messages == 0));
    assert_eq!(
        cost.calls
            .iter()
            .map(|call| call.system_prompt)
            .collect::<Vec<_>>(),
        vec![100, 210]
    );
    assert_eq!(
        cost.calls
            .iter()
            .map(|call| call.real_prompt_tokens)
            .collect::<Vec<_>>(),
        vec![Some(100), Some(210)]
    );
}
