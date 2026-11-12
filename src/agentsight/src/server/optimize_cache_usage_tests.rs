use super::upsert_opt_run_root;
use agentsight_opt::llm::{ChatMessage, RecordParams, TrajectoryRecorder};
use agentsight_trajectory_collector::TrajectoryStore;

fn dimension(name: &str, cached: u32) -> agentsight_atif::AtifTrajectory {
    let recorder = TrajectoryRecorder::new("fixture-model", "fixture-analysis");
    recorder.record(RecordParams {
        messages: &[ChatMessage::user("Review the fixture")],
        response: "done",
        reasoning: None,
        model: "fixture-model",
        input_tokens: 100,
        output_tokens: 10,
        cached_tokens: cached,
        reasoning_tokens: 0,
        start_ts: "2026-11-12T00:00:00Z",
        end_ts: "2026-11-12T00:00:05Z",
        label: None,
    });
    let mut doc = recorder.to_atif();
    doc.session_id = Some(format!("opt:fixture:subagent:{name}"));
    doc.trajectory_id = Some(name.into());
    doc
}

fn root(store: &TrajectoryStore) -> agentsight_atif::AtifTrajectory {
    serde_json::from_str(&store.get_atif_json("opt:fixture").unwrap().unwrap()).unwrap()
}

#[test]
fn recorded_cache_usage_survives_root_persistence_and_cost_analysis() {
    let dir = tempfile::tempdir().unwrap();
    let store = TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap();
    upsert_opt_run_root(&store, "fixture", "accuracy", &dimension("accuracy", 60)).unwrap();
    upsert_opt_run_root(&store, "fixture", "summary", &dimension("summary", 30)).unwrap();
    let doc = root(&store);
    assert_eq!(
        doc.final_metrics.as_ref().unwrap().total_cached_tokens,
        Some(90)
    );
    assert_eq!(
        doc.steps[0].metrics.as_ref().unwrap().cached_tokens,
        Some(60)
    );
    assert_eq!(
        doc.steps[1].metrics.as_ref().unwrap().cached_tokens,
        Some(30)
    );
    let loaded =
        agentsight_opt::AtifTrajectory::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
    let stats = agentsight_opt::cost::compute_cost(&loaded).unwrap();
    assert_eq!(stats.total_real_input_tok, 200);
    assert_eq!(stats.total_real_output_tok, 20);
    assert_eq!(stats.total_real_cached_tok, 90);
}

#[test]
fn rerun_replaces_cached_usage_including_an_explicit_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap();
    for cached in [60, 20, 0] {
        upsert_opt_run_root(
            &store,
            "fixture",
            "accuracy",
            &dimension("accuracy", cached),
        )
        .unwrap();
        let doc = root(&store);
        assert_eq!(doc.steps.len(), 1);
        assert_eq!(
            doc.final_metrics.unwrap().total_cached_tokens,
            Some(u64::from(cached))
        );
    }
}

#[test]
fn legacy_unknown_cache_usage_is_not_reported_as_measured_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap();
    let mut doc = dimension("accuracy", 0);
    doc.final_metrics.as_mut().unwrap().total_cached_tokens = None;
    upsert_opt_run_root(&store, "fixture", "accuracy", &doc).unwrap();
    assert_eq!(
        root(&store).final_metrics.unwrap().total_cached_tokens,
        None
    );
    upsert_opt_run_root(&store, "fixture", "summary", &dimension("summary", 30)).unwrap();
    assert_eq!(
        root(&store).final_metrics.unwrap().total_cached_tokens,
        Some(30)
    );
}
