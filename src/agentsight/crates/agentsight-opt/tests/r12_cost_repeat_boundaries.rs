use agentsight_opt::{cost, AtifTrajectory};
use serde_json::{json, Value};

fn call(name: &str, arguments: Value) -> Value {
    json!({"tool_call_id":name,"function_name":name,"arguments":arguments})
}
fn agent(calls: Vec<Value>) -> Value {
    json!({"source":"agent","message":"Checking the requested files.","tool_calls":calls})
}
fn trajectory(mut steps: Vec<Value>) -> AtifTrajectory {
    for (index, step) in steps.iter_mut().enumerate() {
        step["step_id"] = json!(index + 1);
        step["timestamp"] = json!(format!("2026-01-01T00:00:{index:02}Z"));
    }
    AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7","agent":{"name":"fixture","version":"1"},"steps":steps}).to_string()).unwrap()
}
fn batch(second: &str) -> Value {
    agent(vec![
        call("Read", json!({"path":"shared.rs"})),
        call("Search", json!({"query":second})),
    ])
}

#[test]
fn shared_first_tool_does_not_make_distinct_parallel_batches_removable() {
    let trajectory = trajectory(vec![batch("first"), batch("second")]);
    let stats = cost::compute_cost(&trajectory).unwrap();
    assert!(stats.calls.iter().all(|call| !call.removable_turn));
    assert_eq!(stats.headroom.orch_savable_tok, 0);
}

#[test]
fn adding_or_removing_a_sibling_tool_is_not_an_exact_repeat() {
    let single = agent(vec![call("Read", json!({"path":"shared.rs"}))]);
    for steps in [
        vec![single.clone(), batch("extra")],
        vec![batch("extra"), single],
    ] {
        assert!(!cost::compute_cost(&trajectory(steps)).unwrap().calls[1].removable_turn);
    }
}

#[test]
fn user_requested_repeat_does_not_inflate_orchestration_headroom() {
    let steps = vec![
        batch("same"),
        json!({"source":"user","message":"Please check the same files again after my edit."}),
        batch("same"),
    ];
    let stats = cost::compute_cost(&trajectory(steps)).unwrap();
    assert!(!stats.calls[1].removable_turn);
    assert_eq!(stats.headroom.orch_savable_tok, 0);
}

#[test]
fn changed_arguments_remain_distinct() {
    let steps = vec![
        agent(vec![call(
            "Bash",
            json!({"command":"cargo test","cwd":"first"}),
        )]),
        agent(vec![call(
            "Bash",
            json!({"command":"cargo test","cwd":"second"}),
        )]),
    ];
    assert!(!cost::compute_cost(&trajectory(steps)).unwrap().calls[1].removable_turn);
}

#[test]
fn identical_batch_without_new_user_is_still_a_candidate() {
    let stats = cost::compute_cost(&trajectory(vec![batch("same"), batch("same")])).unwrap();
    assert!(!stats.calls[0].removable_turn);
    assert!(stats.calls[1].removable_turn);
    assert!(stats.headroom.orch_savable_tok > 0);
}

#[test]
fn failed_result_rule_and_text_only_control_are_preserved() {
    let mut failed = agent(vec![call("Read", json!({"path":"missing"}))]);
    failed["observation"] = json!({"results":[{"source_call_id":"Read","content":"unavailable","extra":{"is_error":true}}]});
    let stats = cost::compute_cost(&trajectory(vec![
        failed,
        json!({"source":"agent","message":"done"}),
        json!({"source":"agent","message":"done"}),
    ]))
    .unwrap();
    assert!(stats.calls[0].removable_turn);
    assert!(!stats.calls[1].removable_turn);
    assert!(!stats.calls[2].removable_turn);
}
