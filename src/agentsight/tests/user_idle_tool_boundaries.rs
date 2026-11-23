//! User-idle reports must not mistake tool continuations for human pauses.
use agentsight_opt::{AtifTrajectory, perf::compute_stats};
use serde_json::{Value, json};

fn stats(mut first: Value, mut next: Value) -> agentsight_opt::types::PerfStats {
    first["step_id"] = json!(1);
    first["source"] = json!("agent");
    first["timestamp"] = json!("2026-11-08T00:00:01Z");
    first["extra"] = json!({"start_timestamp":"2026-11-08T00:00:00Z"});
    next["step_id"] = json!(2);
    next["source"] = json!("user");
    next["timestamp"] = json!("2026-11-08T00:01:31Z");
    let document = json!({"schema_version":"ATIF-v1.7", "session_id":"idle-fixture",
        "agent":{"name":"fixture","version":"1"}, "steps":[first,next]});
    compute_stats(&AtifTrajectory::from_json(&document.to_string()).unwrap()).unwrap()
}

fn reply() -> Value {
    json!({"message":"Finished the requested work."})
}

fn result_user(text: &str) -> Value {
    json!({"message":text,"observation":{"results":[
        {"source_call_id":"unrecorded","content":"retained tool output"}
    ]}})
}

#[test]
fn progress_text_with_pending_tool_is_not_a_finished_reply() {
    let first = json!({"message":"Running the build now.","tool_calls":[
        {"tool_call_id":"build","function_name":"Bash","arguments":{"command":"make"}}
    ]});
    let report = stats(first, json!({"message":"Cancel the build."}));
    assert!(report.idle_gaps.is_empty());
    assert_eq!(report.frag_idle_secs, 0.0);
}

#[test]
fn result_only_carrier_is_not_a_human_idle_endpoint() {
    assert!(stats(reply(), result_user("")).idle_gaps.is_empty());
}

#[test]
fn injected_context_is_not_a_human_idle_endpoint() {
    let next = json!({"message":"<system-reminder>background status</system-reminder>"});
    assert!(stats(reply(), next).idle_gaps.is_empty());
}

#[test]
fn a_real_reply_and_followup_still_report_idle() {
    let report = stats(reply(), json!({"message":"Now check the tests."}));
    assert_eq!(report.idle_gaps.len(), 1);
    assert_eq!(report.idle_gaps[0].dur, 90.0);
}

#[test]
fn mixed_result_and_real_user_text_remain_a_human_turn() {
    let report = stats(reply(), result_user("Please continue."));
    assert_eq!(report.idle_gaps.len(), 1);
    assert_eq!(report.idle_gaps[0].dur, 90.0);
}
