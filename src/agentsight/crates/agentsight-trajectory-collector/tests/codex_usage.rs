//! Snapshot accounting through the public converter and persisted collector.

use agentsight_atif::{AtifTrajectory, StepSource};
use agentsight_trajectory_collector::{
    codex::convert_codex_events, scan_once, CollectorConfig, TrajectoryMaintenancePolicy,
    TrajectoryStore,
};
use serde_json::{json, Value};

fn usage(input: u64, output: u64, cached: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output, "cached_input_tokens": cached})
}

fn snapshot(total: Value, last: Value) -> Value {
    json!({"type": "event_msg", "payload": {
        "type": "token_count", "info": {"total_token_usage": total, "last_token_usage": last}
    }})
}

fn user(text: &str) -> Value {
    json!({"type": "event_msg", "payload": {"type": "user_message", "message": text}})
}

fn convert(events: &[Value]) -> AtifTrajectory {
    convert_codex_events(events, "codex").unwrap()
}

fn step_usage(trajectory: &AtifTrajectory) -> Vec<(u64, u64, u64)> {
    trajectory
        .steps
        .iter()
        .filter_map(|step| {
            let metrics = step.metrics.as_ref()?;
            Some((
                metrics.prompt_tokens.unwrap_or(0),
                metrics.completion_tokens.unwrap_or(0),
                metrics.cached_tokens.unwrap_or(0),
            ))
        })
        .collect()
}

#[test]
fn repeated_snapshot_counts_once() {
    let event = snapshot(usage(100, 20, 40), usage(100, 20, 40));
    let trajectory = convert(&[user("first"), event.clone(), event]);
    assert_eq!(step_usage(&trajectory), vec![(100, 20, 40)]);
    let totals = trajectory.final_metrics.unwrap();
    assert_eq!(totals.total_prompt_tokens, Some(100));
    assert_eq!(totals.total_completion_tokens, Some(20));
    assert_eq!(totals.total_cached_tokens, Some(40));
}

#[test]
fn stale_snapshot_after_user_does_not_create_agent_step() {
    let event = snapshot(usage(100, 20, 40), usage(100, 20, 40));
    let trajectory = convert(&[user("first"), event.clone(), user("second"), event]);
    assert_eq!(trajectory.steps.len(), 3);
    assert_eq!(trajectory.steps.last().unwrap().source, StepSource::User);
    assert_eq!(step_usage(&trajectory), vec![(100, 20, 40)]);
}

#[test]
fn cumulative_delta_belongs_to_current_turn() {
    let old = snapshot(usage(100, 20, 40), usage(100, 20, 40));
    let trajectory = convert(&[
        user("first"),
        old.clone(),
        user("second"),
        old,
        snapshot(usage(150, 30, 60), usage(50, 10, 20)),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(100, 20, 40), (50, 10, 20)]);
}

#[test]
fn equal_last_usage_with_advancing_totals_counts_both() {
    let trajectory = convert(&[
        snapshot(usage(100, 20, 40), usage(100, 20, 40)),
        snapshot(usage(200, 40, 80), usage(100, 20, 40)),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(200, 40, 80)]);
}

#[test]
fn cache_only_advance_does_not_recharge_input_or_output() {
    let trajectory = convert(&[
        snapshot(usage(100, 20, 40), usage(100, 20, 40)),
        snapshot(usage(100, 20, 50), usage(100, 20, 50)),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(100, 20, 50)]);
}

#[test]
fn first_snapshot_and_counter_reset_use_last_usage() {
    let trajectory = convert(&[
        snapshot(usage(1000, 200, 400), usage(100, 20, 40)),
        snapshot(usage(50, 10, 20), usage(50, 10, 20)),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(150, 30, 60)]);
    assert_eq!(
        trajectory.final_metrics.unwrap().total_prompt_tokens,
        Some(50)
    );
}

#[test]
fn missing_totals_keep_legacy_fallback_and_zero_is_not_a_step() {
    let trajectory = convert(&[
        snapshot(Value::Null, usage(100, 20, 40)),
        snapshot(Value::Null, usage(100, 20, 40)),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(200, 40, 80)]);
    assert!(convert(&[snapshot(usage(0, 0, 0), usage(0, 0, 0))])
        .steps
        .is_empty());
}

#[test]
fn missing_total_breaks_the_incremental_baseline() {
    let trajectory = convert(&[
        snapshot(usage(100, 20, 40), usage(100, 20, 40)),
        snapshot(Value::Null, usage(50, 10, 20)),
        snapshot(usage(200, 40, 80), usage(50, 10, 20)),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(200, 40, 80)]);
}

#[test]
fn cumulative_advance_without_last_usage_is_recovered() {
    let trajectory = convert(&[
        snapshot(usage(100, 20, 40), usage(100, 20, 40)),
        snapshot(usage(150, 30, 60), Value::Null),
    ]);
    assert_eq!(step_usage(&trajectory), vec![(150, 30, 60)]);
}

#[test]
fn collector_persists_correct_steps_and_authoritative_totals() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("anolisa-usage-{}-{nonce}", std::process::id()));
    let sessions = root.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let old = snapshot(usage(100, 20, 40), usage(100, 20, 40));
    let events = [
        json!({"type":"session_meta", "payload":{"id":"usage-session"}}),
        user("first"),
        old.clone(),
        user("second"),
        old,
        snapshot(usage(150, 30, 60), usage(50, 10, 20)),
    ];
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    std::fs::write(sessions.join("usage-session.jsonl"), content).unwrap();
    {
        let store = TrajectoryStore::new_with_path(&root.join("trajectories.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![sessions]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };
        scan_once(&store, &config);
        let record = store.get("usage-session").unwrap().unwrap();
        let trajectory: AtifTrajectory = serde_json::from_str(&record.atif_json).unwrap();
        assert_eq!(step_usage(&trajectory), vec![(100, 20, 40), (50, 10, 20)]);
        assert_eq!(record.total_prompt_tokens, Some(150));
        assert_eq!(record.total_completion_tokens, Some(30));
        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(
            store.get("usage-session").unwrap().unwrap().atif_json,
            record.atif_json
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
