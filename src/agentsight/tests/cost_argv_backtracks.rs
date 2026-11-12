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
