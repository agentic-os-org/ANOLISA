//! Session exports retain captured metrics and collected subagent documents.

use super::tests::{
    cleanup_db, test_app_state_with_activity_stores, trajectory_record, unique_handler_db,
    write_completed_event_for_session,
};
use super::*;
use actix_web::{App, test};
use agentsight_trajectory_collector::TrajectoryStore;

#[actix_web::test]
async fn session_export_retains_collected_children_without_replacing_capture() {
    let uuid = "00000000-0000-0000-0000-000000000001";
    let rollout = format!("rollout-2026-10-15T00-00-00-{uuid}");
    for (index, (captured, parent, source, collected_parent)) in [
        ("session-dual", "session-dual", "qoder", true),
        (uuid, rollout.as_str(), "codex", true),
        (uuid, rollout.as_str(), "codex", false),
    ]
    .into_iter()
    .enumerate()
    {
        let genai_path = unique_handler_db(&format!("dual-source-export-{index}"));
        let trajectory_path = unique_handler_db(&format!("dual-source-children-{index}"));
        write_completed_event_for_session(&genai_path, "dual", captured);
        let store = Arc::new(TrajectoryStore::new_with_path(&trajectory_path).unwrap());
        if collected_parent {
            store
                .upsert_trajectory(&trajectory_record(parent, "fixture", source))
                .unwrap();
        }
        let mut child = trajectory_record(&format!("{parent}:subagent:child"), "fixture", source);
        child.is_subagent = true;
        child.atif_json = serde_json::json!({
            "schema_version": agentsight_atif::ATIF_SCHEMA_VERSION,
            "session_id": child.session_id,
            "agent": {"name": "child", "version": "fixture"},
            "steps": [],
        })
        .to_string();
        store.upsert_trajectory(&child).unwrap();
        let app = test::init_service(
            App::new()
                .app_data(test_app_state_with_activity_stores(
                    genai_path.clone(),
                    Some(store),
                ))
                .service(export_atif_session)
                .service(get_trajectory_detail),
        )
        .await;
        if collected_parent {
            let collected: Value = test::call_and_read_body_json(
                &app,
                test::TestRequest::get()
                    .uri(&format!("/trajectories/{parent}"))
                    .to_request(),
            )
            .await;
            assert_eq!(
                collected["subagent_trajectories"][0]["session_id"],
                child.session_id
            );
        }
        let exported: Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri(&format!("/export/atif/session/{captured}"))
                .to_request(),
        )
        .await;
        assert_eq!(exported["session_id"], captured);
        assert_eq!(exported["final_metrics"]["total_prompt_tokens"], 10);
        assert_eq!(exported["final_metrics"]["total_completion_tokens"], 5);
        assert!(!exported["steps"].as_array().unwrap().is_empty());
        assert_eq!(
            exported["subagent_trajectories"][0]["session_id"], child.session_id,
            "the list's folded children must remain reachable from its exported parent"
        );
        agentsight_atif::validate_trajectory_str(&exported.to_string()).unwrap();
        cleanup_db(&genai_path);
        cleanup_db(&trajectory_path);
    }
}

#[actix_web::test]
async fn session_export_keeps_capture_when_optional_children_are_unavailable() {
    for invalid_child in [false, true] {
        let genai_path = unique_handler_db("capture-optional-children");
        let trajectory_path = unique_handler_db("capture-invalid-children");
        write_completed_event_for_session(&genai_path, "optional", "session-optional");
        let store = if invalid_child {
            let store = Arc::new(TrajectoryStore::new_with_path(&trajectory_path).unwrap());
            let mut record =
                trajectory_record("session-optional:subagent:broken", "fixture", "qoder");
            record.is_subagent = true;
            record.atif_json = "not JSON".into();
            store.upsert_trajectory(&record).unwrap();
            Some(store)
        } else {
            None
        };
        let app = test::init_service(
            App::new()
                .app_data(test_app_state_with_activity_stores(
                    genai_path.clone(),
                    store,
                ))
                .service(export_atif_session),
        )
        .await;
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/export/atif/session/session-optional")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let exported: Value = test::read_body_json(response).await;
        assert_eq!(exported["final_metrics"]["total_prompt_tokens"], 10);
        assert!(exported["subagent_trajectories"].is_null());
        agentsight_atif::validate_trajectory_str(&exported.to_string()).unwrap();
        cleanup_db(&genai_path);
        cleanup_db(&trajectory_path);
    }
}
