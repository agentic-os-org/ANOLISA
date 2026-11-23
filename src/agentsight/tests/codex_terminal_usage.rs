#![cfg(feature = "server")]

use agentsight_opt::{AtifTrajectory, cost::compute_cost};
use agentsight_trajectory_collector::codex::convert_codex_events;
use serde_json::{Value, json};

fn usage(input: u64, output: u64, cached: u64) -> Value {
    json!({"input_tokens":input,"output_tokens":output,"cached_input_tokens":cached})
}

fn count(total: Value, last: Value) -> Value {
    json!({"type":"event_msg","payload":{"type":"token_count",
        "info":{"total_token_usage":total,"last_token_usage":last}}})
}

fn totals(events: &[Value]) -> (u64, u64, u64) {
    let trajectory = convert_codex_events(events, "codex").unwrap();
    trajectory.validate_step_ids().unwrap();
    let final_metrics = trajectory.final_metrics.as_ref().unwrap();
    let totals = (
        final_metrics.total_prompt_tokens.unwrap(),
        final_metrics.total_completion_tokens.unwrap(),
        final_metrics.total_cached_tokens.unwrap(),
    );
    let parsed = AtifTrajectory::from_json(&serde_json::to_string(&trajectory).unwrap()).unwrap();
    let cost = compute_cost(&parsed).unwrap();
    assert_eq!(
        totals,
        (
            cost.total_real_input_tok,
            cost.total_real_output_tok,
            cost.total_real_cached_tok
        )
    );
    totals
}

#[test]
fn last_call_only_tail_advances_the_authoritative_session_totals() {
    assert_eq!(
        totals(&[
            count(usage(100, 40, 30), usage(10, 4, 3)),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"continue"}}),
            count(Value::Null, usage(20, 8, 5)),
            count(Value::Null, usage(30, 9, 7)),
        ]),
        (150, 57, 42)
    );
}

#[test]
fn a_later_total_replaces_the_fallback_extension_without_double_counting() {
    assert_eq!(
        totals(&[
            count(usage(100, 40, 30), usage(10, 4, 3)),
            count(Value::Null, usage(20, 8, 5)),
            count(usage(150, 57, 42), usage(30, 9, 7)),
            count(usage(150, 57, 42), usage(30, 9, 7)),
        ]),
        (150, 57, 42)
    );
}

#[test]
fn invalid_or_zero_last_usage_does_not_erase_known_totals() {
    assert_eq!(
        totals(&[
            count(usage(100, 40, 30), usage(10, 4, 3)),
            count(Value::Null, json!({"input_tokens":"bad"})),
            count(Value::Null, usage(0, 0, 0)),
        ]),
        (100, 40, 30)
    );
}

#[test]
fn a_counter_reset_replaces_history_before_extending_its_tail() {
    assert_eq!(
        totals(&[
            count(usage(100, 40, 30), usage(10, 4, 3)),
            count(Value::Null, usage(20, 8, 5)),
            count(usage(12, 6, 3), usage(12, 6, 3)),
            count(Value::Null, usage(2, 1, 1)),
        ]),
        (14, 7, 4)
    );
}

#[test]
fn last_only_history_still_sums_steps_without_an_authoritative_total() {
    assert_eq!(
        totals(&[
            count(Value::Null, usage(20, 8, 5)),
            count(Value::Null, usage(30, 9, 7)),
        ]),
        (50, 17, 12)
    );
}
