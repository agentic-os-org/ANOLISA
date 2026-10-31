use agentsight_opt::{
    atif::{AtifTrajectory, render_trimmed},
    cost::{compute_cost, extract_waste_candidates},
};
use agentsight_trajectory_collector::atif::convert_qoder_events;
use serde_json::json;

fn replayed_context(text: &str) -> AtifTrajectory {
    let trajectory = convert_qoder_events(
        &[
            json!({"type":"user", "message":{"content":[
            {"type":"tool_result", "tool_use_id":"prior-call", "content":text},
            {"type":"text", "text":"continue"}]}}),
            json!({"type":"assistant", "message":{"content":[{"type":"text","text":"done"}]}}),
        ],
        "qoder",
    )
    .unwrap();
    AtifTrajectory::from_json(&trajectory.to_json_value().unwrap().to_string()).unwrap()
}

#[test]
fn collector_preserved_context_reaches_cost_breakdown_and_prompt() {
    let trajectory = replayed_context("previous tool output");
    assert_eq!(trajectory.steps[0].results().len(), 1);
    let cost = compute_cost(&trajectory).unwrap();
    let tool_chars = cost
        .breakdown
        .iter()
        .find(|x| x.label == "工具返回")
        .unwrap()
        .chars;
    assert_eq!(tool_chars, "previous tool output".chars().count());
    assert_eq!(
        cost.total_chars,
        "continueprevious tool outputdone".chars().count()
    );
    assert!(cost.calls[0].tool_results > 0);
    let rendered = render_trimmed(&trajectory);
    assert!(rendered.contains("tool_result: previous tool output"));
    assert_eq!(rendered.matches("previous tool output").count(), 1);
}

#[test]
fn large_replayed_output_keeps_bounded_render_and_replay_accounting() {
    let trajectory = replayed_context(&"x".repeat(12000));
    let cost = compute_cost(&trajectory).unwrap();
    assert_eq!(cost.calls[0].tool_results, 3000);
    assert!(cost.calls[0].trimmable > 0);
    let rendered = render_trimmed(&trajectory);
    assert!(rendered.contains("12000 chars total"));
    assert!(rendered.len() < 500);
    let candidates = extract_waste_candidates(&trajectory).unwrap();
    assert!(
        serde_json::to_value(candidates)
            .unwrap()
            .to_string()
            .contains("tool_output")
    );
}

#[test]
fn ordinary_agent_observations_are_counted_only_once() {
    let trajectory = AtifTrajectory::from_json(&json!({"schema_version":"ATIF-v1.7", "steps":[
        {"step_id":1,"source":"agent","message":"call", "observation":{"results":[{"content":"abcd"}]}},
        {"step_id":2,"source":"agent","message":"done"}
    ]}).to_string()).unwrap();
    let cost = compute_cost(&trajectory).unwrap();
    assert_eq!(cost.total_chars, 12);
    assert_eq!(cost.calls[0].tool_results, 0);
    assert_eq!(cost.calls[1].tool_results, 1);
    assert_eq!(
        render_trimmed(&trajectory)
            .matches("tool_result: abcd")
            .count(),
        1
    );
}
