use super::*;
use actix_web::{App, test as awtest, web};
use agentsight_trajectory_collector::{TrajectoryRecord, TrajectoryStore};
use serde_json::{Value, json};

fn save(store: &TrajectoryStore, session: &str, second_round: bool) {
    let mut doc = partial_move_trajectory();
    doc.session_id = Some(session.into());
    if second_round {
        let mut user = doc.steps[0].clone();
        user.step_id = 3;
        user.message = "现在只检查乙文件".into();
        let mut agent = doc.steps[1].clone();
        agent.step_id = 4;
        agent.message = "已检查乙文件".into();
        doc.steps.extend([user, agent]);
    }
    store
        .upsert_trajectory(&TrajectoryRecord {
            session_id: session.into(),
            schema_version: doc.schema_version.clone(),
            agent_name: "fixture".into(),
            model_name: None,
            num_steps: doc.steps.len() as i64,
            total_prompt_tokens: None,
            total_completion_tokens: None,
            start_time: None,
            end_time: None,
            first_user_message: None,
            last_user_message: None,
            atif_json: serde_json::to_string(&doc).unwrap(),
            project: "fixture".into(),
            source: "fixture".into(),
            is_subagent: false,
            file_path: String::new(),
            file_size: 0,
            file_mtime_ns: 0,
        })
        .unwrap();
}

fn state(dir: &std::path::Path, store: Arc<TrajectoryStore>) -> web::Data<AppState> {
    let endpoint = responding_llm_sequence(vec![
        combined_response(),
        attribution_response("已把甲和乙移到归档目录"),
        combined_response(),
        attribution_response("已检查乙文件"),
    ]);
    std::fs::write(
        dir.join("optimization_config.json"),
        json!({
            "base_url":endpoint, "api_key":"fixture-key", "model":"test-model"
        })
        .to_string(),
    )
    .unwrap();
    let mut state = test_app_state().into_inner();
    let state_mut = Arc::get_mut(&mut state).unwrap();
    state_mut.storage_path = dir.to_path_buf();
    state_mut.optimize = Some(crate::server::optimize::OptimizeState::init(dir, None));
    state_mut.causal_store = Some(Arc::new(
        causal_store::CausalCaseStore::open_private(dir).unwrap(),
    ));
    *state_mut.trajectory_store.write().unwrap() = Some(store);
    web::Data::from(state)
}

async fn changed_latest_round(durable: bool) {
    let dir = tempfile::tempdir().unwrap();
    let session = format!("freshness-{}", uuid::Uuid::new_v4());
    let store =
        Arc::new(TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap());
    save(&store, &session, false);
    let state = state(dir.path(), Arc::clone(&store));
    let app =
        awtest::init_service(App::new().app_data(state).service(run_causal_attribution)).await;
    let request = || {
        awtest::TestRequest::post()
            .uri("/causal-attribution")
            .set_json(json!({"session_id":session, "id_kind":"session", "complaint":"请核对结果"}))
            .to_request()
    };
    let initial: Value = awtest::call_and_read_body_json(&app, request()).await;
    assert_eq!(initial["cached"], false, "{initial}");
    if durable {
        causal_cache()
            .lock()
            .unwrap()
            .remove(&(format!("sess:{session}"), None));
    }
    let cached: Value = awtest::call_and_read_body_json(&app, request()).await;
    assert_eq!(
        cached["cached"], true,
        "unchanged snapshots reuse a paid analysis"
    );
    save(&store, &session, true);
    if durable {
        causal_cache()
            .lock()
            .unwrap()
            .remove(&(format!("sess:{session}"), None));
    }
    let refreshed: Value = awtest::call_and_read_body_json(&app, request()).await;
    assert_eq!(
        refreshed["cached"], false,
        "changed source must not return the old last round: {refreshed}"
    );
    assert_eq!(refreshed["case"]["task"], "现在只检查乙文件");
}

#[actix_web::test]
async fn memory_cache_tracks_the_current_latest_round() {
    changed_latest_round(false).await;
}

#[actix_web::test]
async fn durable_cache_tracks_the_current_latest_round() {
    changed_latest_round(true).await;
}

#[actix_web::test]
async fn retained_cases_survive_missing_sources_in_memory_and_on_disk() {
    for durable in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let session = format!("retained-{}", uuid::Uuid::new_v4());
        let store =
            Arc::new(TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap());
        save(&store, &session, false);
        let state = state(dir.path(), store);
        let app = awtest::init_service(
            App::new()
                .app_data(state.clone())
                .service(run_causal_attribution),
        )
        .await;
        let request = || {
            awtest::TestRequest::post()
                .uri("/causal-attribution")
                .set_json(json!({"session_id":session,"complaint":"请核对结果"}))
                .to_request()
        };
        let initial: Value = awtest::call_and_read_body_json(&app, request()).await;
        assert_eq!(initial["cached"], false, "{initial}");
        *state.trajectory_store.write().unwrap() = None;
        if durable {
            causal_cache()
                .lock()
                .unwrap()
                .remove(&(format!("sess:{session}"), None));
        }
        let retained: Value = awtest::call_and_read_body_json(&app, request()).await;
        assert_eq!(retained["cached"], true, "{retained}");
        assert_eq!(retained["case"], initial["case"]);
        assert!(retained["case"].get("_agentsight_causal_source").is_none());
    }
}

#[actix_web::test]
async fn legacy_rows_are_refreshed_only_when_a_source_is_available() {
    for available in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let session = format!("legacy-{}", uuid::Uuid::new_v4());
        let store =
            Arc::new(TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap());
        save(&store, &session, false);
        let state = state(dir.path(), store);
        let app = awtest::init_service(
            App::new()
                .app_data(state.clone())
                .service(run_causal_attribution),
        )
        .await;
        let request = || {
            awtest::TestRequest::post()
                .uri("/causal-attribution")
                .set_json(json!({"session_id":session,"complaint":"请核对结果"}))
                .to_request()
        };
        let initial: Value = awtest::call_and_read_body_json(&app, request()).await;
        assert_eq!(initial["cached"], false, "{initial}");
        let key = format!("sess:{session}");
        state
            .causal_store
            .as_ref()
            .unwrap()
            .put(
                causal_store::CaseKey {
                    session_key: &key,
                    round: None,
                    complaint: "请核对结果",
                },
                &initial["case"].to_string(),
            )
            .unwrap();
        causal_cache().lock().unwrap().remove(&(key, None));
        if !available {
            *state.trajectory_store.write().unwrap() = None;
        }
        let restored: Value = awtest::call_and_read_body_json(&app, request()).await;
        assert_eq!(restored["cached"], !available, "{restored}");
        if !available {
            assert_eq!(restored["case"], initial["case"]);
        }
    }
}

#[test]
fn source_identity_covers_preceding_evidence_but_ignores_map_order() {
    let mut doc = partial_move_trajectory();
    let original = cache_source::fingerprint(&doc, 1..2).unwrap();
    doc.steps[0].message.push_str("changed preceding context");
    assert_ne!(original, cache_source::fingerprint(&doc, 1..2).unwrap());
    doc.steps[0].extra = Some(std::collections::HashMap::from([
        ("a".into(), json!({"z":1,"b":2})),
        ("b".into(), json!(2)),
    ]));
    let ordered = cache_source::fingerprint(&doc, 1..2).unwrap();
    doc.steps[0].extra = Some(std::collections::HashMap::from([
        ("b".into(), json!(2)),
        ("a".into(), json!({"b":2,"z":1})),
    ]));
    assert_eq!(ordered, cache_source::fingerprint(&doc, 1..2).unwrap());
}
