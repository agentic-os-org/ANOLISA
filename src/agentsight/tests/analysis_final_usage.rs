use agentsight_opt::{atif::AtifTrajectory, cost::compute_cost};
use serde_json::{Value, json};

fn stats(totals: Value, steps: Value) -> agentsight_opt::types::CostStats {
    let trajectory = AtifTrajectory::from_json(
        &json!({"schema_version":"ATIF-v1.7",
        "agent":{"name":"hermes","version":"unknown"}, "steps":steps,
        "final_metrics":totals})
        .to_string(),
    )
    .unwrap();
    compute_cost(&trajectory).unwrap()
}

#[test]
fn hermes_session_totals_survive_without_per_step_measurements() {
    let cost = stats(
        json!({"total_prompt_tokens":120,"total_completion_tokens":30,"total_cached_tokens":20}),
        json!([{"step_id":1,"source":"user","message":"inspect"},
        {"step_id":2,"source":"agent","message":"done"}]),
    );
    assert_eq!(
        (
            cost.total_real_input_tok,
            cost.total_real_output_tok,
            cost.total_real_cached_tok
        ),
        (120, 30, 20)
    );
    assert_eq!(cost.usage_steps, 0, "do not fabricate per-step usage");
    assert!(cost.calls[0].real_prompt_tokens.is_none());
}

#[test]
fn final_counters_are_not_added_to_the_same_step_counters() {
    let steps = json!([{"step_id":1,"source":"agent","message":"done", "metrics":{"prompt_tokens":20,"completion_tokens":4,"cached_tokens":5}}]);
    let complete = stats(
        json!({"total_prompt_tokens":20,"total_completion_tokens":4,"total_cached_tokens":5}),
        steps.clone(),
    );
    assert_eq!(
        (
            complete.total_real_input_tok,
            complete.total_real_output_tok,
            complete.total_real_cached_tok
        ),
        (20, 4, 5)
    );
    let partial = stats(json!({"total_prompt_tokens":120}), steps.clone());
    assert_eq!(
        (
            partial.total_real_input_tok,
            partial.total_real_output_tok,
            partial.total_real_cached_tok
        ),
        (120, 4, 5)
    );
    assert_eq!(partial.usage_steps, 1);
    let absent = stats(Value::Null, steps.clone());
    assert_eq!(
        (
            absent.total_real_input_tok,
            absent.total_real_output_tok,
            absent.total_real_cached_tok
        ),
        (20, 4, 5)
    );
    let known_zero = stats(json!({"total_cached_tokens":0}), steps);
    assert_eq!(known_zero.total_real_cached_tok, 0);
}

#[test]
fn empty_replayed_history_keeps_known_totals() {
    let cost = stats(
        json!({"total_prompt_tokens":12,"total_completion_tokens":0}),
        json!([]),
    );
    assert_eq!(cost.total_real_input_tok, 12);
    assert_eq!(cost.total_real_output_tok, 0);
    assert!(cost.calls.is_empty());
    let unknown = stats(Value::Null, json!([]));
    assert_eq!(unknown.total_real_input_tok, 0);
}

#[test]
fn collected_hermes_database_retains_billing_totals_in_analysis() {
    use agentsight_trajectory_collector::{
        CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
    };
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("hermes");
    std::fs::create_dir(&home).unwrap();
    let source = rusqlite::Connection::open(home.join("state.db")).unwrap();
    source
        .execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, input_tokens INTEGER,
         output_tokens INTEGER, cache_read_tokens INTEGER);
         CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT,
         role TEXT, content TEXT, timestamp REAL);
         INSERT INTO sessions VALUES ('fixture', 120, 30, 20);
         INSERT INTO messages VALUES (1, 'fixture', 'user', 'inspect', 1781517600.0);
         INSERT INTO messages VALUES (2, 'fixture', 'assistant', 'done', 1781517601.0);",
        )
        .unwrap();
    let store = TrajectoryStore::new_with_path(&root.path().join("collected.db")).unwrap();
    scan_once(
        &store,
        &CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![home]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        },
    );
    let documents = store.list_recent_atif_jsons(0, 10).unwrap();
    assert_eq!(documents.len(), 1);
    let trajectory = AtifTrajectory::from_json(&documents[0].1).unwrap();
    let cost = compute_cost(&trajectory).unwrap();
    assert_eq!(
        (
            cost.total_real_input_tok,
            cost.total_real_output_tok,
            cost.total_real_cached_tok
        ),
        (120, 30, 20)
    );
    assert_eq!(cost.usage_steps, 0);
    assert!(
        cost.calls
            .iter()
            .all(|call| call.real_prompt_tokens.is_none())
    );
}
