//! Reuse-label endpoints (Linux).
//!
//! Thin wrappers: every decision — which trajectories to examine, what a label
//! means, which status code a failure warrants — lives in [`crate::reuse::api`]
//! so this file and its macOS counterpart cannot drift apart.

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, Responder, get, post, web};

use super::AppState;
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
fn labels(data: &AppState) -> std::result::Result<&crate::reuse::ReuseStore, HttpResponse> {
    data.reuse_store.as_deref().ok_or_else(|| {
        error_response(&ReuseApiError::ReuseUnavailable(
            "reuse.db could not be opened; see the server log".to_string(),
        ))
    })
}

/// POST /api/reuse/triage
///
/// Labels collected trajectories and records the automatic verdicts. Safe to
/// call repeatedly: unchanged content under unchanged rules is skipped.
///
/// Thresholds come from the defaults for now; wiring them to the
/// `causal_labeling` feature flag arrives with the confirmation endpoints.
#[post("/reuse/triage")]
pub async fn run_triage(
    data: web::Data<AppState>,
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

/// GET /api/reuse/sessions
///
/// Lists label rows with the effective label already resolved.
#[get("/reuse/sessions")]
pub async fn list_sessions(
    data: web::Data<AppState>,
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
    data: web::Data<AppState>,
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
    data: web::Data<AppState>,
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
pub async fn label_stats(data: web::Data<AppState>) -> impl Responder {
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
/// Asks the model to label trajectories the rules could not place. Off unless
/// `features.reuse_llm_judge` is set, because every judgement is a paid call.
#[post("/reuse/judge")]
pub async fn run_judgements(
    data: web::Data<AppState>,
    body: web::Json<JudgeQuery>,
) -> impl Responder {
    if !data.reuse_llm_judge_enabled {
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "llm_judge_disabled",
            "message": "second-level labelling is off; set features.reuse_llm_judge to enable it",
        }));
    }
    let store = match labels(&data) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let Some(trajectories) = data.trajectory_store() else {
        return error_response(&ReuseApiError::TrajectoriesUnavailable);
    };
    let Some(optimize) = data.optimize.as_ref() else {
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "llm_not_configured",
            "message": "no LLM is configured; second-level labelling needs an API key",
        }));
    };
    let client = match optimize.build_client() {
        Ok(client) => client,
        Err(response) => return response,
    };
    match api::run_judgements(&trajectories, store, &client, &body).await {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => error_response(&error),
    }
}
