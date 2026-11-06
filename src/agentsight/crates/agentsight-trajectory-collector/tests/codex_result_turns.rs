//! Steering while a tool is running must not change ownership of its output.

use agentsight_atif::StepSource;
use agentsight_trajectory_collector::codex::{convert_codex_events, extract_private_metadata};
use serde_json::{json, Value};

fn event(kind: &str, payload: Value) -> Value {
    json!({"type":kind,"timestamp":"2026-01-01T00:00:00Z","payload":payload})
}

#[test]
fn late_output_is_attached_to_its_issuing_turn() {
    let events = vec![
        event(
            "event_msg",
            json!({"type":"user_message","message":"run the checks"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call","call_id":"first","name":"exec","arguments":"{}"}),
        ),
        event(
            "event_msg",
            json!({"type":"user_message","message":"also inspect the docs"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call","call_id":"second","name":"exec","arguments":"{}"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call_output","call_id":"first","output":"first output"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call_output","call_id":"second","output":"second output"}),
        ),
    ];
    let trajectory = convert_codex_events(&events, "codex").unwrap();
    let agents: Vec<_> = trajectory
        .steps
        .iter()
        .filter(|s| s.source == StepSource::Agent)
        .collect();
    assert_eq!(agents.len(), 2);
    assert_eq!(
        agents[0].observation.as_ref().map(|o| o.results.len()),
        Some(1)
    );
    assert_eq!(
        agents[0].observation.as_ref().unwrap().results[0]
            .source_call_id
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        agents[1].observation.as_ref().unwrap().results[0]
            .source_call_id
            .as_deref(),
        Some("second")
    );
}

#[test]
fn late_output_does_not_invent_another_agent_turn() {
    let events = vec![
        event(
            "response_item",
            json!({"type":"custom_tool_call","call_id":"first","name":"exec","input":"pwd"}),
        ),
        event("event_msg", json!({"type":"user_message","message":"wait"})),
        event(
            "response_item",
            json!({"type":"custom_tool_call_output","call_id":"first","output":"/fixture"}),
        ),
    ];
    let trajectory = convert_codex_events(&events, "codex").unwrap();
    assert_eq!(trajectory.steps.len(), 2);
    assert_eq!(
        extract_private_metadata(&events, "fixture")["assistant_message_count"],
        1
    );
}

#[test]
fn command_failure_survives_modern_and_legacy_user_boundaries() {
    for user in [
        event(
            "event_msg",
            json!({"type":"user_message","message":"continue"}),
        ),
        event(
            "response_item",
            json!({"type":"message","role":"user",
            "content":[{"type":"input_text","text":"continue"}]}),
        ),
    ] {
        for (call_type, output_type) in [
            ("custom_tool_call", "custom_tool_call_output"),
            ("function_call", "function_call_output"),
            ("local_shell_call", "local_shell_call_output"),
        ] {
            let events = vec![
                event(
                    "response_item",
                    json!({"type":call_type,"call_id":"first","name":"exec","input":"false","arguments":"{}"}),
                ),
                user.clone(),
                event(
                    "event_msg",
                    json!({"type":"item_completed","item":{
                    "type":"CommandExecution","status":"completed","exit_code":1}}),
                ),
                event(
                    "response_item",
                    json!({"type":output_type,"call_id":"first","output":"diagnostic"}),
                ),
            ];
            let trajectory = convert_codex_events(&events, "codex").unwrap();
            assert_eq!(trajectory.steps.len(), 2);
            let result = &trajectory.steps[0].observation.as_ref().unwrap().results[0];
            assert_eq!(result.content.as_ref().unwrap(), "diagnostic");
            assert_eq!(result.extra.as_ref().unwrap()["is_error"], true);
            assert_eq!(
                extract_private_metadata(&events, "fixture")["assistant_message_count"],
                1
            );
        }
    }
}

#[test]
fn same_turn_and_unknown_results_keep_the_existing_behavior() {
    let events = vec![
        event(
            "response_item",
            json!({"type":"function_call","call_id":"known","name":"exec","arguments":"{}"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call_output","call_id":"known","output":"ok"}),
        ),
        event("event_msg", json!({"type":"user_message","message":"next"})),
        event(
            "response_item",
            json!({"type":"function_call_output","call_id":"orphan","output":"preserved"}),
        ),
    ];
    let trajectory = convert_codex_events(&events, "codex").unwrap();
    assert_eq!(trajectory.steps.len(), 3);
    assert_eq!(
        trajectory.steps[0].observation.as_ref().unwrap().results[0]
            .source_call_id
            .as_deref(),
        Some("known")
    );
    assert_eq!(
        trajectory.steps[2].observation.as_ref().unwrap().results[0]
            .source_call_id
            .as_deref(),
        Some("orphan")
    );
    assert_eq!(
        extract_private_metadata(&events, "fixture")["assistant_message_count"],
        2
    );
}

#[test]
fn reused_id_belongs_to_the_most_recent_call() {
    let events = vec![
        event(
            "response_item",
            json!({"type":"function_call","call_id":"same","name":"exec","arguments":"{}"}),
        ),
        event(
            "event_msg",
            json!({"type":"user_message","message":"retry"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call","call_id":"same","name":"exec","arguments":"{}"}),
        ),
        event(
            "response_item",
            json!({"type":"function_call_output","call_id":"same","output":"new result"}),
        ),
    ];
    let trajectory = convert_codex_events(&events, "codex").unwrap();
    assert!(trajectory.steps[0].observation.is_none());
    assert_eq!(
        trajectory.steps[2].observation.as_ref().unwrap().results[0]
            .content
            .as_ref()
            .unwrap(),
        "new result"
    );
    assert_eq!(
        extract_private_metadata(&events, "fixture")["assistant_message_count"],
        2
    );
}
