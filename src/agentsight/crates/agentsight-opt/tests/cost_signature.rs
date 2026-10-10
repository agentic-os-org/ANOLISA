use agentsight_opt::atif::AtifTrajectory;
use agentsight_opt::cost::compute_cost;

fn trajectory_with_distinct_long_paths() -> AtifTrajectory {
    let common =
        "/workspace/project/fixtures/this-shared-prefix-is-long-enough-to-fill-the-summary-window/";
    let steps = (0..3)
        .map(|i| {
            serde_json::json!({
                "step_id": i + 1,
                "source": "agent",
                "timestamp": format!("2026-10-10T00:00:0{i}Z"),
                "message": "",
                "tool_calls": [{
                    "tool_call_id": format!("call-{i}"),
                    "function_name": "Read",
                    "arguments": {"file_path": format!("{common}unique-file-{i}-with-a-distinct-suffix.txt")}
                }],
                "observation": {"results": [{
                    "source_call_id": format!("call-{i}"),
                    "content": format!("file {i} body: {}", "plain fixture text ".repeat(20))
                }]}
            })
        })
        .collect::<Vec<_>>();
    AtifTrajectory::from_json(
        &serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "session_id": "fixture",
            "steps": steps
        })
        .to_string(),
    )
    .unwrap()
}

#[test]
fn distinct_file_arguments_are_not_reported_as_redundant_calls() {
    let report = compute_cost(&trajectory_with_distinct_long_paths()).unwrap();
    assert!(
        report.redundant_calls.is_empty(),
        "different file paths must not collapse into one truncated signature: {:?}",
        report.redundant_calls
    );
}

#[test]
fn distinct_primary_actions_do_not_mark_whole_turns_removable() {
    let report = compute_cost(&trajectory_with_distinct_long_paths()).unwrap();
    assert!(
        report.calls.iter().all(|call| !call.removable_turn),
        "reading a different file is a different primary action: {:?}",
        report
            .calls
            .iter()
            .map(|call| call.removable_turn)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.headroom.orch_savable_tok, 0);
}

#[test]
fn genuine_repeated_long_arguments_keep_a_bounded_display_summary() {
    let mut trajectory = trajectory_with_distinct_long_paths();
    let same_arguments = trajectory.steps[0].tool_calls.as_ref().unwrap()[0]
        .arguments
        .clone();
    for step in &mut trajectory.steps {
        step.tool_calls.as_mut().unwrap()[0].arguments = same_arguments.clone();
    }
    let report = compute_cost(&trajectory).unwrap();
    assert_eq!(report.redundant_calls.len(), 1);
    let group = &report.redundant_calls[0];
    assert_eq!(group.name, "Read");
    assert_eq!(group.count, 3);
    assert!(group.wasted_chars > 0);
    assert!(group.cmd_sig.chars().count() <= 81);
    assert!(group.cmd_sig.ends_with('…'));
    assert!(report.calls.iter().skip(1).all(|call| call.removable_turn));
    assert!(report.headroom.orch_savable_tok > 0);
}

#[test]
fn genuine_repeated_short_arguments_keep_the_full_display_summary() {
    let mut trajectory = trajectory_with_distinct_long_paths();
    for step in &mut trajectory.steps {
        step.tool_calls.as_mut().unwrap()[0].arguments =
            serde_json::json!({"file_path":"README.md"});
    }
    let report = compute_cost(&trajectory).unwrap();
    assert_eq!(report.redundant_calls.len(), 1);
    assert_eq!(report.redundant_calls[0].count, 3);
    assert_eq!(
        report.redundant_calls[0].cmd_sig,
        r#"{"file_path":"README.md"}"#
    );
}
