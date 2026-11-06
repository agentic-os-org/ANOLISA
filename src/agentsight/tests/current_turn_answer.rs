#![cfg(feature = "server")]

use agentsight_opt::{atif::AtifTrajectory, trace::build_inventory};
use agentsight_trajectory_collector::atif::convert_qoder_events;
use serde_json::json;

fn inventory(tail: Vec<serde_json::Value>) -> agentsight_opt::trace::TraceInventory {
    let mut events = vec![
        json!({"type":"user", "message":{"content":"Fix the parser"}}),
        json!({"type":"assistant", "message":{"content":[{"type":"text","text":"Done, all tests passed."}]}}),
    ];
    events.extend(tail);
    let exported = convert_qoder_events(&events, "claude-code")
        .unwrap()
        .to_json_value()
        .unwrap();
    let trajectory = AtifTrajectory::from_json(&exported.to_string()).unwrap();
    build_inventory(&trajectory)
}

#[test]
fn unanswered_followup_does_not_reuse_previous_completion_claim() {
    let inv = inventory(vec![
        json!({"type":"user","message":{"content":"Now fix the database"}}),
    ]);
    assert_eq!(inv.user_turns.len(), 2);
    assert!(
        inv.final_answer.is_empty(),
        "the previous answer does not answer the new request: {}",
        inv.final_answer
    );
}

#[test]
fn interrupted_tool_turn_does_not_inherit_previous_success() {
    let inv = inventory(vec![
        json!({"type":"user","message":{"content":"Now fix the database"}}),
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"db","name":"Bash","input":{"command":"cargo test database"}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"db","is_error":true,"content":"database test failed"}]}}),
    ]);
    assert_eq!(inv.tool_calls.len(), 1);
    assert!(inv.tool_calls[0].err);
    assert!(inv.final_answer.is_empty());
}

#[test]
fn current_response_wins_and_tool_feedback_does_not_end_turn() {
    let inv = inventory(vec![
        json!({"type":"user","message":{"content":"Now fix the database"}}),
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"Database check failed; I could not finish."}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"missing","content":"late result"}]}}),
    ]);
    assert_eq!(
        inv.final_answer,
        "Database check failed; I could not finish."
    );
}

#[test]
fn injected_user_context_does_not_clear_a_real_answer() {
    for text in [
        "<system-reminder>context</system-reminder>",
        "<local-command-stdout>ok</local-command-stdout>",
        "  ",
    ] {
        let inv = inventory(vec![json!({"type":"user","message":{"content":text}})]);
        assert_eq!(inv.final_answer, "Done, all tests passed.");
    }
}

#[test]
fn mixed_tool_reply_and_real_followup_starts_an_unanswered_turn() {
    let inv = inventory(vec![json!({"type":"user","message":{"content":[
        {"type":"tool_result","tool_use_id":"missing","content":"external output"},
        {"type":"text","text":"Also check the migration"}
    ]}})]);
    assert_eq!(inv.user_turns.len(), 2);
    assert!(inv.final_answer.is_empty());
}
