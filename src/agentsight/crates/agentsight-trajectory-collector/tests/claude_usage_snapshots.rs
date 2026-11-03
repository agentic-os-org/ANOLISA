use agentsight_trajectory_collector::atif::convert_qoder_events;
use agentsight_trajectory_collector::{
    scan_once, CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore,
};
use serde_json::{json, Value};

fn assistant(id: Option<&str>, input: u64, output: u64, read: u64, created: u64) -> Value {
    let mut message = json!({
        "role": "assistant",
        "model": "fixture-model",
        "usage": {
            "input_tokens": input,
            "output_tokens": output,
            "cache_read_input_tokens": read,
            "cache_creation_input_tokens": created
        },
        "content": [{"type": "text", "text": format!("part {output}")}]
    });
    if let Some(id) = id {
        message["id"] = json!(id);
    }
    json!({"type": "assistant", "timestamp": "2026-10-10T00:00:01Z", "message": message})
}

#[test]
fn same_message_usage_snapshots_are_counted_once() {
    let events = vec![
        assistant(Some("msg-fixture-one"), 100, 1, 50, 20),
        assistant(Some("msg-fixture-one"), 100, 9, 50, 20),
    ];
    let trajectory = convert_qoder_events(&events, "claude-code").unwrap();
    assert_eq!(trajectory.steps.len(), 1);
    assert_eq!(trajectory.steps[0].message, "part 1\npart 9");
    let usage = trajectory.steps[0].metrics.as_ref().unwrap();
    assert_eq!(
        usage.prompt_tokens,
        Some(170),
        "one inference input counted once"
    );
    assert_eq!(
        usage.completion_tokens,
        Some(9),
        "use the final cumulative output"
    );
    assert_eq!(usage.cached_tokens, Some(50));
    let total = trajectory.final_metrics.as_ref().unwrap();
    assert_eq!(total.total_prompt_tokens, Some(170));
    assert_eq!(total.total_completion_tokens, Some(9));
    assert_eq!(total.total_cached_tokens, Some(50));
}

#[test]
fn distinct_message_ids_continue_to_sum_usage() {
    let events = vec![
        assistant(Some("msg-fixture-one"), 100, 1, 0, 0),
        assistant(Some("msg-fixture-two"), 40, 9, 0, 0),
    ];
    let trajectory = convert_qoder_events(&events, "claude-code").unwrap();
    let usage = trajectory.steps[0].metrics.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, Some(140));
    assert_eq!(usage.completion_tokens, Some(10));
}

#[test]
fn unidentified_partial_usage_keeps_existing_accounting() {
    let events = vec![assistant(None, 100, 1, 0, 0), assistant(None, 40, 9, 0, 0)];
    let trajectory = convert_qoder_events(&events, "qoder").unwrap();
    let usage = trajectory.steps[0].metrics.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, Some(140));
    assert_eq!(usage.completion_tokens, Some(10));
}

#[test]
fn other_sources_keep_identified_partial_usage_accounting() {
    let events = vec![
        assistant(Some("message-fixture-one"), 100, 1, 0, 0),
        assistant(Some("message-fixture-one"), 40, 9, 0, 0),
    ];
    let trajectory = convert_qoder_events(&events, "qoder").unwrap();
    let usage = trajectory.steps[0].metrics.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, Some(140));
    assert_eq!(usage.completion_tokens, Some(10));
}

#[test]
fn later_content_without_usage_keeps_the_last_reported_snapshot() {
    for reported_usage in [
        None,
        Some(Value::Null),
        Some(json!({})),
        Some(json!({"cache_read_input_tokens": 999})),
        Some(json!({"service_tier": "fixture"})),
    ] {
        let first = assistant(Some("msg-fixture-one"), 100, 9, 50, 20);
        let mut tail = first.clone();
        match reported_usage {
            Some(usage) => tail["message"]["usage"] = usage,
            None => {
                tail["message"].as_object_mut().unwrap().remove("usage");
            }
        }
        tail["message"]["content"] = json!([{"type": "text", "text": "tail"}]);
        let trajectory = convert_qoder_events(&[first, tail], "claude-code").unwrap();
        let usage = trajectory.steps[0].metrics.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, Some(170));
        assert_eq!(usage.completion_tokens, Some(9));
        assert_eq!(trajectory.steps[0].message, "part 9\ntail");
    }
}

#[test]
fn mixed_identified_and_anonymous_messages_keep_their_usage() {
    let events = vec![
        assistant(Some("msg-fixture-one"), 100, 1, 0, 0),
        assistant(None, 40, 5, 0, 0),
        assistant(Some("msg-fixture-one"), 100, 9, 0, 0),
    ];
    let trajectory = convert_qoder_events(&events, "claude-code").unwrap();
    let usage = trajectory.steps[0].metrics.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, Some(140));
    assert_eq!(usage.completion_tokens, Some(14));
}

#[test]
fn collector_persists_one_message_snapshot_and_stable_file_state() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "anolisa-claude-usage-{}-{nonce}",
        std::process::id()
    ));
    let projects = root.join(".claude/projects");
    let project = projects.join("-data-fixture");
    std::fs::create_dir_all(&project).unwrap();
    let session = "11111111-2222-4333-8444-555555555555";
    let events = [
        json!({"type": "user", "timestamp": "2026-10-10T00:00:00Z", "message": {"role": "user", "content": "fixture question"}}),
        assistant(Some("msg-fixture-one"), 100, 1, 50, 20),
        assistant(Some("msg-fixture-one"), 100, 9, 50, 20),
    ];
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    std::fs::write(project.join(format!("{session}.jsonl")), content).unwrap();
    {
        let store = TrajectoryStore::new_with_path(&root.join("trajectories.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![projects]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };
        scan_once(&store, &config);
        let record = store.get(session).unwrap().unwrap();
        assert_eq!(record.source, "claude-code");
        assert_eq!(record.total_prompt_tokens, Some(170));
        assert_eq!(record.total_completion_tokens, Some(9));
        let original = record.atif_json;
        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.get(session).unwrap().unwrap().atif_json, original);
    }
    std::fs::remove_dir_all(root).unwrap();
}
