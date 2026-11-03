//! Partial tool observations must not prove that every recorded call failed.

use agentsight_opt::{atif::AtifTrajectory, cost::compute_cost};
use serde_json::{json, Value};

fn trajectory(results: Value) -> AtifTrajectory {
    let shared: agentsight_atif::AtifTrajectory = serde_json::from_value(json!({
        "schema_version":"ATIF-v1.7","session_id":"fixture-session",
        "agent":{"name":"fixture","version":"1"},"steps":[
            {"step_id":1,"source":"user","message":"ordinary fixture instruction"},
            {"step_id":2,"source":"agent","message":"", "tool_calls":[
                {"tool_call_id":"call-a","function_name":"Read","arguments":{"file_path":"/fixture/a"}},
                {"tool_call_id":"call-b","function_name":"Read","arguments":{"file_path":"/fixture/b"}}
            ],"observation":{"results":results},
             "metrics":{"prompt_tokens":1000,"completion_tokens":10}}
        ]
    })).unwrap();
    shared.validate_step_ids().unwrap();
    AtifTrajectory::from_json(&serde_json::to_string(&shared).unwrap()).unwrap()
}

fn failed(id: &str) -> Value {
    json!({"source_call_id":id,"content":"recorded failure","extra":{"is_error":true}})
}

#[test]
fn one_failed_result_does_not_make_the_two_call_turn_removable() {
    let cost = compute_cost(&trajectory(json!([failed("call-a")]))).unwrap();
    assert!(
        !cost.calls[0].removable_turn,
        "call-b has no result, so its failure is unknown"
    );
    assert_eq!(cost.headroom.orch_savable_tok, 0);
}

#[test]
fn an_unmatched_failure_does_not_prove_any_of_the_recorded_calls_failed() {
    let cost = compute_cost(&trajectory(json!([failed("different-call")]))).unwrap();
    assert!(!cost.calls[0].removable_turn);
}

#[test]
fn both_matched_failures_still_make_the_turn_removable() {
    let cost = compute_cost(&trajectory(json!([failed("call-b"), failed("call-a")]))).unwrap();
    assert!(cost.calls[0].removable_turn);
}

#[test]
fn a_matched_success_prevents_the_failed_turn_classification() {
    let cost = compute_cost(&trajectory(json!([failed("call-a"), {
        "source_call_id":"call-b","content":"recorded success","extra":{"is_error":false}
    }])))
    .unwrap();
    assert!(!cost.calls[0].removable_turn);
}

#[test]
fn legacy_idless_complete_failures_keep_positional_pairing() {
    let result = json!({"content":"recorded failure","extra":{"is_error":true}});
    let cost = compute_cost(&trajectory(json!([result.clone(), result]))).unwrap();
    assert!(cost.calls[0].removable_turn);
}

#[test]
fn legacy_idless_partial_failure_does_not_prove_the_second_call_failed() {
    let cost = compute_cost(&trajectory(json!([{
        "content":"recorded failure","extra":{"is_error":true}
    }])))
    .unwrap();
    assert!(!cost.calls[0].removable_turn);
}

#[test]
fn tolerated_echoed_ids_still_match_the_recorded_calls() {
    let cost = compute_cost(&trajectory(json!([failed("callb"), failed("CALLA")]))).unwrap();
    assert!(cost.calls[0].removable_turn);
}

#[test]
fn duplicate_failure_result_does_not_cover_a_different_missing_call() {
    let cost = compute_cost(&trajectory(json!([failed("call-a"), failed("call-a")]))).unwrap();
    assert!(!cost.calls[0].removable_turn);
}

#[test]
fn any_recorded_success_keeps_the_existing_conservative_classification() {
    let cost = compute_cost(&trajectory(json!([failed("call-a"), failed("call-b"), {
        "source_call_id":"call-a","content":"later recorded success","extra":{"is_error":false}
    }])))
    .unwrap();
    assert!(!cost.calls[0].removable_turn);
}
