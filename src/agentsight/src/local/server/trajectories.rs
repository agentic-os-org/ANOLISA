//! Trajectory collection endpoints for macOS local server.
//!
//! Mirrors the upstream `/api/trajectories` routes but backed by
//! `agentsight_trajectory_collector::TrajectoryStore` instead of the full
//! Linux-only `AppState`. On macOS the collector scans Qoder/QoderWork session
//! directories and stores ATIF v1.7 documents in `trajectories.db`.

use actix_web::{HttpResponse, Responder, get, web};
use agentsight_atif::StepCategory;
use agentsight_trajectory_collector::StepScanFilter;
use serde::Deserialize;

use super::LocalState;

/// Default and hard-cap for the trajectory list `limit` parameter.
const TRAJECTORY_DEFAULT_LIMIT: i64 = 200;
const TRAJECTORY_MAX_LIMIT: i64 = 1000;

#[derive(Deserialize)]
pub struct TrajectoryQuery {
    pub project: Option<String>,
    pub source: Option<String>,
    pub agent_name: Option<String>,
    pub limit: Option<i64>,
    /// Keep only trajectories whose effective reuse label is one of these
    /// (comma-separated: `good,bad`). Never-triaged sessions match nothing.
    pub label: Option<String>,
    /// Drop trajectories whose label is one of these (`useless` by contract).
    pub exclude_label: Option<String>,
    /// Keep only trajectories a person settled (`confirm`/`override`).
    pub human_backed: Option<bool>,
}

/// GET /api/trajectories
#[get("/api/trajectories")]
pub async fn list_trajectories(
    state: web::Data<LocalState>,
    query: web::Query<TrajectoryQuery>,
) -> impl Responder {
    let Some(tstore) = state.trajectory_store() else {
        return HttpResponse::Ok().json(Vec::<serde_json::Value>::new());
    };
    let limit = match query.limit {
        Some(v) if v > 0 => v.min(TRAJECTORY_MAX_LIMIT),
        _ => TRAJECTORY_DEFAULT_LIMIT,
    };
    match tstore.list_summaries(
        query.project.as_deref(),
        query.source.as_deref(),
        query.agent_name.as_deref(),
        limit,
    ) {
        Ok(mut rows) => {
            filter_rows_by_reuse_labels(state.as_ref(), &query, &mut rows);
            HttpResponse::Ok().json(rows)
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Applies the reuse-label query parameters; mirrors the Linux endpoint's
/// filter so a skill gets the same answer on either platform.
fn filter_rows_by_reuse_labels(
    state: &LocalState,
    query: &TrajectoryQuery,
    rows: &mut Vec<agentsight_trajectory_collector::TrajectorySummary>,
) {
    let labels_needed =
        query.label.is_some() || query.exclude_label.is_some() || query.human_backed == Some(true);
    if !labels_needed {
        return;
    }
    let Some(labels) = state.reuse_store.as_deref() else {
        if query.label.is_some() || query.human_backed == Some(true) {
            rows.clear();
        }
        return;
    };
    let parse = |raw: &str| -> Vec<crate::reuse::TrajectoryLabel> {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(crate::reuse::TrajectoryLabel::parse)
            .collect()
    };
    let keep: Option<std::collections::HashSet<String>> = query.label.as_deref().map(|raw| {
        labels
            .sessions_with_labels(&parse(raw))
            .unwrap_or_default()
            .into_iter()
            .collect()
    });
    let drop: Option<std::collections::HashSet<String>> =
        query.exclude_label.as_deref().map(|raw| {
            labels
                .sessions_with_labels(&parse(raw))
                .unwrap_or_default()
                .into_iter()
                .collect()
        });
    let backed: Option<std::collections::HashSet<String>> = if query.human_backed == Some(true) {
        Some(
            labels
                .list_labels(&crate::reuse::LabelFilter::default())
                .unwrap_or_default()
                .into_iter()
                .filter(|l| l.is_human_backed())
                .map(|l| l.session_id)
                .collect(),
        )
    } else {
        None
    };
    rows.retain(|row| {
        let id = row.session_id.as_str();
        keep.as_ref().is_none_or(|set| set.contains(id))
            && drop.as_ref().is_none_or(|set| !set.contains(id))
            && backed.as_ref().is_none_or(|set| set.contains(id))
    });
}

/// GET /api/trajectories/filters
#[get("/api/trajectories/filters")]
pub async fn trajectory_filters(state: web::Data<LocalState>) -> impl Responder {
    let Some(tstore) = state.trajectory_store() else {
        return HttpResponse::Ok().json(serde_json::json!({
            "projects": [], "sources": [], "agent_names": []
        }));
    };
    match tstore.list_filters() {
        Ok(filters) => HttpResponse::Ok().json(filters),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/trajectories/{session_id}
#[get("/api/trajectories/{session_id}")]
pub async fn get_trajectory_detail(
    state: web::Data<LocalState>,
    path: web::Path<String>,
) -> impl Responder {
    let Some(tstore) = state.trajectory_store() else {
        return HttpResponse::NotFound().json(
            serde_json::json!({"error": "not_found", "message": "Trajectory store not available"}),
        );
    };
    let session_id = path.into_inner();

    match tstore.get_atif_json(&session_id) {
        Ok(Some(json_str)) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&json_str).unwrap_or(serde_json::json!({"raw": json_str}));
            let mut doc = parsed;

            // Embed subagent trajectories if any
            if let Ok(subagents) = tstore.get_subagent_atif_jsons(&session_id)
                && !subagents.is_empty()
            {
                let mut sub_docs = Vec::new();
                for sa_json in subagents {
                    if let Ok(sa) = serde_json::from_str::<serde_json::Value>(&sa_json) {
                        sub_docs.push(sa);
                    }
                }
                if !sub_docs.is_empty() {
                    doc["subagent_trajectories"] = serde_json::Value::Array(sub_docs);
                }
            }

            HttpResponse::Ok().json(doc)
        }
        Ok(None) => HttpResponse::NotFound()
            .json(serde_json::json!({"error": "not_found", "message": "Trajectory not found"})),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Defaults and hard caps for `/api/trajectories/steps`.
const STEP_DEFAULT_LIMIT: i64 = 50;
const STEP_MAX_LIMIT: i64 = 500;
const STEP_DEFAULT_CONTEXT: i64 = 3;
const STEP_MAX_CONTEXT: i64 = 10;
const STEP_DEFAULT_MAX_SCAN: i64 = 500;
const STEP_MAX_MAX_SCAN: i64 = 2000;

#[derive(Deserialize)]
pub struct TrajectoryStepQuery {
    /// Comma-separated step categories, matched as OR. Omit for every step.
    pub category: Option<String>,
    pub agent_name: Option<String>,
    pub project: Option<String>,
    pub source: Option<String>,
    pub session_id: Option<String>,
    pub limit: Option<i64>,
    pub context: Option<i64>,
    pub max_scan: Option<i64>,
}

/// GET /api/trajectories/steps
///
/// Must be registered before `/api/trajectories/{session_id}`.
#[get("/api/trajectories/steps")]
pub async fn list_trajectory_steps(
    state: web::Data<LocalState>,
    query: web::Query<TrajectoryStepQuery>,
) -> impl Responder {
    // An unknown category is rejected rather than ignored: silently dropping it
    // would return unrelated steps that look like a legitimate empty result.
    let mut categories = Vec::new();
    if let Some(raw) = query.category.as_deref() {
        for token in raw.split(',').filter(|t| !t.trim().is_empty()) {
            match StepCategory::parse(token) {
                Some(c) => categories.push(c),
                None => {
                    return HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "invalid_category",
                        "message": format!("Unknown category '{}'", token.trim()),
                        "valid_categories": StepCategory::ALL
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>(),
                    }));
                }
            }
        }
    }

    let Some(tstore) = state.trajectory_store() else {
        return HttpResponse::Ok().json(serde_json::json!({
            "hits": [], "scanned_trajectories": 0,
            "truncated": false, "skipped_unparsable": 0
        }));
    };

    let limit = match query.limit {
        Some(v) if v > 0 => v.min(STEP_MAX_LIMIT),
        _ => STEP_DEFAULT_LIMIT,
    };
    let context_radius = match query.context {
        Some(v) if v >= 0 => v.min(STEP_MAX_CONTEXT),
        _ => STEP_DEFAULT_CONTEXT,
    };
    let max_scan = match query.max_scan {
        Some(v) if v > 0 => v.min(STEP_MAX_MAX_SCAN),
        _ => STEP_DEFAULT_MAX_SCAN,
    };

    let filter = StepScanFilter {
        project: query.project.clone(),
        source: query.source.clone(),
        agent_name: query.agent_name.clone(),
        session_id: query.session_id.clone(),
        categories,
        limit,
        context_radius,
        max_scan,
    };
    match tstore.scan_steps(&filter) {
        Ok(outcome) => HttpResponse::Ok().json(outcome),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, test};
    use agentsight_trajectory_collector::TrajectoryStore;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    fn make_state(store: Option<Arc<TrajectoryStore>>) -> web::Data<LocalState> {
        web::Data::new(LocalState {
            trajectory_store: Arc::new(RwLock::new(store)),
            db_path: PathBuf::from("/nonexistent/trajectories.db"),
            reuse_store: None,
            reuse_llm_judge_enabled: false,
        })
    }

    fn make_state_with_store(
        store: Arc<TrajectoryStore>,
        db_path: PathBuf,
    ) -> web::Data<LocalState> {
        web::Data::new(LocalState {
            trajectory_store: Arc::new(RwLock::new(Some(store))),
            db_path,
            reuse_store: None,
            reuse_llm_judge_enabled: false,
        })
    }

    #[actix_web::test]
    async fn test_list_trajectories_no_store() {
        let state = make_state(None);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .service(list_trajectories)
                .service(trajectory_filters)
                .service(get_trajectory_detail),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/trajectories")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = test::TestRequest::get()
            .uri("/api/trajectories/filters")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = test::TestRequest::get()
            .uri("/api/trajectories/nonexistent")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn test_list_trajectories_with_store() {
        let tmp = std::env::temp_dir().join("agentsight_traj_handler_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let db_path = tmp.join("trajectories.db");
        let store = Arc::new(TrajectoryStore::new_with_path(&db_path).unwrap());
        let state = make_state_with_store(store, db_path);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .service(list_trajectories)
                .service(trajectory_filters),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/trajectories")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = test::TestRequest::get()
            .uri("/api/trajectories/filters")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[actix_web::test]
    async fn test_get_trajectory_detail_not_found() {
        let tmp = std::env::temp_dir().join("agentsight_traj_detail_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let db_path = tmp.join("trajectories.db");
        let store = Arc::new(TrajectoryStore::new_with_path(&db_path).unwrap());
        let state = make_state_with_store(store, db_path);
        let app =
            test::init_service(App::new().app_data(state).service(get_trajectory_detail)).await;

        let req = test::TestRequest::get()
            .uri("/api/trajectories/missing-session")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[actix_web::test]
    async fn test_get_trajectory_detail_found() {
        let tmp = std::env::temp_dir().join("agentsight_traj_found_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let db_path = tmp.join("trajectories.db");
        let store = Arc::new(TrajectoryStore::new_with_path(&db_path).unwrap());
        let atif = r#"{"schema_version":"ATIF-v1.7","session_id":"sess-found","steps":[]}"#;
        let record = agentsight_trajectory_collector::TrajectoryRecord {
            session_id: "sess-found".to_string(),
            schema_version: "ATIF-v1.7".to_string(),
            agent_name: "test".to_string(),
            model_name: None,
            num_steps: 0,
            total_prompt_tokens: None,
            total_completion_tokens: None,
            start_time: None,
            end_time: None,
            first_user_message: None,
            last_user_message: None,
            atif_json: atif.to_string(),
            project: "test".to_string(),
            source: "test".to_string(),
            is_subagent: false,
            file_path: String::new(),
            file_size: 0,
            file_mtime_ns: 0,
        };
        store.upsert_trajectory(&record).unwrap();

        let state = make_state_with_store(store, db_path);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .service(get_trajectory_detail)
                .service(list_trajectories)
                .service(trajectory_filters),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/trajectories/sess-found")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = test::TestRequest::get()
            .uri("/api/trajectories?limit=10")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = test::TestRequest::get()
            .uri("/api/trajectories?limit=0")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
