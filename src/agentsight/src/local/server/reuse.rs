//! Reuse-label endpoints (macOS local server).
//!
//! Mirrors `server::reuse` handler for handler. Both delegate to
//! [`crate::reuse::api`], which is where the behaviour actually lives — the
//! labels a macOS reader sees must be the labels a Linux reader sees.

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, Responder, get, post, web};

use super::LocalState;
use crate::reuse::api::{
    self, BatchConfirmRequest, JudgeQuery, LabelDecisionRequest, ReuseApiError, SessionsQuery,
    TriageQuery,
};
use crate::reuse::triage::TriageConfig;

/// Renders a failure using the status and body the shared layer decided.
fn error_response(error: &ReuseApiError) -> HttpResponse {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    HttpResponse::build(status).json(error.body())
}

/// The label store, or the response explaining why labels are unavailable.
fn labels(data: &LocalState) -> std::result::Result<&crate::reuse::ReuseStore, HttpResponse> {
    data.reuse_store.as_deref().ok_or_else(|| {
        error_response(&ReuseApiError::ReuseUnavailable(
            "reuse.db could not be opened; see the server log".to_string(),
        ))
    })
}

/// POST /api/reuse/triage — see `server::reuse::run_triage`.
#[post("/reuse/triage")]
pub async fn run_triage(
    data: web::Data<LocalState>,
    query: web::Query<TriageQuery>,
) -> impl Responder {
    let store = match labels(&data) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match api::run_triage(
        data.trajectory_store().as_deref(),
        store,
        &query,
        &TriageConfig::default(),
    ) {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => {
            log::warn!("Reuse triage failed: {error}");
            error_response(&error)
        }
    }
}

/// GET /api/reuse/sessions — see `server::reuse::list_sessions`.
#[get("/reuse/sessions")]
pub async fn list_sessions(
    data: web::Data<LocalState>,
    query: web::Query<SessionsQuery>,
) -> impl Responder {
    let store = match labels(&data) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match api::list_sessions(store, &query) {
        Ok(sessions) => HttpResponse::Ok().json(serde_json::json!({
            "count": sessions.len(),
            "sessions": sessions,
        })),
        Err(error) => error_response(&error),
    }
}

/// POST /api/reuse/sessions/{session_id}/label
///
/// Confirms or replaces one trajectory's label. The only route to `bad`: the
/// deterministic rules never accuse.
#[post("/reuse/sessions/{session_id}/label")]
pub async fn apply_label(
    data: web::Data<LocalState>,
    path: web::Path<String>,
    body: web::Json<LabelDecisionRequest>,
) -> impl Responder {
    let store = match labels(&data) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match api::apply_label(store, &path.into_inner(), &body) {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => error_response(&error),
    }
}

/// POST /api/reuse/sessions/labels:batch-confirm
///
/// Endorses several automatic verdicts in one call; missing ids are skipped so a
/// stale entry cannot fail a whole page.
#[post("/reuse/sessions/labels:batch-confirm")]
pub async fn confirm_labels(
    data: web::Data<LocalState>,
    body: web::Json<BatchConfirmRequest>,
) -> impl Responder {
    let store = match labels(&data) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match api::confirm_labels(store, &body) {
        Ok(confirmed) => HttpResponse::Ok().json(serde_json::json!({
            "confirmed": confirmed.len(),
            "session_ids": confirmed,
        })),
        Err(error) => error_response(&error),
    }
}

/// GET /api/reuse/label-stats
///
/// How often a person accepted or overturned each rule's verdict.
#[get("/reuse/label-stats")]
pub async fn label_stats(data: web::Data<LocalState>) -> impl Responder {
    let store = match labels(&data) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match api::label_stats(store) {
        Ok(stats) => HttpResponse::Ok().json(serde_json::json!({
            "count": stats.len(),
            "rules": stats,
        })),
        Err(error) => error_response(&error),
    }
}

/// POST /api/reuse/judge
///
/// Asks the model to label trajectories the rules could not place. Off unless a
/// configuration file sets `features.reuse_llm_judge`, because every judgement
/// is a paid request.
///
/// Takes the optimize state rather than [`LocalState`] alone: the label store
/// and the model client live in different places on this side, and that state
/// already carries both.
#[post("/reuse/judge")]
pub async fn run_judgements(
    data: web::Data<super::optimize::OptimizeAppState>,
    body: web::Json<JudgeQuery>,
) -> impl Responder {
    let local: &LocalState = &data.local_state;
    if !local.reuse_llm_judge_enabled {
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "llm_judge_disabled",
            "message": "second-level labelling is off; pass --config with features.reuse_llm_judge",
        }));
    }
    let store = match labels(local) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let Some(trajectories) = local.trajectory_store() else {
        return error_response(&ReuseApiError::TrajectoriesUnavailable);
    };
    let client = match data.optimize.build_client() {
        Ok(client) => client,
        Err(response) => return response,
    };
    match api::run_judgements(&trajectories, store, &client, &body).await {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => error_response(&error),
    }
}
