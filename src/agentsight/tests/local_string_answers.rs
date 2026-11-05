#![cfg(feature = "server")]

// Exercise the desktop implementation on Linux, as the identity regressions do.
// Its pre-existing legacy fallback predates the let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
mod converter;

use agentsight_atif::StepSource;
use agentsight_opt::{AtifTrajectory, trace::build_inventory};
use serde_json::{Value, json};

fn convert(events: &[Value]) -> agentsight_atif::AtifTrajectory {
    let text = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    std::fs::write(&path, &text).unwrap();
    let from_file = converter::convert_jsonl_to_atif(&path).unwrap();
    let from_content = converter::convert_jsonl_content_to_atif(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&from_file).unwrap(),
        serde_json::to_value(&from_content).unwrap()
    );
    from_file.validate_step_ids().unwrap();
    from_file
}

#[test]
fn string_answer_survives_local_file_loading_and_analysis() {
    let events = [
        json!({"type":"user","message":{"content":"inspect project"}}),
        json!({"type":"assistant","timestamp":"2026-01-01T00:00:05Z",
            "message":{"model":"fixture-model","content":"完成检查，保留所有记录。"}}),
    ];
    let local = convert(&events);
    let canonical =
        agentsight_trajectory_collector::atif::convert_qoder_events(&events, "claude-code")
            .unwrap();
    assert_eq!(local.steps[1].message, canonical.steps[1].message);
    assert_eq!(local.steps[1].message, "完成检查，保留所有记录。");
    assert_eq!(local.steps[1].model_name.as_deref(), Some("fixture-model"));
    assert_eq!(
        local.steps[1].timestamp.as_deref(),
        Some("2026-01-01T00:00:05Z")
    );
    let analysis = AtifTrajectory::from_json(&serde_json::to_string(&local).unwrap()).unwrap();
    assert_eq!(
        build_inventory(&analysis).final_answer,
        "完成检查，保留所有记录。"
    );
}

#[test]
fn block_tools_and_following_string_answer_are_both_preserved() {
    let local = convert(&[
        json!({"type":"user","message":{"content":"run test"}}),
        json!({"type":"assistant","message":{"content":[
            {"type":"thinking","thinking":"check the exit status"},
            {"type":"tool_use","id":"call-a","name":"Bash","input":{"command":"true"}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call-a","content":"passed"}]}}),
        json!({"type":"assistant","message":{"content":"All tests passed."}}),
    ]);
    assert_eq!(local.steps.len(), 3);
    assert_eq!(
        local.steps[1].reasoning_content.as_deref(),
        Some("check the exit status")
    );
    assert_eq!(
        local.steps[1].observation.as_ref().unwrap().results[0].content,
        Some(json!("passed"))
    );
    assert_eq!(local.steps[2].message, "All tests passed.");
    assert_eq!(local.steps[2].source, StepSource::Agent);
    assert!(local.steps[2].tool_calls.is_none());
}

#[test]
fn array_answers_keep_their_block_order_and_empty_answers_stay_empty() {
    let local = convert(&[
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}}),
        json!({"type":"user","message":{"content":"next"}}),
        json!({"type":"assistant","message":{"content":""}}),
    ]);
    assert_eq!(local.steps[0].message, "first\nsecond");
    assert!(local.steps[2].message.is_empty());
}

#[test]
fn collector_keeps_string_chunks_with_block_text_and_usage() {
    let events = [
        json!({"type":"user","message":{"content":"question"}}),
        json!({"type":"assistant","message":{"id":"reply","model":"fixture-model",
            "content":"first","usage":{"input_tokens":10,"output_tokens":2}}}),
        json!({"type":"assistant","message":{"id":"reply","content":[
            {"type":"thinking","thinking":"reason"},{"type":"text","text":"second"}],
            "usage":{"input_tokens":10,"output_tokens":4}}}),
        json!({"type":"assistant","message":{"id":"reply","content":"third",
            "usage":{"input_tokens":10,"output_tokens":6}}}),
    ];
    let trajectory =
        agentsight_trajectory_collector::atif::convert_qoder_events(&events, "claude-code")
            .unwrap();
    assert_eq!(trajectory.steps.len(), 2);
    let answer = &trajectory.steps[1];
    assert_eq!(answer.message, "first\nsecond\nthird");
    assert_eq!(answer.reasoning_content.as_deref(), Some("reason"));
    assert_eq!(answer.model_name.as_deref(), Some("fixture-model"));
    let metrics = answer.metrics.as_ref().unwrap();
    assert_eq!(metrics.prompt_tokens, Some(10));
    assert_eq!(metrics.completion_tokens, Some(6));
    trajectory.validate_step_ids().unwrap();
}
