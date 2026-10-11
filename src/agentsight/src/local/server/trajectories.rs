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
    if let Err(response) = reject_unknown_label_tokens(&query) {
        return response;
    }
    let Some(tstore) = state.trajectory_store() else {
        return HttpResponse::Ok().json(Vec::<serde_json::Value>::new());
    };
    let limit = match query.limit {
        Some(v) if v > 0 => v.min(TRAJECTORY_MAX_LIMIT),
        _ => TRAJECTORY_DEFAULT_LIMIT,
    };
    // Label filters live in `reuse.db`, so they are applied after the
    // `trajectories.db` query. Fetching only the newest `limit` rows first
    // would hide every match older than them, so a labelled query reads the
    // whole summary set (payload-free rows) and truncates after filtering —
    // same contract as the Linux endpoint.
    let fetch_limit = if reuse_label_filter_requested(&query) {
        i64::MAX
    } else {
        limit
    };
    match tstore.list_summaries(
        query.project.as_deref(),
        query.source.as_deref(),
        query.agent_name.as_deref(),
        fetch_limit,
    ) {
        Ok(mut rows) => {
            filter_rows_by_reuse_labels(state.as_ref(), &query, &mut rows);
            rows.truncate(limit as usize);
            HttpResponse::Ok().json(rows)
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Whether the query asks for any `reuse.db`-backed filtering.
fn reuse_label_filter_requested(query: &TrajectoryQuery) -> bool {
    query.label.is_some() || query.exclude_label.is_some() || query.human_backed.is_some()
}

/// Parses a comma-separated label list, rejecting unknown tokens.
///
/// Silently dropping a typo turned `?label=goodd` into "no label matches",
/// which the caller cannot tell from an empty result. The Linux endpoint
/// answers 400 for the same input; a skill must get the same answer on
/// either platform.
fn parse_label_tokens(
    field: &str,
    raw: &str,
) -> Result<Vec<crate::reuse::TrajectoryLabel>, HttpResponse> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|token| {
            crate::reuse::TrajectoryLabel::parse(token).ok_or_else(|| {
                HttpResponse::BadRequest().json(serde_json::json!({
                    "error": {
                        "code": "bad_request",
                        "message": format!("{field} '{token}' is not a known trajectory label"),
                        "retryable": false,
                    }
                }))
            })
        })
        .collect()
}

fn reject_unknown_label_tokens(query: &TrajectoryQuery) -> Result<(), HttpResponse> {
    if let Some(raw) = query.label.as_deref() {
        parse_label_tokens("label", raw)?;
    }
    if let Some(raw) = query.exclude_label.as_deref() {
        parse_label_tokens("exclude_label", raw)?;
    }
    Ok(())
}

/// Applies the reuse-label query parameters; mirrors the Linux endpoint's
/// filter so a skill gets the same answer on either platform.
fn filter_rows_by_reuse_labels(
    state: &LocalState,
    query: &TrajectoryQuery,
    rows: &mut Vec<agentsight_trajectory_collector::TrajectorySummary>,
) {
    if !reuse_label_filter_requested(query) {
        return;
    }
    let Some(labels) = state.reuse_store.as_deref() else {
        // No label store: nothing has been assessed, so a positive filter
        // matches nothing. `human_backed=false` is the opposite polarity —
        // nothing is settled, so every row belongs in the answer.
        if query.label.is_some() || query.human_backed == Some(true) {
            rows.clear();
        }
        return;
    };
    // Tokens were validated when the request entered the handler, so every
    // one of them parses here.
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
    // `human_backed` is a tri-state filter: absent keeps every row, `true`
    // keeps only human-settled rows, and `false` keeps only rows no person has
    // settled — including never-triaged rows, which have no label row at all.
    // Mirror of the Linux endpoint's filter.
    let human_backed = query.human_backed;
    let settled: Option<std::collections::HashSet<String>> = human_backed.map(|_| {
        labels
            .list_labels(&crate::reuse::LabelFilter::default())
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.is_human_backed())
            .map(|l| l.session_id)
            .collect()
    });
    rows.retain(|row| {
        let id = row.session_id.as_str();
        keep.as_ref().is_none_or(|set| set.contains(id))
            && drop.as_ref().is_none_or(|set| !set.contains(id))
            && settled
                .as_ref()
                .is_none_or(|set| human_backed == Some(set.contains(id)))
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
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, RwLock};

    use crate::config::StorageConfig;
    use crate::database::{
        DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseRole, DatabaseSpec,
    };

    fn manager(db_path: &Path) -> Arc<DatabaseManager> {
        Arc::new(
            DatabaseManager::new(
                DatabaseRole::LocalServer,
                [DatabaseSpec::new(
                    DatabaseId::Trajectories,
                    db_path,
                    DatabaseAccess::ReadOnly,
                    DatabaseCoverage::Partial,
                )],
            )
            .unwrap(),
        )
    }

    fn make_state(store: Option<Arc<TrajectoryStore>>) -> web::Data<LocalState> {
        let db_path = PathBuf::from("/nonexistent/trajectories.db");
        web::Data::new(LocalState {
            trajectory_store: Arc::new(RwLock::new(store)),
            database_manager: manager(&db_path),
            db_path,
            storage_config: StorageConfig::default(),
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
            database_manager: manager(&db_path),
            db_path,
            storage_config: StorageConfig::default(),
            reuse_store: None,
            reuse_llm_judge_enabled: false,
        })
    }

    fn make_state_with_reuse(
        store: Arc<TrajectoryStore>,
        reuse: Arc<crate::reuse::ReuseStore>,
        db_path: PathBuf,
    ) -> web::Data<LocalState> {
        web::Data::new(LocalState {
            trajectory_store: Arc::new(RwLock::new(Some(store))),
            database_manager: manager(&db_path),
            db_path,
            storage_config: StorageConfig::default(),
            reuse_store: Some(reuse),
            reuse_llm_judge_enabled: false,
        })
    }

    /// Same contract as the Linux endpoint: a reuse-label filter must not be
    /// applied after the SQL cap, or a labelled trajectory older than the
    /// newest `limit` rows is silently dropped from the answer.
    #[actix_web::test]
    async fn trajectory_label_filter_keeps_older_labelled_rows() {
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_local_label_filter_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let db_path = tmp.join("trajectories.db");
        {
            let store = TrajectoryStore::new_with_path(&db_path).unwrap();
            for session in ["old-good", "new-1", "new-2"] {
                let record = agentsight_trajectory_collector::TrajectoryRecord {
                    session_id: session.to_string(),
                    schema_version: "ATIF-v1.7".to_string(),
                    agent_name: "qoder".to_string(),
                    model_name: None,
                    num_steps: 1,
                    total_prompt_tokens: None,
                    total_completion_tokens: None,
                    start_time: None,
                    end_time: None,
                    first_user_message: None,
                    last_user_message: None,
                    atif_json: "{\"schema_version\":\"ATIF-v1.7\",\"steps\":[]}".to_string(),
                    project: "p".to_string(),
                    source: "qoder".to_string(),
                    is_subagent: false,
                    file_path: format!("/tmp/{session}.jsonl"),
                    file_size: 1,
                    file_mtime_ns: 1,
                };
                store.upsert_trajectory(&record).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        let store = Arc::new(TrajectoryStore::new_with_path(&db_path).unwrap());
        let reuse = crate::reuse::ReuseStore::open_private(&tmp).unwrap();
        reuse
            .upsert_auto_label(
                "old-good",
                crate::reuse::label::TrajectoryIdentity {
                    title: None,
                    project: "p".to_string(),
                    source: "qoder".to_string(),
                    agent_name: "qoder".to_string(),
                    started_at: None,
                    is_subagent: false,
                },
                crate::reuse::TriageOutcome {
                    label: crate::reuse::TrajectoryLabel::Good,
                    reason: "fixture".to_string(),
                    metrics: crate::reuse::TriageMetrics {
                        n_steps: 1,
                        n_user_turns: 1,
                        n_tool_calls: 0,
                        max_agent_len: 1,
                    },
                    n_findings: 0,
                    rules: Vec::new(),
                },
                "hash",
                "version",
            )
            .unwrap();

        let state = make_state_with_reuse(store, Arc::new(reuse), db_path.clone());
        let app = test::init_service(App::new().app_data(state).service(list_trajectories)).await;

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/trajectories?label=good&limit=2")
                .to_request(),
        )
        .await;
        assert!(resp.status().is_success());
        let rows: serde_json::Value = test::read_body_json(resp).await;
        let arr = rows.as_array().unwrap();
        assert_eq!(arr.len(), 1, "labelled row must survive the limit: {rows}");
        assert_eq!(arr[0]["session_id"], "old-good");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[actix_web::test]
    async fn trajectory_human_backed_false_excludes_settled_rows() {
        // Mirror of the Linux endpoint's contract: `human_backed=false` is a
        // public query field, but the filter used to recognise only
        // `Some(true)` and silently answered with every trajectory, including
        // human-settled ones — indistinguishable from a filter that matched
        // everything.
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_local_human_backed_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let db_path = tmp.join("trajectories.db");
        {
            let store = TrajectoryStore::new_with_path(&db_path).unwrap();
            for session in ["settled", "untriaged"] {
                let record = agentsight_trajectory_collector::TrajectoryRecord {
                    session_id: session.to_string(),
                    schema_version: "ATIF-v1.7".to_string(),
                    agent_name: "qoder".to_string(),
                    model_name: None,
                    num_steps: 1,
                    total_prompt_tokens: None,
                    total_completion_tokens: None,
                    start_time: None,
                    end_time: None,
                    first_user_message: None,
                    last_user_message: None,
                    atif_json: "{\"schema_version\":\"ATIF-v1.7\",\"steps\":[]}".to_string(),
                    project: "p".to_string(),
                    source: "qoder".to_string(),
                    is_subagent: false,
                    file_path: format!("/tmp/{session}.jsonl"),
                    file_size: 1,
                    file_mtime_ns: 1,
                };
                store.upsert_trajectory(&record).unwrap();
            }
        }

        let store = Arc::new(TrajectoryStore::new_with_path(&db_path).unwrap());
        let reuse = crate::reuse::ReuseStore::open_private(&tmp).unwrap();
        reuse
            .upsert_auto_label(
                "settled",
                crate::reuse::label::TrajectoryIdentity {
                    title: Some("settled by a person".to_string()),
                    project: "p".to_string(),
                    source: "qoder".to_string(),
                    agent_name: "qoder".to_string(),
                    started_at: None,
                    is_subagent: false,
                },
                crate::reuse::TriageOutcome {
                    label: crate::reuse::TrajectoryLabel::Good,
                    reason: "fixture".to_string(),
                    metrics: crate::reuse::TriageMetrics {
                        n_steps: 1,
                        n_user_turns: 1,
                        n_tool_calls: 0,
                        max_agent_len: 1,
                    },
                    n_findings: 0,
                    rules: Vec::new(),
                },
                "hash",
                "version",
            )
            .unwrap();
        reuse
            .apply_decision("settled", crate::reuse::LabelAction::Confirm, "alice", None)
            .unwrap();

        let state = make_state_with_reuse(store, Arc::new(reuse), db_path.clone());
        let app = test::init_service(App::new().app_data(state).service(list_trajectories)).await;

        let session_ids = |body: &serde_json::Value| -> Vec<String> {
            body.as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_id"].as_str().unwrap().to_string())
                .collect()
        };

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/trajectories?human_backed=false")
                .to_request(),
        )
        .await;
        assert!(resp.status().is_success());
        let rows: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(
            session_ids(&rows),
            vec!["untriaged".to_string()],
            "human_backed=false must exclude the settled row, not ignore the field"
        );

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/trajectories?human_backed=true")
                .to_request(),
        )
        .await;
        assert!(resp.status().is_success());
        let rows: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(session_ids(&rows), vec!["settled".to_string()]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[actix_web::test]
    async fn trajectory_label_typos_are_rejected() {
        // Same contract as the Linux endpoint: an unknown token is the
        // caller's mistake, not an empty result set.
        let state = make_state(None);
        let app = test::init_service(App::new().app_data(state).service(list_trajectories)).await;

        for uri in [
            "/api/trajectories?label=goodd",
            "/api/trajectories?exclude_label=nope",
            "/api/trajectories?label=good&exclude_label=nope",
        ] {
            let resp =
                test::call_service(&app, test::TestRequest::get().uri(uri).to_request()).await;
            assert_eq!(resp.status().as_u16(), 400, "{uri} must be rejected");
            let body: serde_json::Value = test::read_body_json(resp).await;
            assert_eq!(body["error"]["code"], "bad_request");
        }

        // A known token still passes.
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/trajectories?label=good")
                .to_request(),
        )
        .await;
        assert!(resp.status().is_success());
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
