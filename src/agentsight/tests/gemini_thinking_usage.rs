use agentsight_opt::{atif::AtifTrajectory, cost::compute_cost};
use agentsight_trajectory_collector::gemini::convert_gemini_events;
use serde_json::{Value, json};

fn converted(tokens: Value) -> agentsight_atif::AtifTrajectory {
    convert_gemini_events(
        &[
            json!({"sessionId":"fixture", "projectHash":"p"}),
            json!({"id":"answer", "type":"gemini", "content":"answer", "tokens":tokens}),
        ],
        "gemini-cli",
    )
    .unwrap()
}

#[test]
fn thinking_tokens_reach_step_totals_and_cost_analysis() {
    let trajectory =
        converted(json!({"input":100, "output":8, "thoughts":32, "cached":20, "total":140}));
    let metrics = trajectory.steps[0].metrics.as_ref().unwrap();
    assert_eq!(metrics.completion_tokens, Some(40));
    assert_eq!(metrics.prompt_tokens, Some(100));
    assert_eq!(metrics.cached_tokens, Some(20));
    assert_eq!(
        trajectory
            .final_metrics
            .as_ref()
            .unwrap()
            .total_completion_tokens,
        Some(40)
    );
    let analysis =
        AtifTrajectory::from_json(&trajectory.to_json_value().unwrap().to_string()).unwrap();
    let cost = compute_cost(&analysis).unwrap();
    assert_eq!(cost.total_real_output_tok, 40);
    assert_eq!(cost.calls[0].output_tokens, 40);
}

#[test]
fn zero_visible_output_still_counts_reasoning() {
    let trajectory = converted(json!({"input":50, "output":0, "thoughts":25}));
    assert_eq!(
        trajectory.steps[0]
            .metrics
            .as_ref()
            .unwrap()
            .completion_tokens,
        Some(25)
    );
}

#[test]
fn absent_or_invalid_thinking_does_not_change_visible_usage() {
    for thoughts in [Value::Null, json!(-1), json!(true), json!("7"), json!(0)] {
        let trajectory = converted(json!({"input":100, "output":8, "thoughts":thoughts}));
        assert_eq!(
            trajectory.steps[0]
                .metrics
                .as_ref()
                .unwrap()
                .completion_tokens,
            Some(8)
        );
    }
    assert_eq!(
        converted(json!({"output":8})).steps[0]
            .metrics
            .as_ref()
            .unwrap()
            .completion_tokens,
        Some(8)
    );
}

#[test]
fn replayed_measurement_replaces_instead_of_double_counting() {
    let events = [
        json!({"id":"answer", "type":"gemini", "content":"answer", "tokens":{"input":20,"output":2,"thoughts":5}}),
        json!({"id":"answer", "type":"gemini", "content":"answer", "tokens":{"input":20,"output":3,"thoughts":9}}),
    ];
    let trajectory = convert_gemini_events(&events, "gemini-cli").unwrap();
    assert_eq!(trajectory.steps.len(), 1);
    assert_eq!(
        trajectory.final_metrics.unwrap().total_completion_tokens,
        Some(12)
    );
}
