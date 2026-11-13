//! JSON-repair history is input context, not a second execution of old calls.
use agentsight_opt::{
    AtifTrajectory,
    cost::compute_cost,
    llm::{ChatMessage, RecordParams, TrajectoryRecorder},
    perf::compute_stats,
};

fn record(
    recorder: &TrajectoryRecorder,
    messages: &[ChatMessage],
    response: &str,
    start: &str,
    end: &str,
) {
    recorder.record(RecordParams {
        messages,
        response,
        reasoning: None,
        model: "fixture",
        input_tokens: 100,
        output_tokens: 10,
        cached_tokens: 0,
        reasoning_tokens: 0,
        start_ts: start,
        end_ts: end,
        label: None,
    });
}

#[test]
fn a_json_repair_does_not_replay_the_failed_answer_as_an_llm_call() {
    let recorder = TrajectoryRecorder::new("fixture", "repair");
    let prompt = "p".repeat(400);
    let broken = "a".repeat(200);
    record(
        &recorder,
        &[ChatMessage::user(&prompt)],
        &broken,
        "2026-11-13T00:00:00Z",
        "2026-11-13T00:00:01Z",
    );
    record(
        &recorder,
        &[
            ChatMessage::user(&prompt),
            ChatMessage {
                role: "assistant".into(),
                content: broken,
            },
            ChatMessage::user("r".repeat(80)),
        ],
        "fixed",
        "2026-11-13T00:00:05Z",
        "2026-11-13T00:00:07Z",
    );
    let mut parsed =
        AtifTrajectory::from_json(&serde_json::to_string(&recorder.to_atif()).unwrap()).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.calls.len(), 2, "there were only two provider requests");
    assert_eq!(cost.total_real_output_tok, 20);
    let perf = compute_stats(&parsed).unwrap();
    assert_eq!(
        perf.model_secs, 3.0,
        "replayed history must not create inference time"
    );
    for step in &mut parsed.steps {
        if let Some(m) = &mut step.metrics {
            m.prompt_tokens = None;
            m.completion_tokens = None;
        }
    }
    let uncalibrated = compute_cost(&parsed).unwrap();
    assert_eq!(uncalibrated.calls[1].user_messages, 120);
    assert_eq!(uncalibrated.calls[1].assistant_messages, 50);
}

#[test]
fn independent_recorded_requests_do_not_inherit_each_others_history() {
    let recorder = TrajectoryRecorder::new("fixture", "independent");
    for prompt in ["a".repeat(400), "b".repeat(800)] {
        record(
            &recorder,
            &[ChatMessage::user(prompt)],
            "done",
            "2026-11-13T00:00:00Z",
            "2026-11-13T00:00:01Z",
        );
    }
    let mut parsed =
        AtifTrajectory::from_json(&serde_json::to_string(&recorder.to_atif()).unwrap()).unwrap();
    for step in &mut parsed.steps {
        if let Some(m) = &mut step.metrics {
            m.prompt_tokens = None;
            m.completion_tokens = None;
        }
    }
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.calls[1].user_messages, 200);
    assert_eq!(cost.calls[1].assistant_messages, 0);
}

#[test]
fn copied_tool_history_is_context_without_fresh_execution_evidence() {
    use serde_json::json;
    let parsed = AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","steps":[
        {"step_id":1,"source":"agent","is_copied_context":true,"timestamp":"2026-10-01T00:00:00Z",
         "message":"old answer","tool_calls":[{"tool_call_id":"old","function_name":"shell","arguments":{"command":"old command"}}],
         "metrics":{"prompt_tokens":500,"completion_tokens":100}},
        {"step_id":2,"source":"user","message":"new question","timestamp":"2026-11-13T00:00:00Z"},
        {"step_id":3,"source":"agent","message":"new answer","timestamp":"2026-11-13T00:00:02Z",
         "extra":{"start_timestamp":"2026-11-13T00:00:00Z"},"metrics":{"prompt_tokens":20,"completion_tokens":5}}
    ]}).to_string()).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.calls.len(), 1);
    assert_eq!(cost.total_real_input_tok, 20);
    assert_eq!(cost.total_real_output_tok, 5);
    let perf = compute_stats(&parsed).unwrap();
    assert_eq!(perf.wall_secs, 2.0);
    assert!(perf.tool_calls.is_empty());
    let inventory = agentsight_opt::trace::build_inventory(&parsed);
    assert_eq!(inventory.final_answer, "new answer");
    assert!(inventory.tool_calls.is_empty());
    let candidates = agentsight_opt::cost::extract_waste_candidates(&parsed).unwrap();
    assert_eq!(candidates.total_steps, 1);
    assert!(agentsight_opt::atif::render_trimmed(&parsed).contains("copied request context"));
}

#[test]
fn legacy_incremental_trajectories_keep_their_accumulated_history() {
    use serde_json::json;
    let parsed = AtifTrajectory::from_json(
        &json!({"schema_version":"ATIF-v1.7","steps":[
            {"step_id":1,"source":"user","message":"u".repeat(400)},
            {"step_id":2,"source":"agent","message":"a".repeat(200)},
            {"step_id":3,"source":"user","message":"v".repeat(80)},
            {"step_id":4,"source":"agent","message":"done"}
        ]})
        .to_string(),
    )
    .unwrap();
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.calls.len(), 2);
    assert_eq!(cost.calls[1].user_messages, 120);
    assert_eq!(cost.calls[1].assistant_messages, 50);
}

#[test]
fn a_request_with_only_system_input_also_clears_conversation_history() {
    let recorder = TrajectoryRecorder::new("fixture", "system-only");
    record(
        &recorder,
        &[ChatMessage::user("u".repeat(400))],
        "first",
        "2026-11-13T00:00:00Z",
        "2026-11-13T00:00:01Z",
    );
    record(
        &recorder,
        &[ChatMessage::system("s".repeat(80))],
        "second",
        "2026-11-13T00:00:05Z",
        "2026-11-13T00:00:07Z",
    );
    let mut parsed =
        AtifTrajectory::from_json(&serde_json::to_string(&recorder.to_atif()).unwrap()).unwrap();
    for step in &mut parsed.steps {
        if let Some(m) = &mut step.metrics {
            m.prompt_tokens = None;
        }
    }
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.calls[1].user_messages, 0);
    assert_eq!(cost.calls[1].assistant_messages, 0);
    assert_eq!(cost.calls[1].system_prompt, 20);
}

#[test]
fn copied_user_history_does_not_start_a_new_user_turn() {
    use serde_json::json;
    let parsed = AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","steps":[
        {"step_id":1,"source":"user","message":"Inspect the fixture"},
        {"step_id":2,"source":"agent","tool_calls":[{"tool_call_id":"first","function_name":"shell","arguments":{"command":"cat fixture"}}]},
        {"step_id":3,"source":"user","is_copied_context":true,"message":"Inspect the fixture"},
        {"step_id":4,"source":"agent","tool_calls":[{"tool_call_id":"second","function_name":"shell","arguments":{"command":"cat fixture"}}]}
    ]}).to_string()).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(cost.calls.len(), 2);
    assert!(!cost.calls[0].removable_turn);
    assert!(
        cost.calls[1].removable_turn,
        "copied input does not authorize a fresh repeat"
    );
}
