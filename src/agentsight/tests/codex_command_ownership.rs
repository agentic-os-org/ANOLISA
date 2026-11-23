//! Codex CommandExecution IDs identify the call even when commands overlap.
use agentsight_opt::{AtifTrajectory, trace::build_inventory};
use agentsight_trajectory_collector::codex::convert_codex_events;
use serde_json::{Value, json};

fn response(payload: Value) -> Value {
    json!({"type":"response_item","payload":payload})
}
fn call(id: &str) -> Value {
    response(json!({"type":"function_call","call_id":id,"name":"exec_command","arguments":"{}"}))
}
fn output(id: &str) -> Value {
    response(json!({"type":"function_call_output","call_id":id,"output":"command output"}))
}
fn completion(id: &str, status: &str, exit: Option<i32>) -> Value {
    json!({"type":"event_msg","payload":{"type":"item_completed","item":{
        "type":"CommandExecution","id":id,"status":status,"exit_code":exit
    }}})
}
fn failures(events: &[Value]) -> Vec<(String, bool)> {
    let trajectory = convert_codex_events(events, "codex").unwrap();
    let analysis = AtifTrajectory::from_json(&serde_json::to_string(&trajectory).unwrap()).unwrap();
    build_inventory(&analysis)
        .tool_calls
        .into_iter()
        .map(|call| (call.call_id, call.err))
        .collect()
}

#[test]
fn parallel_completion_uses_its_id_instead_of_the_last_open_call() {
    assert_eq!(
        failures(&[
            call("first"),
            call("second"),
            completion("first", "failed", Some(1)),
            output("first"),
            completion("second", "completed", Some(0)),
            output("second")
        ]),
        [("first".into(), true), ("second".into(), false)]
    );
}

#[test]
fn a_declined_command_must_not_override_failure_detection_with_success() {
    assert_eq!(
        failures(&[
            call("rejected"),
            completion("rejected", "declined", None),
            output("rejected")
        ]),
        [("rejected".into(), true)]
    );
}

#[test]
fn independent_success_and_failure_keep_their_verdicts() {
    assert_eq!(
        failures(&[
            call("first"),
            completion("first", "completed", Some(0)),
            output("first"),
            call("second"),
            completion("second", "failed", Some(2)),
            output("second")
        ]),
        [("first".into(), false), ("second".into(), true)]
    );
}

#[test]
fn an_unidentified_command_does_not_guess_among_parallel_calls() {
    let events = [
        call("first"),
        call("second"),
        completion("unrecorded", "failed", Some(1)),
        output("second"),
        output("first"),
    ];
    let trajectory = serde_json::to_value(convert_codex_events(&events, "codex").unwrap()).unwrap();
    for result in trajectory["steps"][0]["observation"]["results"]
        .as_array()
        .unwrap()
    {
        assert!(result["extra"]["is_error"].is_null());
    }
}
