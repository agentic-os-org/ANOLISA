//! Cost evidence must survive the command shapes emitted by Codex.
use agentsight_opt::{AtifTrajectory, cost::extract_waste_candidates, types::WasteCandidateSet};
use agentsight_trajectory_collector::codex::convert_codex_events;
use serde_json::{Value, json};

fn candidates(arguments: Value) -> WasteCandidateSet {
    let mut steps = vec![json!({
        "step_id":1,"source":"agent","message":"Discard the unsuccessful changes.",
        "tool_calls":[{"tool_call_id":"rollback","function_name":"shell","arguments":arguments}],
        "observation":{"results":[{"source_call_id":"rollback","content":"done"}]},
        "metrics":{"prompt_tokens":100,"completion_tokens":20}
    })];
    for id in 2..=5 {
        steps.push(
            json!({"step_id":id,"source":"agent","message":"Continue with the alternative.",
            "metrics":{"prompt_tokens":100,"completion_tokens":20}}),
        );
    }
    let trajectory = AtifTrajectory::from_json(
        &json!({
            "schema_version":"ATIF-v1.7","session_id":"cost-argv","steps":steps
        })
        .to_string(),
    )
    .unwrap();
    extract_waste_candidates(&trajectory).unwrap()
}

fn assert_backtrack(set: &WasteCandidateSet, expected: bool) {
    assert_eq!(
        set.ledger.iter().filter(|row| row.backtrack).count(),
        usize::from(expected)
    );
    let detour = set
        .candidates
        .iter()
        .find(|candidate| candidate.id == "detour")
        .unwrap();
    assert!(detour.facts.contains(if expected {
        "1 处回退"
    } else {
        "0 处回退"
    }));
    assert_eq!(detour.steps, if expected { vec![0] } else { vec![] });
    assert_eq!(
        detour.potential_save_tokens,
        if expected { set.ledger[0].tokens } else { 0 }
    );
    assert!(set.ledger[0].tokens > 0);
}

#[test]
fn argv_reversals_match_string_commands_in_cost_evidence() {
    for command in [
        vec!["git", "reset", "--hard", "HEAD~1"],
        vec!["git", "switch", "main"],
        vec!["git", "stash", "pop"],
    ] {
        for key in ["command", "cmd", "script"] {
            let argv = candidates(json!({key:command}));
            let text = candidates(json!({key:command.join(" ")}));
            assert_backtrack(&argv, true);
            assert_backtrack(&text, true);
            assert_eq!(argv.ledger[0].tokens, text.ledger[0].tokens);
        }
    }
}

#[test]
fn inspection_and_branch_creation_are_not_reversals() {
    for command in [
        vec!["git", "stash", "list"],
        vec!["git", "stash", "show", "-p"],
        vec!["git", "switch", "-c", "fix/parser"],
        vec!["git", "checkout", "-b", "fix/parser"],
    ] {
        assert_backtrack(&candidates(json!({"command":command})), false);
        assert_backtrack(&candidates(json!({"command":command.join(" ")})), false);
    }
}

#[test]
fn malformed_commands_and_descriptive_arguments_are_not_evidence() {
    for arguments in [
        json!({"command":["git", "reset", null, "--hard"]}),
        json!({"command":[]}),
        json!({"command":" "}),
        json!({"pattern":"git reset --hard"}),
        json!({"new_string":"git stash pop"}),
    ] {
        assert_backtrack(&candidates(arguments), false);
    }
}

#[test]
fn codex_local_shell_rollback_reaches_the_cost_ledger() {
    let events = vec![
        json!({"type":"response_item","payload":{
            "type":"local_shell_call","call_id":"rollback",
            "action":{"type":"exec","command":["git","reset","--hard","HEAD~1"]}
        }}),
        json!({"type":"response_item","payload":{
            "type":"local_shell_call_output","id":"rollback","output":"HEAD is now at abc123 baseline"
        }}),
    ];
    let collected = convert_codex_events(&events, "cost-argv").unwrap();
    let trajectory =
        AtifTrajectory::from_json(&serde_json::to_string(&collected).unwrap()).unwrap();
    assert_eq!(
        trajectory.steps[0].calls()[0].arguments["command"],
        json!(["git", "reset", "--hard", "HEAD~1"])
    );
    let set = extract_waste_candidates(&trajectory).unwrap();
    assert_eq!(set.ledger.len(), 1);
    assert!(set.ledger[0].backtrack);
    assert!(!set.ledger[0].is_error);
}

#[test]
fn codex_cmd_and_argv_calls_preserve_distinct_and_recurring_action_sigs() {
    let turn = |id: &str, call_item: Value, output_item: Value| -> Vec<Value> {
        vec![
            json!({"type":"event_msg","payload":{"type":"user_message","message":format!("step {id}")}}),
            call_item,
            output_item,
        ]
    };
    let distinct_events: Vec<Value> = [
        turn(
            "1",
            json!({"type":"response_item","payload":{
                "type":"function_call","call_id":"c1","name":"exec_command",
                "arguments":"{\"cmd\":\"cargo check\"}"
            }}),
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"c1","output":"ok"
            }}),
        ),
        turn(
            "2",
            json!({"type":"response_item","payload":{
                "type":"function_call","call_id":"c2","name":"exec_command",
                "arguments":"{\"cmd\":\"cargo test\"}"
            }}),
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"c2","output":"ok"
            }}),
        ),
        turn(
            "3",
            json!({"type":"response_item","payload":{
                "type":"local_shell_call","call_id":"c3",
                "action":{"type":"exec","command":["git","status"]}
            }}),
            json!({"type":"response_item","payload":{
                "type":"local_shell_call_output","id":"c3","output":"clean"
            }}),
        ),
        turn(
            "4",
            json!({"type":"response_item","payload":{
                "type":"local_shell_call","call_id":"c4",
                "action":{"type":"exec","command":["git","diff"]}
            }}),
            json!({"type":"response_item","payload":{
                "type":"local_shell_call_output","id":"c4","output":""
            }}),
        ),
        turn(
            "5",
            json!({"type":"response_item","payload":{
                "type":"function_call","call_id":"c5","name":"Edit",
                "arguments":"{\"filePath\":\"src/lib.rs\"}"
            }}),
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"c5","output":"edited"
            }}),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();

    let collected = convert_codex_events(&distinct_events, "codex-distinct-sigs").unwrap();
    let trajectory =
        AtifTrajectory::from_json(&serde_json::to_string(&collected).unwrap()).unwrap();
    let set = extract_waste_candidates(&trajectory).unwrap();
    let detour = set.candidates.iter().find(|c| c.id == "detour").unwrap();
    assert_eq!(
        (
            set.ledger
                .iter()
                .map(|row| row.action_sig.as_str())
                .collect::<Vec<_>>(),
            detour.facts.contains("重复动作签名无"),
        ),
        (
            vec![
                "exec_command:cargo check",
                "exec_command:cargo test",
                "shell:git status",
                "shell:git diff",
                "Edit:src/lib.rs",
            ],
            true,
        ),
        "detour.facts={}",
        detour.facts
    );

    // Repeating the same normalized command across different files still marks
    // "重复动作签名存在".
    let repeated_events: Vec<Value> = [
        turn(
            "1",
            json!({"type":"response_item","payload":{
                "type":"local_shell_call","call_id":"r1",
                "action":{"type":"exec","command":["rg","parse","src/a.rs"]}
            }}),
            json!({"type":"response_item","payload":{
                "type":"local_shell_call_output","id":"r1","output":"hit a"
            }}),
        ),
        turn(
            "2",
            json!({"type":"response_item","payload":{
                "type":"local_shell_call","call_id":"r2",
                "action":{"type":"exec","command":["rg","parse","src/b.rs"]}
            }}),
            json!({"type":"response_item","payload":{
                "type":"local_shell_call_output","id":"r2","output":"hit b"
            }}),
        ),
        turn(
            "3",
            json!({"type":"response_item","payload":{
                "type":"function_call","call_id":"r3","name":"exec_command",
                "arguments":"{\"cmd\":\"cargo check\"}"
            }}),
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"r3","output":"ok"
            }}),
        ),
        turn(
            "4",
            json!({"type":"response_item","payload":{
                "type":"function_call","call_id":"r4","name":"exec_command",
                "arguments":"{\"cmd\":\"cargo test\"}"
            }}),
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"r4","output":"ok"
            }}),
        ),
        turn(
            "5",
            json!({"type":"response_item","payload":{
                "type":"function_call","call_id":"r5","name":"Edit",
                "arguments":"{\"filePath\":\"src/main.rs\"}"
            }}),
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"r5","output":"ok"
            }}),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    let repeated_collected = convert_codex_events(&repeated_events, "codex-repeated-sigs").unwrap();
    let repeated_traj =
        AtifTrajectory::from_json(&serde_json::to_string(&repeated_collected).unwrap()).unwrap();
    let repeated_set = extract_waste_candidates(&repeated_traj).unwrap();
    assert_eq!(repeated_set.ledger[0].action_sig, "shell:rg parse <path>");
    assert_eq!(repeated_set.ledger[1].action_sig, "shell:rg parse <path>");
    let repeated_detour = repeated_set
        .candidates
        .iter()
        .find(|c| c.id == "detour")
        .unwrap();
    assert!(
        repeated_detour.facts.contains("重复动作签名存在"),
        "recurring normalized argv command must report repeated action signature: {}",
        repeated_detour.facts
    );
}
