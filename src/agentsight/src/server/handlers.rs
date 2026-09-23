//! API request handlers

use std::collections::HashMap;

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, Responder, get, post, web};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use agentsight_atif::StepCategory;
use agentsight_trajectory_collector::StepScanFilter;

use super::AppState;
use crate::agent_sec::{AgentSecClient, AgentSecClientError, DaemonResponse};
use crate::grader::{
    EvaluationRequest, EvaluationResponse, GraderError, GraderType, RULE_GRADER_VERSION,
    RuleGrader, TargetType, load_conversation_input,
};
use crate::health::AgentHealthStatus;
use crate::storage::sqlite::GenAISqliteStore;
use crate::storage::sqlite::genai::{ModelTimeseriesBucket, TimeseriesBucket};

// ─── Prometheus helpers ───────────────────────────────────────────────────────

/// Escape a Prometheus label value per the text format spec:
/// backslash → \\, double-quote → \", newline → \n
fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// GET /health — health check endpoint
#[get("/health")]
pub async fn health(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": data.start_time.elapsed().as_secs()
    }))
}

// ─── Authentication endpoints ────────────────────────────────────────────────

/// POST /api/auth/login
///
/// Accepts `{"token": "..."}` and sets a signed session cookie on success.
pub async fn auth_login(
    data: web::Data<AppState>,
    body: web::Json<serde_json::Value>,
) -> impl Responder {
    let candidate = body.get("token").and_then(|v| v.as_str()).unwrap_or("");

    if !data.auth.verify_token(candidate) {
        return HttpResponse::Unauthorized()
            .json(json!({"error": "invalid_token", "message": "The provided token is incorrect"}));
    }

    // Create a signed session cookie (24-hour TTL)
    let cookie_value = data.auth.create_session_cookie(24 * 3600);
    let cookie = actix_web::cookie::Cookie::build("agentsight_session", cookie_value)
        .path("/")
        .http_only(true)
        .same_site(actix_web::cookie::SameSite::Lax)
        .max_age(actix_web::cookie::time::Duration::hours(24))
        .finish();

    HttpResponse::Ok()
        .cookie(cookie)
        .json(json!({"status": "authenticated"}))
}

/// GET /api/auth/status
///
/// Returns whether authentication is enabled plus the capability list the
/// dashboard should render (probed from installed companion components).
/// Exempt from auth middleware.
#[get("/status")]
pub async fn auth_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(json!({
        "auth_enabled": data.auth.enabled,
        "mode": "linux",
        "capabilities": super::capabilities::app_capabilities(),
    }))
}

/// GET /api/auth/verify
///
/// Checks whether the current request carries a valid session.  Exempt from
/// auth middleware so the frontend can probe authentication state.
#[get("/verify")]
pub async fn auth_verify(data: web::Data<AppState>, req: actix_web::HttpRequest) -> impl Responder {
    if !data.auth.enabled {
        return HttpResponse::Ok().json(json!({"authenticated": true}));
    }

    // Loopback requests are trusted — consistent with AuthMiddleware bypass.
    let is_loopback = req
        .peer_addr()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false);
    if is_loopback {
        return HttpResponse::Ok().json(json!({"authenticated": true}));
    }

    // Check session cookie
    let authenticated = req
        .cookie("agentsight_session")
        .map(|c| data.auth.verify_session_cookie(c.value()))
        .unwrap_or(false);

    HttpResponse::Ok().json(json!({"authenticated": authenticated}))
}

// ─── Session / Trace query endpoints ───────────────────────────────────────

/// Query parameters for /api/sessions
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    /// Start of time range in nanoseconds (default: 24 h ago)
    pub start_ns: Option<i64>,
    /// End of time range in nanoseconds (default: now)
    pub end_ns: Option<i64>,
    /// Include auxiliary calls (recap, web_search) in results (default: false)
    pub include_auxiliary: Option<bool>,
}

/// Query parameters for a Session resource timeline.
#[derive(Debug, Deserialize)]
pub struct SessionResourceQuery {
    /// Optional lower timestamp bound in Unix epoch nanoseconds.
    pub start_ns: Option<i64>,
    /// Optional upper timestamp bound in Unix epoch nanoseconds.
    pub end_ns: Option<i64>,
    /// Maximum number of raw samples returned after stride-based downsampling.
    pub max_points: Option<usize>,
}

/// GET /api/sessions?start_ns=<i64>&end_ns=<i64>
///
/// Returns a list of gen_ai.session_id values with aggregated stats.
#[get("/sessions")]
pub async fn list_sessions(
    data: web::Data<AppState>,
    query: web::Query<SessionQuery>,
) -> impl Responder {
    let db_path = &data.storage_path;

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64); // 24 h

    match GenAISqliteStore::new_with_path(db_path, crate::config::InsertStoragePolicy::default()) {
        Ok(store) => {
            match store.list_sessions(start_ns, end_ns, query.include_auxiliary.unwrap_or(false)) {
                Ok(sessions) => HttpResponse::Ok().json(sessions),
                Err(e) => HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": e.to_string()})),
            }
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/sessions/{session_id}/traces?start_ns=<i64>&end_ns=<i64>
///
/// Returns conversations belonging to a session with token stats.
/// Optional `start_ns`/`end_ns` query parameters filter conversations by time.
#[get("/sessions/{session_id}/traces")]
pub async fn list_traces_by_session(
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<TimeRangeQuery>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let session_id = path.into_inner();

    let start_ns = query.start_ns;
    let end_ns = query.end_ns;

    match GenAISqliteStore::new_with_path(db_path, crate::config::InsertStoragePolicy::default()) {
        Ok(store) => match store.list_traces_by_session(
            &session_id,
            start_ns,
            end_ns,
            query.include_auxiliary.unwrap_or(false),
        ) {
            Ok(traces) => HttpResponse::Ok().json(traces),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/sessions/{session_id}/resources
///
/// Returns process-level CPU/RSS observations during the Session together
/// with LLM and inferred Tool Call intervals on the same epoch-nanosecond clock.
#[get("/sessions/{session_id}/resources")]
pub async fn get_session_resources(
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<SessionResourceQuery>,
) -> impl Responder {
    const DEFAULT_MAX_POINTS: usize = 2_000;
    const MAX_POINTS_LIMIT: usize = 10_000;

    if matches!((query.start_ns, query.end_ns), (Some(start), Some(end)) if start > end) {
        return HttpResponse::BadRequest()
            .json(json!({"error": "start_ns must not exceed end_ns"}));
    }
    let max_points = query.max_points.unwrap_or(DEFAULT_MAX_POINTS);
    if max_points == 0 || max_points > MAX_POINTS_LIMIT {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("max_points must be between 1 and {MAX_POINTS_LIMIT}")
        }));
    }

    let db_path = data.storage_path.clone();
    let session_id = path.into_inner();
    let start_ns = query.start_ns;
    let end_ns = query.end_ns;
    let loaded = web::block(move || {
        let store = GenAISqliteStore::new_with_path(
            &db_path,
            crate::config::InsertStoragePolicy::default(),
        )
        .map_err(|error| error.to_string())?;
        store
            .get_session_resource_timeline(&session_id, start_ns, end_ns, max_points)
            .map_err(|error| error.to_string())
    })
    .await;

    match loaded {
        Ok(Ok(Some(timeline))) => HttpResponse::Ok().json(timeline),
        Ok(Ok(None)) => HttpResponse::NotFound().json(json!({"error": "session not found"})),
        Ok(Err(error)) => HttpResponse::InternalServerError().json(json!({"error": error})),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": format!("resource query worker failed: {error}")})),
    }
}

/// GET /api/traces/{trace_id}
///
/// Returns detailed LLM call events for a trace.
#[get("/traces/{trace_id}")]
pub async fn get_trace_detail(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let trace_id = path.into_inner();

    match GenAISqliteStore::new_with_path(db_path, crate::config::InsertStoragePolicy::default()) {
        Ok(store) => match store.get_trace_events(&trace_id) {
            Ok(events) => HttpResponse::Ok().json(events),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/conversations/{conversation_id}
///
/// Returns detailed LLM call events for a conversation (user query).
#[get("/conversations/{conversation_id}")]
pub async fn get_conversation_events(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let conversation_id = path.into_inner();

    match GenAISqliteStore::new_with_path(db_path, crate::config::InsertStoragePolicy::default()) {
        Ok(store) => match store.get_events_by_conversation(&conversation_id) {
            Ok(events) => HttpResponse::Ok().json(events),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

// ─── Grader endpoints ────────────────────────────────────────────────────────

/// Query parameters for GET /api/grader/latest.
#[derive(Debug, Deserialize)]
pub struct GraderLatestQuery {
    pub target_type: String,
    pub target_id: String,
}

/// POST /api/grader/evaluate
///
/// Manually evaluate a conversation snapshot with the rule-based grader.
#[post("/grader/evaluate")]
pub async fn evaluate_grader(
    data: web::Data<AppState>,
    body: web::Json<EvaluationRequest>,
) -> impl Responder {
    let target_type = match parse_grader_target_type(&body.target_type) {
        Ok(target_type) => target_type,
        Err(error) => return grader_error_response(error),
    };

    if body.target_id.trim().is_empty() {
        return HttpResponse::BadRequest()
            .json(json!({"error": "bad_request", "message": "target_id is required"}));
    }

    let input = match load_conversation_input(
        &data.storage_path,
        data.interruption_store.as_deref(),
        &body.target_id,
        body.force,
    ) {
        Ok(input) => input,
        Err(error) => return grader_error_response(error),
    };
    let store = &data.evaluation_store;

    match store.find_completed(
        target_type,
        &body.target_id,
        &input.input_hash,
        GraderType::Rule,
        RULE_GRADER_VERSION,
    ) {
        Ok(Some(record)) => {
            if let Some(result) = record.result {
                return HttpResponse::Ok().json(EvaluationResponse {
                    result,
                    reused_existing_run: true,
                });
            }
        }
        Ok(None) => {}
        Err(error) => return grader_error_response(error),
    }

    let result = RuleGrader::evaluate(&input);
    match store.insert_completed(&result) {
        Ok(true) => {}
        Ok(false) => {
            return match store.find_completed(
                target_type,
                &body.target_id,
                &input.input_hash,
                GraderType::Rule,
                RULE_GRADER_VERSION,
            ) {
                Ok(Some(record)) => {
                    if let Some(result) = record.result {
                        HttpResponse::Ok().json(EvaluationResponse {
                            result,
                            reused_existing_run: true,
                        })
                    } else {
                        grader_error_response(GraderError::Storage(
                            "existing evaluation run is missing result_json".to_string(),
                        ))
                    }
                }
                Ok(None) => grader_error_response(GraderError::Storage(
                    "evaluation insert was ignored but no completed run was found".to_string(),
                )),
                Err(error) => grader_error_response(error),
            };
        }
        Err(error) => return grader_error_response(error),
    }

    HttpResponse::Ok().json(EvaluationResponse {
        result,
        reused_existing_run: false,
    })
}

/// GET /api/grader/latest?target_type=conversation&target_id=<id>
///
/// Return the latest completed evaluation result for a conversation.
#[get("/grader/latest")]
pub async fn latest_grader(
    data: web::Data<AppState>,
    query: web::Query<GraderLatestQuery>,
) -> impl Responder {
    let target_type = match parse_grader_target_type(&query.target_type) {
        Ok(target_type) => target_type,
        Err(error) => return grader_error_response(error),
    };

    if query.target_id.trim().is_empty() {
        return HttpResponse::BadRequest()
            .json(json!({"error": "bad_request", "message": "target_id is required"}));
    }

    match data
        .evaluation_store
        .latest_completed(target_type, &query.target_id)
    {
        Ok(Some(record)) => HttpResponse::Ok().json(record.result),
        Ok(None) => HttpResponse::Ok().json(serde_json::Value::Null),
        Err(error) => grader_error_response(error),
    }
}

fn parse_grader_target_type(value: &str) -> Result<TargetType, GraderError> {
    match value {
        "conversation" => Ok(TargetType::Conversation),
        other => Err(GraderError::UnsupportedTarget(other.to_string())),
    }
}

fn grader_error_response(error: GraderError) -> HttpResponse {
    match error {
        GraderError::ConversationNotFound(id) => HttpResponse::NotFound().json(json!({
            "error": "conversation_not_found",
            "message": format!("Conversation not found: {id}"),
        })),
        GraderError::ConversationNotReady { pending_count } => HttpResponse::Conflict().json(json!({
            "error": "conversation_not_ready",
            "message": "Conversation still has pending LLM calls. Retry after completion or use force=true.",
            "pending_call_count": pending_count,
        })),
        GraderError::UnsupportedTarget(target) => HttpResponse::BadRequest().json(json!({
            "error": "unsupported_target",
            "message": format!("Unsupported target_type: {target}. MVP supports only conversation."),
        })),
        GraderError::Storage(message) => HttpResponse::InternalServerError().json(json!({
            "error": "storage_error",
            "message": message,
        })),
        GraderError::Json(error) => HttpResponse::InternalServerError().json(json!({
            "error": "json_error",
            "message": error.to_string(),
        })),
    }
}

// ─── Agent-name & time-series endpoints ────────────────────────────────────

/// Query parameters shared by agent-name and time-series endpoints
#[derive(Debug, Deserialize)]
pub struct TimeRangeQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    /// Include auxiliary calls (recap, web_search) in results (default: false)
    pub include_auxiliary: Option<bool>,
}

/// Query parameters for time-series endpoints
#[derive(Debug, Deserialize)]
pub struct TimeseriesQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    /// Filter by a specific agent name (optional)
    pub agent_name: Option<String>,
    /// Number of buckets (default 30)
    pub buckets: Option<u32>,
}

/// Query parameters for LLM latency percentile metrics.
#[derive(Debug, Deserialize)]
pub struct LatencyMetricsQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub agent_name: Option<String>,
}

/// GET /api/metrics/latency
#[get("/metrics/latency")]
pub async fn get_latency_metrics(
    data: web::Data<AppState>,
    query: web::Query<LatencyMetricsQuery>,
) -> impl Responder {
    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = match query.start_ns {
        Some(start_ns) => start_ns,
        None => match end_ns.checked_sub(86_400_000_000_000i64) {
            Some(start_ns) => start_ns,
            None => {
                return HttpResponse::BadRequest()
                    .json(serde_json::json!({"error": "default time range is out of bounds"}));
            }
        },
    };
    if start_ns > end_ns {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "start_ns must not exceed end_ns"}));
    }
    match GenAISqliteStore::new_with_path(
        &data.storage_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(store) => match store.get_latency_metrics(start_ns, end_ns, query.agent_name.as_deref())
        {
            Ok(summary) => HttpResponse::Ok().json(summary),
            Err(error) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": error.to_string()})),
        },
        Err(error) => HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": error.to_string()})),
    }
}

/// GET /api/agent-names?start_ns=<i64>&end_ns=<i64>
///
/// Returns a sorted list of distinct agent_name values.
#[get("/agent-names")]
pub async fn list_agent_names(
    data: web::Data<AppState>,
    query: web::Query<TimeRangeQuery>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);

    match GenAISqliteStore::new_with_path(db_path, crate::config::InsertStoragePolicy::default()) {
        Ok(store) => match store.list_agent_names(start_ns, end_ns) {
            Ok(names) => HttpResponse::Ok().json(names),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Response body for /api/timeseries
#[derive(Debug, serde::Serialize)]
pub struct TimeseriesResponse {
    pub token_series: Vec<TimeseriesBucket>,
    pub model_series: Vec<ModelTimeseriesBucket>,
}

/// GET /api/timeseries?start_ns=<i64>&end_ns=<i64>&agent_name=<str>&buckets=<u32>
///
/// Returns time-bucketed token stats (input/output/total) and per-model total-token
/// breakdowns, both within the requested time range.
#[get("/timeseries")]
pub async fn get_timeseries(
    data: web::Data<AppState>,
    query: web::Query<TimeseriesQuery>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);
    let buckets = query.buckets.unwrap_or(30);
    let agent_name = query.agent_name.as_deref();

    match GenAISqliteStore::new_with_path(db_path, crate::config::InsertStoragePolicy::default()) {
        Ok(store) => {
            let token_series =
                match store.get_token_timeseries(start_ns, end_ns, agent_name, buckets) {
                    Ok(v) => v,
                    Err(e) => {
                        return HttpResponse::InternalServerError()
                            .json(serde_json::json!({"error": e.to_string()}));
                    }
                };
            let model_series =
                match store.get_model_timeseries(start_ns, end_ns, agent_name, buckets) {
                    Ok(v) => v,
                    Err(e) => {
                        return HttpResponse::InternalServerError()
                            .json(serde_json::json!({"error": e.to_string()}));
                    }
                };
            HttpResponse::Ok().json(TimeseriesResponse {
                token_series,
                model_series,
            })
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Current UNIX time in nanoseconds
fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

// ─── agent-sec Security Observability endpoints ─────────────────────────────

/// GET /api/security/status
///
/// Reports only whether the agent-sec daemon is reachable. Data-plane failures
/// are surfaced by the individual security query endpoints.
#[get("/security/status")]
pub async fn security_status(data: web::Data<AppState>) -> impl Responder {
    let client = match agent_sec_client(&data) {
        Ok(client) => client,
        Err(err) => {
            return security_state_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon_unreachable",
                json!({ "error": err.to_string() }),
                Some("agent-sec daemon is unavailable"),
            );
        }
    };

    let daemon_health = match call_daemon(client, "daemon.health", json!({})).await {
        Ok(response) if response.ok => response,
        Ok(response) => return daemon_error_response(response),
        Err(err) => {
            return security_state_response(
                client_error_status(&err),
                "daemon_unreachable",
                json!({ "error": err.to_string() }),
                Some("agent-sec daemon is unavailable"),
            );
        }
    };

    security_state_response(
        StatusCode::OK,
        "daemon_reachable",
        json!({
            "daemon": daemon_health.data,
            "socket_path": client_socket_path(&data),
        }),
        None,
    )
}

/// GET /api/security/summary
#[get("/security/summary")]
pub async fn security_summary(
    data: web::Data<AppState>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    proxy_security_query(data, "sec.summary", query_to_params(&query)).await
}

/// GET /api/security/events/count-by
#[get("/security/events/count-by")]
pub async fn security_events_count_by(
    data: web::Data<AppState>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    proxy_security_query(data, "sec.events.count_by", query_to_params(&query)).await
}

/// GET /api/security/events
#[get("/security/events")]
pub async fn security_events_list(
    data: web::Data<AppState>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    proxy_security_query(data, "sec.events.list", query_to_params(&query)).await
}

/// GET /api/security/events/{event_id}
#[get("/security/events/{event_id}")]
pub async fn security_event_detail(
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    let params = query_to_params(&query).map(|mut params| {
        params["event_id"] = Value::String(path.into_inner());
        params
    });
    proxy_security_query(data, "sec.events.get", params).await
}

/// GET /api/security/observability/sessions
#[get("/security/observability/sessions")]
pub async fn security_observability_sessions(
    data: web::Data<AppState>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    proxy_security_query(data, "obs.sessions.list", query_to_params(&query)).await
}

/// GET /api/security/observability/sessions/{session_id}/runs
#[get("/security/observability/sessions/{session_id}/runs")]
pub async fn security_observability_runs(
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    let params = query_to_params(&query).map(|mut params| {
        params["session_id"] = Value::String(path.into_inner());
        params
    });
    proxy_security_query(data, "obs.runs.list", params).await
}

/// GET /api/security/observability/timeline
#[get("/security/observability/timeline")]
pub async fn security_observability_timeline(
    data: web::Data<AppState>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    proxy_security_query(data, "obs.timeline.get", query_to_params(&query)).await
}

async fn proxy_security_query(
    data: web::Data<AppState>,
    method: &'static str,
    params: Result<Value, HttpResponse>,
) -> HttpResponse {
    let params = match params {
        Ok(params) => params,
        Err(response) => return response,
    };

    let client = match agent_sec_client(&data) {
        Ok(client) => client,
        Err(err) => return client_error_response(err),
    };

    match call_daemon(client, method, params).await {
        Ok(response) if response.ok => {
            let state = derive_security_query_state(method, &response.data);
            security_state_response(StatusCode::OK, state, response.data, None)
        }
        Ok(response) => daemon_error_response(response),
        Err(err) => client_error_response(err),
    }
}

async fn call_daemon(
    client: AgentSecClient,
    method: &'static str,
    params: Value,
) -> Result<DaemonResponse, AgentSecClientError> {
    let method = method.to_string();
    match web::block(move || client.call(&method, params)).await {
        Ok(result) => result,
        Err(err) => Err(AgentSecClientError::Transport(format!(
            "daemon client task failed: {err}"
        ))),
    }
}

fn agent_sec_client(data: &web::Data<AppState>) -> Result<AgentSecClient, AgentSecClientError> {
    AgentSecClient::with_timeout(None, data.security_observability.timeout_ms)
}

fn client_socket_path(data: &web::Data<AppState>) -> Option<String> {
    agent_sec_client(data)
        .ok()
        .map(|client| client.socket_path().display().to_string())
}

fn query_to_params(query: &web::Query<HashMap<String, String>>) -> Result<Value, HttpResponse> {
    let mut params = serde_json::Map::new();
    for (key, raw_value) in query.iter() {
        let value = parse_security_query_value(key, raw_value)?;
        params.insert(key.clone(), value);
    }
    Ok(Value::Object(params))
}

fn parse_security_query_value(key: &str, raw_value: &str) -> Result<Value, HttpResponse> {
    match key {
        "start_ns" | "end_ns" | "limit" | "offset" | "latest_limit" => {
            let value = raw_value
                .parse::<i64>()
                .map_err(|_| bad_request_response(format!("{key} must be an integer")))?;
            Ok(Value::Number(value.into()))
        }
        "include_details" | "include_security" => parse_bool(raw_value)
            .map(Value::Bool)
            .ok_or_else(|| bad_request_response(format!("{key} must be a boolean"))),
        _ => Ok(Value::String(raw_value.to_string())),
    }
}

fn parse_bool(raw_value: &str) -> Option<bool> {
    match raw_value {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn derive_security_query_state(method: &str, data: &Value) -> &'static str {
    match method {
        "sec.summary" if data.get("total").and_then(Value::as_i64).unwrap_or(0) == 0 => "empty",
        "sec.events.list" | "obs.sessions.list" | "obs.runs.list"
            if data.get("total").and_then(Value::as_i64).unwrap_or(0) == 0 =>
        {
            "empty"
        }
        "sec.events.count_by"
            if data
                .get("items")
                .and_then(Value::as_array)
                .map(|items| items.is_empty())
                .unwrap_or(true) =>
        {
            "empty"
        }
        "sec.events.get" if !data.get("found").and_then(Value::as_bool).unwrap_or(false) => {
            "not_found"
        }
        "sec.events.get" => "found",
        "obs.timeline.get"
            if data
                .get("items")
                .and_then(Value::as_array)
                .map(|items| items.is_empty())
                .unwrap_or(true) =>
        {
            "empty"
        }
        _ => "ok",
    }
}

fn security_state_response(
    status: StatusCode,
    state: &str,
    data: Value,
    message: Option<&str>,
) -> HttpResponse {
    let mut body = json!({
        "state": state,
        "data": data,
        "meta": {
            "source": "agent-sec-daemon",
        },
    });
    if let Some(message) = message {
        body["message"] = Value::String(message.to_string());
    }
    HttpResponse::build(status).json(body)
}

fn bad_request_response(message: String) -> HttpResponse {
    HttpResponse::BadRequest().json(json!({
        "error": {
            "code": "bad_request",
            "message": message,
            "retryable": false,
        }
    }))
}

fn client_error_response(err: AgentSecClientError) -> HttpResponse {
    let status = client_error_status(&err);
    let (code, retryable) = match &err {
        AgentSecClientError::SocketPath(_) | AgentSecClientError::Transport(_) => {
            ("daemon_unavailable", true)
        }
        AgentSecClientError::Timeout(_) => ("daemon_timeout", true),
        AgentSecClientError::ResponseTooLarge(_) => ("payload_too_large", false),
        AgentSecClientError::Protocol(_) => ("daemon_protocol_mismatch", false),
    };

    HttpResponse::build(status).json(json!({
        "error": {
            "code": code,
            "message": err.to_string(),
            "retryable": retryable,
        }
    }))
}

fn client_error_status(err: &AgentSecClientError) -> StatusCode {
    match err {
        AgentSecClientError::SocketPath(_) | AgentSecClientError::Transport(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        AgentSecClientError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
        AgentSecClientError::ResponseTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        AgentSecClientError::Protocol(_) => StatusCode::BAD_GATEWAY,
    }
}

fn daemon_error_response(response: DaemonResponse) -> HttpResponse {
    let daemon_error = response.error.clone();
    let daemon_code = daemon_error
        .as_ref()
        .map(|error| error.code.as_str())
        .unwrap_or("internal_error");
    let message = daemon_error
        .as_ref()
        .map(|error| error.message.clone())
        .unwrap_or_else(|| response.stderr.clone());

    let (status, code, retryable) = match daemon_code {
        "bad_request" => (StatusCode::BAD_REQUEST, "bad_request", false),
        "unknown_method" => (StatusCode::BAD_GATEWAY, "daemon_protocol_mismatch", false),
        "payload_too_large" => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", false),
        "timeout" => (StatusCode::GATEWAY_TIMEOUT, "daemon_timeout", true),
        "busy" => (StatusCode::SERVICE_UNAVAILABLE, "daemon_busy", true),
        "unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "daemon_capability_unavailable",
            true,
        ),
        "shutdown" => (StatusCode::SERVICE_UNAVAILABLE, "daemon_shutdown", true),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "daemon_internal_error",
            false,
        ),
    };

    HttpResponse::build(status).json(json!({
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
            "daemon_code": daemon_code,
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use std::time::Instant;

    use actix_web::App;
    use actix_web::body::to_bytes;
    use actix_web::test as awtest;

    use crate::agent_sec::DaemonErrorPayload;
    use crate::genai::GenAIExporter;
    use crate::genai::semantic::{
        GenAISemanticEvent, LLMCall, LLMRequest, LLMResponse, MessagePart, OutputMessage,
        TokenUsage,
    };
    use crate::grader::EvaluationStore;
    use crate::health::HealthStore;
    use crate::storage::sqlite::genai::{AgentActivitySummary, PendingCallInfo, PendingOrigin};
    use agentsight_trajectory_collector::{
        TrajectoryAgentActivitySummary, TrajectoryRecord, TrajectoryStore,
    };

    use super::*;

    #[test]
    fn query_to_params_parses_security_query_types() {
        let query = web::Query(HashMap::from([
            ("start_ns".to_string(), "100".to_string()),
            ("limit".to_string(), "25".to_string()),
            ("include_details".to_string(), "true".to_string()),
            ("agent_name".to_string(), "codex".to_string()),
        ]));

        let params = query_to_params(&query).expect("valid query should parse");

        assert_eq!(
            params,
            json!({
                "start_ns": 100,
                "limit": 25,
                "include_details": true,
                "agent_name": "codex",
            })
        );
    }

    #[actix_web::test]
    async fn query_to_params_rejects_invalid_security_query_types() {
        let query = web::Query(HashMap::from([(
            "include_security".to_string(),
            "sometimes".to_string(),
        )]));

        let response = query_to_params(&query).expect_err("invalid boolean should fail");
        let body = response_json(response).await;

        assert_eq!(body["error"]["code"], "bad_request");
        assert_eq!(body["error"]["retryable"], false);
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("include_security"))
        );
    }

    #[test]
    fn derive_security_query_state_maps_empty_and_found_states() {
        assert_eq!(
            derive_security_query_state("sec.summary", &json!({})),
            "empty"
        );
        assert_eq!(
            derive_security_query_state("sec.events.list", &json!({ "total": 0 })),
            "empty"
        );
        assert_eq!(
            derive_security_query_state("sec.events.get", &json!({ "found": false })),
            "not_found"
        );
        assert_eq!(
            derive_security_query_state("sec.events.get", &json!({ "found": true })),
            "found"
        );
        assert_eq!(
            derive_security_query_state("obs.timeline.get", &json!({ "items": [] })),
            "empty"
        );
        assert_eq!(
            derive_security_query_state("obs.timeline.get", &json!({ "items": [{}] })),
            "ok"
        );
    }

    #[actix_web::test]
    async fn daemon_error_response_maps_daemon_codes_to_http_errors() {
        for (daemon_code, status, code, retryable) in [
            ("bad_request", StatusCode::BAD_REQUEST, "bad_request", false),
            (
                "unknown_method",
                StatusCode::BAD_GATEWAY,
                "daemon_protocol_mismatch",
                false,
            ),
            (
                "payload_too_large",
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                false,
            ),
            (
                "timeout",
                StatusCode::GATEWAY_TIMEOUT,
                "daemon_timeout",
                true,
            ),
            ("busy", StatusCode::SERVICE_UNAVAILABLE, "daemon_busy", true),
            (
                "unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon_capability_unavailable",
                true,
            ),
            (
                "shutdown",
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon_shutdown",
                true,
            ),
            (
                "internal_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                "daemon_internal_error",
                false,
            ),
        ] {
            let response = daemon_error_response(daemon_response_with_error(daemon_code));
            assert_eq!(response.status(), status);

            let body = response_json(response).await;
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["daemon_code"], daemon_code);
            assert_eq!(body["error"]["retryable"], retryable);
        }
    }

    #[actix_web::test]
    async fn client_error_response_maps_protocol_errors_to_bad_gateway() {
        for (err, status, code, retryable) in [
            (
                AgentSecClientError::SocketPath("missing runtime dir".to_string()),
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon_unavailable",
                true,
            ),
            (
                AgentSecClientError::Transport("connect refused".to_string()),
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon_unavailable",
                true,
            ),
            (
                AgentSecClientError::Timeout("read response".to_string()),
                StatusCode::GATEWAY_TIMEOUT,
                "daemon_timeout",
                true,
            ),
            (
                AgentSecClientError::ResponseTooLarge(128),
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                false,
            ),
            (
                AgentSecClientError::Protocol("unexpected response".to_string()),
                StatusCode::BAD_GATEWAY,
                "daemon_protocol_mismatch",
                false,
            ),
        ] {
            let response = client_error_response(err);
            assert_eq!(response.status(), status);

            let body = response_json(response).await;
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["retryable"], retryable);
        }
    }

    #[actix_web::test]
    async fn security_endpoints_report_client_errors_when_daemon_config_is_invalid() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .service(security_status)
                .service(security_summary)
                .service(security_events_count_by)
                .service(security_events_list)
                .service(security_event_detail)
                .service(security_observability_sessions)
                .service(security_observability_runs)
                .service(security_observability_timeline),
        )
        .await;

        for (uri, status) in [
            ("/security/status", StatusCode::SERVICE_UNAVAILABLE),
            ("/security/summary?limit=1", StatusCode::BAD_GATEWAY),
            (
                "/security/events/count-by?include_security=true",
                StatusCode::BAD_GATEWAY,
            ),
            ("/security/events?offset=1", StatusCode::BAD_GATEWAY),
            ("/security/events/event-1", StatusCode::BAD_GATEWAY),
            (
                "/security/observability/sessions?latest_limit=1",
                StatusCode::BAD_GATEWAY,
            ),
            (
                "/security/observability/sessions/session-1/runs",
                StatusCode::BAD_GATEWAY,
            ),
            (
                "/security/observability/timeline?end_ns=2",
                StatusCode::BAD_GATEWAY,
            ),
        ] {
            let response =
                awtest::call_service(&app, awtest::TestRequest::get().uri(uri).to_request()).await;

            assert_eq!(response.status(), status);
        }
    }

    #[actix_web::test]
    async fn latest_grader_uses_shared_evaluation_store() {
        let root = temp_root("latest_grader_shared_store");
        let evaluation_path = root.join("evaluation.db");
        let evaluation_store = Arc::new(EvaluationStore::new_with_path(&evaluation_path).unwrap());
        let result = test_evaluation_result("conv-shared");
        evaluation_store.insert_completed(&result).unwrap();

        let blocked_parent = root.join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").unwrap();
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        let data = web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: blocked_parent.join("genai.db"),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::clone(&evaluation_store),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        });
        let app = awtest::init_service(App::new().app_data(data).service(latest_grader)).await;

        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/grader/latest?target_type=conversation&target_id=conv-shared")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["run_id"], "run-shared");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[actix_web::test]
    async fn evaluate_grader_reuses_existing_run_for_same_snapshot() {
        let root = temp_root("evaluate_grader_reuses_run");
        let db_path = root.join("events.db");
        write_completed_conversation_event(&db_path, "conv-reuse");
        let data = grader_app_state(db_path.clone());
        let app = awtest::init_service(App::new().app_data(data).service(evaluate_grader)).await;
        let request = EvaluationRequest {
            target_type: "conversation".to_string(),
            target_id: "conv-reuse".to_string(),
            force: false,
        };

        let first = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri("/grader/evaluate")
                .set_json(&request)
                .to_request(),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = service_response_json(first).await;
        assert_eq!(first_body["reused_existing_run"], false);
        let run_id = first_body["result"]["run_id"]
            .as_str()
            .expect("run_id should be present")
            .to_string();

        let second = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri("/grader/evaluate")
                .set_json(&request)
                .to_request(),
        )
        .await;
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = service_response_json(second).await;
        assert_eq!(second_body["reused_existing_run"], true);
        assert_eq!(second_body["result"]["run_id"], run_id);

        cleanup_db(&db_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[actix_web::test]
    async fn evaluate_grader_maps_bad_request_not_found_and_pending() {
        let root = temp_root("evaluate_grader_errors");
        let db_path = root.join("events.db");
        write_pending_conversation_event(&db_path, "conv-pending");
        let data = grader_app_state(db_path.clone());
        let app = awtest::init_service(App::new().app_data(data).service(evaluate_grader)).await;

        let bad_request = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri("/grader/evaluate")
                .set_json(&EvaluationRequest {
                    target_type: "trace".to_string(),
                    target_id: "conv-pending".to_string(),
                    force: false,
                })
                .to_request(),
        )
        .await;
        assert_eq!(bad_request.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            service_response_json(bad_request).await["error"],
            "unsupported_target"
        );

        let not_found = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri("/grader/evaluate")
                .set_json(&EvaluationRequest {
                    target_type: "conversation".to_string(),
                    target_id: "missing-conv".to_string(),
                    force: false,
                })
                .to_request(),
        )
        .await;
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            service_response_json(not_found).await["error"],
            "conversation_not_found"
        );

        let pending = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri("/grader/evaluate")
                .set_json(&EvaluationRequest {
                    target_type: "conversation".to_string(),
                    target_id: "conv-pending".to_string(),
                    force: false,
                })
                .to_request(),
        )
        .await;
        assert_eq!(pending.status(), StatusCode::CONFLICT);
        let pending_body = service_response_json(pending).await;
        assert_eq!(pending_body["error"], "conversation_not_ready");
        assert_eq!(pending_body["pending_call_count"], 1);

        cleanup_db(&db_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    async fn response_json(response: HttpResponse) -> Value {
        let body = to_bytes(response.into_body())
            .await
            .expect("response body should be readable");
        serde_json::from_slice(&body).expect("response body should be JSON")
    }

    async fn service_response_json(response: actix_web::dev::ServiceResponse) -> Value {
        let body = to_bytes(response.into_body())
            .await
            .expect("response body should be readable");
        serde_json::from_slice(&body).expect("response body should be JSON")
    }

    fn daemon_response_with_error(code: &str) -> DaemonResponse {
        DaemonResponse {
            request_id: "req-1".to_string(),
            ok: false,
            data: Value::Null,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 1,
            error: Some(DaemonErrorPayload {
                code: code.to_string(),
                message: format!("{code} message"),
            }),
        }
    }

    fn test_evaluation_result(target_id: &str) -> crate::grader::EvaluationResult {
        crate::grader::EvaluationResult {
            target_type: TargetType::Conversation,
            target_id: target_id.to_string(),
            run_id: "run-shared".to_string(),
            input_hash: "input-hash-shared".to_string(),
            verdict: crate::grader::Verdict::Pass,
            score: 1.0,
            summary: "ok".to_string(),
            root_cause: crate::grader::RootCause::None,
            recommended_action: "none".to_string(),
            dimensions: Vec::new(),
            findings: Vec::new(),
            metadata: crate::grader::EvaluationMetadata {
                evaluated_with_pending: false,
                pending_call_count: 0,
                input_event_count: 1,
                grader_type: GraderType::Rule,
                grader_version: RULE_GRADER_VERSION.to_string(),
                rubric_version: None,
                judge_model: None,
                prompt_hash: None,
                confidence: Some(1.0),
            },
        }
    }

    fn grader_app_state(storage_path: PathBuf) -> web::Data<AppState> {
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            evaluation_store: Arc::new(EvaluationStore::new_with_path(&storage_path).unwrap()),
            storage_path,
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    fn write_completed_conversation_event(path: &std::path::Path, conversation_id: &str) {
        let store =
            GenAISqliteStore::new_with_path(path, crate::config::InsertStoragePolicy::default())
                .unwrap();
        let mut call = LLMCall::new(
            format!("call-{conversation_id}"),
            1_700_000_000_000_000_000,
            "anthropic".to_string(),
            "claude".to_string(),
            LLMRequest {
                messages: Vec::new(),
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: None,
                raw_body: None,
            },
            1234,
            "claude".to_string(),
        );
        call.agent_name = Some("claude".to_string());
        call.set_response(
            LLMResponse {
                messages: vec![OutputMessage {
                    role: "assistant".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "done".to_string(),
                    }],
                    name: None,
                    finish_reason: Some("stop".to_string()),
                }],
                streamed: false,
                raw_body: None,
            },
            1_700_000_000_000_000_500,
        );
        call.set_token_usage(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.metadata
            .insert("conversation_id".to_string(), conversation_id.to_string());
        call.metadata.insert(
            "response_id".to_string(),
            format!("trace-{conversation_id}"),
        );
        call.metadata.insert(
            "session_id".to_string(),
            format!("session-{conversation_id}"),
        );
        call.metadata
            .insert("user_query".to_string(), "hello".to_string());

        store.export(&[GenAISemanticEvent::LLMCall(call)]);
        store.flush();
    }

    fn write_pending_conversation_event(path: &std::path::Path, conversation_id: &str) {
        let store =
            GenAISqliteStore::new_with_path(path, crate::config::InsertStoragePolicy::default())
                .unwrap();
        store
            .insert_pending(&PendingCallInfo {
                call_id: format!("pending-{conversation_id}"),
                trace_id: Some(format!("trace-{conversation_id}")),
                conversation_id: Some(conversation_id.to_string()),
                session_id: Some("session-1".to_string()),
                start_timestamp_ns: 1_700_000_000_000_000_000,
                pid: 1234,
                process_name: "claude".to_string(),
                agent_name: Some("claude".to_string()),
                http_method: Some("POST".to_string()),
                http_path: Some("/v1/messages".to_string()),
                input_messages: Some(r#"[{"role":"user","content":"hello"}]"#.to_string()),
                system_instructions: None,
                user_query: Some("hello".to_string()),
                is_sse: true,
                model: Some("claude".to_string()),
                provider: Some("anthropic".to_string()),
                call_kind: "main".to_string(),
                pending_origin: PendingOrigin::RequestCapture,
                pending_match_key: None,
            })
            .unwrap();
        store.flush();
    }

    fn cleanup_db(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentsight_{label}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_app_state(timeout_ms: u64) -> web::Data<AppState> {
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: PathBuf::from(":memory:"),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(std::path::Path::new(":memory:")).unwrap(),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    #[actix_web::test]
    async fn api_unmatched_returns_404_json() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .configure(super::super::configure_routes),
        )
        .await;

        let req = awtest::TestRequest::get()
            .uri("/api/definitely-not-a-route")
            .to_request();
        let resp = awtest::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], "not_found");
    }

    #[actix_web::test]
    async fn health_unmatched_returns_404_json() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .configure(super::super::configure_routes),
        )
        .await;

        let req = awtest::TestRequest::get().uri("/health/nope").to_request();
        let resp = awtest::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], "not_found");
    }

    #[actix_web::test]
    async fn non_api_path_uses_spa_fallback_not_api_404() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .configure(super::super::configure_routes),
        )
        .await;

        let req = awtest::TestRequest::get().uri("/dashboard").to_request();
        let resp = awtest::call_service(&app, req).await;

        // Should NOT be 404 JSON — it should go to SPA fallback (200 or 404 plain)
        // Definitely should not be a JSON 404 with error.code = "not_found"
        let status = resp.status();
        let body_bytes = actix_web::body::to_bytes(resp.into_body()).await.unwrap();
        let is_api_404 = serde_json::from_slice::<Value>(&body_bytes)
            .ok()
            .and_then(|v| v.get("error")?.get("code")?.as_str().map(String::from))
            == Some("not_found".to_string());
        assert!(
            !is_api_404,
            "SPA path /dashboard must not get API 404, got status={status}"
        );
    }

    // ─── Auth endpoint tests ──────────────────────────────────────────────────

    /// A test token that is >= 32 chars (required by read_or_create_token).
    const TEST_TOKEN: &str = "correct-token-for-auth-testing-32chars!!";

    fn test_app_state_with_auth(enabled: bool) -> web::Data<AppState> {
        // Use a unique dir per call to avoid test-parallelism races
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("handler_auth_test_{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        if enabled {
            std::fs::write(dir.join(".dashboard_token"), TEST_TOKEN).ok();
        }
        let auth_config = crate::config::ServerAuthConfig { enabled };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(&auth_config, &dir));
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: PathBuf::from(":memory:"),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(std::path::Path::new(":memory:")).unwrap(),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    #[actix_web::test]
    async fn auth_login_success_with_correct_token() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_auth(true))
                .route("/api/auth/login", web::post().to(auth_login)),
        )
        .await;
        let req = awtest::TestRequest::post()
            .uri("/api/auth/login")
            .set_json(json!({"token": TEST_TOKEN}))
            .to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        // Should have a session cookie set
        let cookie = resp
            .response()
            .cookies()
            .find(|c| c.name() == "agentsight_session");
        assert!(cookie.is_some(), "response should contain session cookie");
    }

    #[actix_web::test]
    async fn auth_login_fails_with_wrong_token() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_auth(true))
                .route("/api/auth/login", web::post().to(auth_login)),
        )
        .await;
        let req = awtest::TestRequest::post()
            .uri("/api/auth/login")
            .set_json(json!({"token": "wrong-token"}))
            .to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], "invalid_token");
    }

    #[actix_web::test]
    async fn auth_status_reports_enabled() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_auth(true))
                .service(auth_status),
        )
        .await;
        let req = awtest::TestRequest::get().uri("/status").to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["auth_enabled"], true);
        // Intrinsic capabilities must always be advertised; component-backed
        // ones (security/enforcement/token_savings) depend on the host.
        let caps = body["capabilities"].as_array().unwrap();
        for cap in [
            "agent_observability",
            "sessions",
            "agent_health",
            "settings",
        ] {
            assert!(caps.iter().any(|v| v == cap), "missing capability {cap}");
        }
    }

    #[actix_web::test]
    async fn auth_status_reports_disabled() {
        let app =
            awtest::init_service(App::new().app_data(test_app_state(0)).service(auth_status)).await;
        let req = awtest::TestRequest::get().uri("/status").to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["auth_enabled"], false);
    }

    #[actix_web::test]
    async fn auth_verify_returns_true_when_disabled() {
        let app =
            awtest::init_service(App::new().app_data(test_app_state(0)).service(auth_verify)).await;
        let req = awtest::TestRequest::get().uri("/verify").to_request();
        let resp = awtest::call_service(&app, req).await;
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["authenticated"], true);
    }

    #[actix_web::test]
    async fn auth_verify_returns_false_without_cookie() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_auth(true))
                .service(auth_verify),
        )
        .await;
        let req = awtest::TestRequest::get().uri("/verify").to_request();
        let resp = awtest::call_service(&app, req).await;
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["authenticated"], false);
    }

    #[actix_web::test]
    async fn auth_verify_returns_true_with_valid_cookie() {
        let state = test_app_state_with_auth(true);
        let cookie_value = state.auth.create_session_cookie(3600);
        let app = awtest::init_service(App::new().app_data(state).service(auth_verify)).await;
        let req = awtest::TestRequest::get()
            .uri("/verify")
            .cookie(actix_web::cookie::Cookie::new(
                "agentsight_session",
                cookie_value,
            ))
            .to_request();
        let resp = awtest::call_service(&app, req).await;
        let body: Value =
            serde_json::from_slice(&actix_web::body::to_bytes(resp.into_body()).await.unwrap())
                .unwrap();
        assert_eq!(body["authenticated"], true);
    }

    fn test_app_state_with_storage(storage_path: PathBuf) -> web::Data<AppState> {
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: storage_path.clone(),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::new(EvaluationStore::new_with_path(&storage_path).unwrap()),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    fn test_app_state_with_interruption_store(
        store: Arc<crate::storage::sqlite::InterruptionStore>,
    ) -> web::Data<AppState> {
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: PathBuf::from(":memory:"),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: Some(store),
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(std::path::Path::new(":memory:")).unwrap(),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    fn unique_handler_db(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentsight_handler_{label}_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    // ─── Trajectory handler tests ────────────────────────────────────────

    fn trajectory_record(session_id: &str, project: &str, source: &str) -> TrajectoryRecord {
        TrajectoryRecord {
            session_id: session_id.to_string(),
            schema_version: "ATIF-v1.7".to_string(),
            agent_name: "qoder".to_string(),
            model_name: Some("qwen-max".to_string()),
            num_steps: 2,
            total_prompt_tokens: Some(100),
            total_completion_tokens: Some(20),
            start_time: Some("2026-07-25T10:00:00Z".to_string()),
            end_time: Some("2026-07-25T10:00:05Z".to_string()),
            first_user_message: Some("修个 bug".to_string()),
            last_user_message: Some("跑下测试".to_string()),
            atif_json: format!(
                "{{\"schema_version\":\"ATIF-v1.7\",\"session_id\":\"{session_id}\"}}"
            ),
            project: project.to_string(),
            source: source.to_string(),
            is_subagent: false,
            file_path: format!("/root/.qoder/projects/{project}/{session_id}.jsonl"),
            file_size: 512,
            file_mtime_ns: 7,
        }
    }

    fn test_app_state_with_trajectory_store(
        store: Option<Arc<TrajectoryStore>>,
    ) -> web::Data<AppState> {
        test_app_state_with_activity_stores(PathBuf::from(":memory:"), store)
    }

    fn test_app_state_with_activity_stores(
        storage_path: PathBuf,
        trajectory_store: Option<Arc<TrajectoryStore>>,
    ) -> web::Data<AppState> {
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            evaluation_store: Arc::new(EvaluationStore::new_with_path(&storage_path).unwrap()),
            storage_path,
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: super::super::SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(trajectory_store)),
        })
    }

    fn seeded_trajectory_store(tag: &str) -> Arc<TrajectoryStore> {
        let db = unique_handler_db(tag);
        let store = TrajectoryStore::new_with_path(&db).unwrap();
        store
            .upsert_trajectory(&trajectory_record("s-1", "proj-a", "qoder"))
            .unwrap();
        store
            .upsert_trajectory(&trajectory_record("s-2", "proj-b", "qoderwork"))
            .unwrap();
        Arc::new(store)
    }

    #[actix_web::test]
    async fn trajectory_list_returns_rows_and_filters() {
        let data = test_app_state_with_trajectory_store(Some(seeded_trajectory_store("list")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let all_resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories")
                .to_request(),
        )
        .await;
        let all: serde_json::Value = awtest::read_body_json(all_resp).await;
        assert_eq!(all.as_array().unwrap().len(), 2);

        let filtered_resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories?project=proj-a")
                .to_request(),
        )
        .await;
        let filtered: serde_json::Value = awtest::read_body_json(filtered_resp).await;
        let arr = filtered.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["session_id"], "s-1");
    }

    #[actix_web::test]
    async fn trajectory_detail_returns_raw_atif_and_404() {
        let data = test_app_state_with_trajectory_store(Some(seeded_trajectory_store("detail")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/s-1")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body()).await.unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc["session_id"], "s-1");

        let miss = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/nope")
                .to_request(),
        )
        .await;
        assert_eq!(miss.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn trajectory_filters_route_not_captured_by_session_id() {
        let data = test_app_state_with_trajectory_store(Some(seeded_trajectory_store("filters")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/filters")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        // `filters` must hit the filters handler (object), not the detail handler (404).
        assert_eq!(body["projects"], serde_json::json!(["proj-a", "proj-b"]));
        assert_eq!(body["sources"], serde_json::json!(["qoder", "qoderwork"]));
    }

    #[actix_web::test]
    async fn trajectory_endpoints_graceful_when_store_absent() {
        let data = test_app_state_with_trajectory_store(None);
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let list = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories")
                .to_request(),
        )
        .await;
        assert_eq!(list.status(), StatusCode::OK);
        let rows: serde_json::Value = awtest::read_body_json(list).await;
        assert_eq!(rows, serde_json::json!([]));

        let filters = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/filters")
                .to_request(),
        )
        .await;
        assert_eq!(filters.status(), StatusCode::OK);
    }

    // ─── Trajectory step query tests ───────────────────────────────────

    /// Store holding one trajectory whose steps cover several categories.
    fn stepped_trajectory_store(tag: &str) -> Arc<TrajectoryStore> {
        let db = unique_handler_db(tag);
        let store = TrajectoryStore::new_with_path(&db).unwrap();
        let mut record = trajectory_record("s-steps", "proj-a", "qoder");
        record.num_steps = 4;
        record.atif_json = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "qoder", "version": "1.0"},
            "session_id": "s-steps",
            "steps": [
                {"step_id": 1, "source": "user", "message": "列一下文件"},
                {"step_id": 2, "source": "agent", "message": "好的",
                 "reasoning_content": "需要执行 ls",
                 "tool_calls": [{"tool_call_id": "t1", "function_name": "bash",
                                 "arguments": {"cmd": "ls"}}]},
                {"step_id": 3, "source": "user", "message": "",
                 "observation": {"results": [{"source_call_id": "t1", "content": "a.txt"}]}},
                {"step_id": 4, "source": "agent", "message": "只有 a.txt"}
            ]
        })
        .to_string();
        store.upsert_trajectory(&record).unwrap();
        Arc::new(store)
    }

    #[actix_web::test]
    async fn trajectory_steps_route_not_captured_by_session_id() {
        let data =
            test_app_state_with_trajectory_store(Some(stepped_trajectory_store("steps-route")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        // `steps` must hit the step handler (object with `hits`), not the detail
        // handler, which would 404 on a session named "steps".
        assert!(body["hits"].is_array());
        assert_eq!(body["scanned_trajectories"], 1);
    }

    #[actix_web::test]
    async fn trajectory_steps_filters_by_category_with_context() {
        let data =
            test_app_state_with_trajectory_store(Some(stepped_trajectory_store("steps-cat")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps?category=tool_call&context=1")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["step"]["step_id"], 2);
        assert_eq!(hits[0]["session_id"], "s-steps");
        assert_eq!(hits[0]["agent_name"], "qoder");
        assert_eq!(hits[0]["step"]["tool_names"], serde_json::json!(["bash"]));
        // context=1 yields exactly one neighbour on each side.
        assert_eq!(hits[0]["context"]["before"][0]["step_id"], 1);
        assert_eq!(hits[0]["context"]["after"][0]["step_id"], 3);
        assert_eq!(hits[0]["context"]["before"].as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["context"]["after"].as_array().unwrap().len(), 1);
    }

    #[actix_web::test]
    async fn trajectory_steps_multi_category_is_or() {
        let data = test_app_state_with_trajectory_store(Some(stepped_trajectory_store("steps-or")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps?category=thinking,tool_result")
                .to_request(),
        )
        .await;
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        let ids: Vec<i64> = body["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["step"]["step_id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[actix_web::test]
    async fn trajectory_steps_rejects_unknown_category() {
        let data =
            test_app_state_with_trajectory_store(Some(stepped_trajectory_store("steps-bad")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps?category=assistant")
                .to_request(),
        )
        .await;
        // Rejecting beats silently ignoring: an ignored filter would return
        // unrelated steps that look like a legitimate result.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        assert_eq!(body["error"], "invalid_category");
        let valid = body["valid_categories"].as_array().unwrap();
        assert_eq!(valid.len(), 6);
        assert!(valid.contains(&serde_json::json!("tool_call")));
    }

    #[actix_web::test]
    async fn trajectory_steps_graceful_when_store_absent() {
        let data = test_app_state_with_trajectory_store(None);
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        assert_eq!(body["hits"], serde_json::json!([]));
        assert_eq!(body["scanned_trajectories"], 0);
        assert_eq!(body["truncated"], false);
    }

    #[actix_web::test]
    async fn trajectory_steps_caps_limit_and_marks_truncated() {
        let data =
            test_app_state_with_trajectory_store(Some(stepped_trajectory_store("steps-lim")));
        let app = awtest::init_service(
            App::new()
                .app_data(data)
                .configure(crate::server::configure_routes),
        )
        .await;

        let resp = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps?limit=1")
                .to_request(),
        )
        .await;
        let body: serde_json::Value = awtest::read_body_json(resp).await;
        assert_eq!(body["hits"].as_array().unwrap().len(), 1);
        assert_eq!(body["truncated"], true);

        // A non-positive limit falls back to the default rather than acting as
        // "no limit" (SQLite treats a negative LIMIT as unbounded).
        let zero = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/trajectories/steps?limit=0")
                .to_request(),
        )
        .await;
        let zero_body: serde_json::Value = awtest::read_body_json(zero).await;
        assert_eq!(zero_body["hits"].as_array().unwrap().len(), 4);
    }

    fn make_interruption_event(
        id: &str,
        session_id: &str,
        conversation_id: &str,
        itype: crate::interruption::InterruptionType,
    ) -> crate::interruption::InterruptionEvent {
        crate::interruption::InterruptionEvent {
            interruption_id: id.to_string(),
            session_id: Some(session_id.to_string()),
            trace_id: Some(format!("trace-{conversation_id}")),
            conversation_id: Some(conversation_id.to_string()),
            call_id: Some(format!("call-{id}")),
            pid: Some(1234),
            agent_name: Some("Agent-A".to_string()),
            interruption_type: itype,
            severity: crate::interruption::types::Severity::High,
            occurred_at_ns: 1_700_000_000_000_000_000,
            detail: Some(r#"{"error":"rate limit"}"#.to_string()),
            resolved: false,
        }
    }

    #[actix_web::test]
    async fn health_reports_status_version_and_uptime() {
        let app =
            awtest::init_service(App::new().app_data(test_app_state(0)).service(health)).await;
        let response =
            awtest::call_service(&app, awtest::TestRequest::get().uri("/health").to_request())
                .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = service_response_json(response).await;
        assert_eq!(body["status"], "ok");
        assert!(body["version"].as_str().is_some());
        assert!(body["uptime_seconds"].as_u64().is_some());
    }

    #[actix_web::test]
    async fn genai_query_handlers_return_persisted_data() {
        let db_path = unique_handler_db("genai_queries");
        write_completed_conversation_event(&db_path, "conv-handler");
        GenAISqliteStore::new_with_path(&db_path, crate::config::InsertStoragePolicy::default())
            .unwrap()
            .insert_resource_samples(&[crate::storage::sqlite::ResourceSample {
                timestamp_ns: 1_700_000_000_000_000_250,
                pid: 1234,
                agent_name: Some("claude".to_string()),
                cpu_percent: 42.5,
                memory_bytes: 256 * 1024 * 1024,
            }])
            .unwrap();
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_storage(db_path.clone()))
                .service(list_sessions)
                .service(list_traces_by_session)
                .service(get_session_resources)
                .service(get_trace_detail)
                .service(get_conversation_events)
                .service(list_agent_names)
                .service(get_timeseries)
                .service(get_latency_metrics),
        )
        .await;

        let sessions = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/sessions?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        assert_eq!(sessions.status(), StatusCode::OK);
        let sessions_body = service_response_json(sessions).await;
        assert_eq!(sessions_body.as_array().unwrap().len(), 1);
        let session_id = sessions_body[0]["session_id"].as_str().unwrap();

        let traces = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(&format!(
                    "/sessions/{session_id}/traces?start_ns=0&end_ns=9223372036854775807"
                ))
                .to_request(),
        )
        .await;
        assert_eq!(traces.status(), StatusCode::OK);
        let traces_body = service_response_json(traces).await;
        assert_eq!(traces_body.as_array().unwrap().len(), 1);

        let resources = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(&format!("/sessions/{session_id}/resources"))
                .to_request(),
        )
        .await;
        assert_eq!(resources.status(), StatusCode::OK);
        let resources_body = service_response_json(resources).await;
        assert_eq!(resources_body["samples"].as_array().map(Vec::len), Some(1));
        assert_eq!(resources_body["samples"][0]["cpu_percent"], 42.5);
        assert_eq!(resources_body["phases"][0]["kind"], "llm");

        let invalid_resources = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(&format!("/sessions/{session_id}/resources?max_points=0"))
                .to_request(),
        )
        .await;
        assert_eq!(invalid_resources.status(), StatusCode::BAD_REQUEST);

        let missing_resources = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/sessions/missing/resources")
                .to_request(),
        )
        .await;
        assert_eq!(missing_resources.status(), StatusCode::NOT_FOUND);

        let detail = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/traces/trace-conv-handler")
                .to_request(),
        )
        .await;
        assert_eq!(detail.status(), StatusCode::OK);
        assert_eq!(
            service_response_json(detail)
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let conversation = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/conversations/conv-handler")
                .to_request(),
        )
        .await;
        assert_eq!(conversation.status(), StatusCode::OK);
        assert_eq!(
            service_response_json(conversation)
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let agent_names = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/agent-names?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        assert_eq!(agent_names.status(), StatusCode::OK);
        assert_eq!(service_response_json(agent_names).await[0], "claude");

        let timeseries = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/timeseries?start_ns=0&end_ns=9223372036854775807&buckets=2")
                .to_request(),
        )
        .await;
        assert_eq!(timeseries.status(), StatusCode::OK);
        let timeseries_body = service_response_json(timeseries).await;
        assert!(timeseries_body["token_series"].as_array().is_some());
        assert!(timeseries_body["model_series"].as_array().is_some());

        let latency = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/metrics/latency?start_ns=0&end_ns=9223372036854775807&agent_name=claude")
                .to_request(),
        )
        .await;
        assert_eq!(latency.status(), StatusCode::OK);
        let latency_body = service_response_json(latency).await;
        assert_eq!(latency_body.as_array().map(Vec::len), Some(1));
        assert_eq!(latency_body[0]["agent_name"], "claude");
        assert!(latency_body[0]["e2e_latency_ms"]["p50"].is_number());
        assert!(latency_body[0]["ttft_ms"].is_null());

        cleanup_db(&db_path);
    }

    #[actix_web::test]
    async fn latency_rejects_unrepresentable_default_start() {
        let db_path = unique_handler_db("latency_overflow");
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_storage(db_path.clone()))
                .service(get_latency_metrics),
        )
        .await;

        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/metrics/latency?end_ns=-9223372036854775808")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        cleanup_db(&db_path);
    }

    #[actix_web::test]
    async fn metrics_returns_prometheus_text_for_agent_tokens_and_interruptions() {
        let db_path = unique_handler_db("metrics");
        write_completed_conversation_event(&db_path, "conv-metrics");
        let interruption_path = unique_handler_db("metrics_interruptions");
        let istore = Arc::new(
            crate::storage::sqlite::InterruptionStore::new_with_path(&interruption_path).unwrap(),
        );
        istore
            .insert(&make_interruption_event(
                "int-metrics",
                "sess-metrics",
                "conv-metrics",
                crate::interruption::InterruptionType::RateLimit,
            ))
            .unwrap();
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        let app = awtest::init_service(
            App::new()
                .app_data(web::Data::new(AppState {
                    reuse_store: None,
                    reuse_llm_judge_enabled: false,
                    causal_store: None,
                    storage_path: db_path.clone(),
                    start_time: Instant::now(),
                    health_store: Arc::new(RwLock::new(HealthStore::new())),
                    interruption_store: Some(Arc::clone(&istore)),
                    evaluation_store: Arc::new(EvaluationStore::new_with_path(&db_path).unwrap()),
                    enforcement: None,
                    containment: None,
                    audit_service: Arc::new(agentsight_audit::AuditService::new(
                        crate::security::SecurityStore::open_in_memory()
                            .unwrap()
                            .audit_store(),
                    )),
                    security_observability: super::super::SecurityObservabilityConfig {
                        timeout_ms: 0,
                    },
                    auth,
                    optimize: None,
                    trajectory_store: Arc::new(RwLock::new(None)),
                }))
                .service(metrics),
        )
        .await;

        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/metrics").to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("agentsight_token_input_total"));
        assert!(text.contains("agent=\"claude\""));
        assert!(text.contains("agentsight_interruptions_total"));
        assert!(text.contains("type=\"rate_limit\""));

        cleanup_db(&db_path);
        cleanup_db(&interruption_path);
    }

    #[actix_web::test]
    async fn agent_health_process_records_can_still_be_deleted() {
        let state = test_app_state(0);
        {
            let mut store = state.health_store.write().unwrap();
            store.update(
                1001,
                crate::health::AgentHealthStatus {
                    pid: 1001,
                    agent_name: "Gateway".to_string(),
                    category: "agent".to_string(),
                    exe_path: "/bin/gateway".to_string(),
                    workspace_path: Some("/workspace/gateway".to_string()),
                    ports: vec![8080],
                    status: crate::health::store::AgentHealthState::Healthy,
                    last_check_time: 1,
                    latency_ms: Some(10),
                    error_message: None,
                    restart_cmd: None,
                    offline_since: None,
                    role: crate::health::store::AgentRole::Gateway,
                    parent_pid: None,
                    has_crash: false,
                },
            );
            store.update(
                1002,
                crate::health::AgentHealthStatus {
                    pid: 1002,
                    agent_name: "Client".to_string(),
                    category: "agent".to_string(),
                    exe_path: "/bin/client".to_string(),
                    workspace_path: Some("/workspace/client".to_string()),
                    ports: Vec::new(),
                    status: crate::health::store::AgentHealthState::Healthy,
                    last_check_time: 1,
                    latency_ms: None,
                    error_message: None,
                    restart_cmd: None,
                    offline_since: None,
                    role: crate::health::store::AgentRole::Client,
                    parent_pid: None,
                    has_crash: false,
                },
            );
            store.update(
                1003,
                crate::health::AgentHealthStatus {
                    pid: 1003,
                    agent_name: "Cosh".to_string(),
                    category: "agent".to_string(),
                    exe_path: "/bin/cosh".to_string(),
                    workspace_path: None,
                    ports: Vec::new(),
                    status: crate::health::store::AgentHealthState::Offline,
                    last_check_time: 1,
                    latency_ms: None,
                    error_message: Some("offline".to_string()),
                    restart_cmd: None,
                    offline_since: Some(1),
                    role: crate::health::store::AgentRole::Client,
                    parent_pid: None,
                    has_crash: true,
                },
            );
        }
        let app = awtest::init_service(
            App::new()
                .app_data(state)
                .service(get_agent_process_health)
                .route("/agent-health/{pid}", web::delete().to(delete_agent_health)),
        )
        .await;

        let filtered = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/agent-process-health")
                .to_request(),
        )
        .await;
        let filtered_body = service_response_json(filtered).await;
        let filtered_agents = filtered_body["agents"].as_array().unwrap();
        assert_eq!(filtered_agents.len(), 1);
        assert_eq!(filtered_agents[0]["pid"], 1001);
        assert_eq!(filtered_body["filtered_count"], 2);

        let include_clients = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/agent-process-health?include_clients=true")
                .to_request(),
        )
        .await;
        let include_body = service_response_json(include_clients).await;
        let agents = include_body["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 2, "Cosh should still be excluded");
        assert_eq!(include_body["filtered_count"], 1);

        let deleted = awtest::call_service(
            &app,
            awtest::TestRequest::delete()
                .uri("/agent-health/1001")
                .to_request(),
        )
        .await;
        assert_eq!(deleted.status(), StatusCode::OK);
        assert_eq!(service_response_json(deleted).await["ok"], true);

        let missing = awtest::call_service(
            &app,
            awtest::TestRequest::delete()
                .uri("/agent-health/9999")
                .to_request(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn agent_health_lists_activity_from_both_sqlite_stores() {
        let genai_path = unique_handler_db("agent-activity-genai");
        write_completed_conversation_event(&genai_path, "agent-health");
        let trajectory_store = seeded_trajectory_store("agent-activity-trajectory");
        let state = test_app_state_with_activity_stores(genai_path.clone(), Some(trajectory_store));
        let app = awtest::init_service(App::new().app_data(state).service(get_agent_health)).await;

        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/agent-health").to_request(),
        )
        .await;
        let body = service_response_json(response).await;
        let agents = body["agents"].as_array().unwrap();

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0]["agent_name"], "claude");
        assert_eq!(agents[0]["genai_calls"], 1);
        assert_eq!(agents[0]["genai_tokens"], 15);
        assert_eq!(agents[0]["trajectory_steps"], 0);
        assert_eq!(agents[0]["source"], "genai_events");
        assert_eq!(agents[1]["agent_name"], "qoder");
        assert_eq!(agents[1]["genai_calls"], 0);
        assert_eq!(agents[1]["trajectory_steps"], 4);
        assert_eq!(agents[1]["trajectory_tokens"], 240);
        assert_eq!(agents[1]["source"], "trajectories");

        cleanup_db(&genai_path);
    }

    #[actix_web::test]
    async fn agent_health_returns_empty_agents_for_empty_stores() {
        let genai_path = unique_handler_db("agent-activity-empty");
        let state = test_app_state_with_activity_stores(genai_path.clone(), None);
        let app = awtest::init_service(App::new().app_data(state).service(get_agent_health)).await;

        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/agent-health").to_request(),
        )
        .await;
        let body = service_response_json(response).await;

        assert_eq!(body["agents"], json!([]));
        cleanup_db(&genai_path);
    }

    #[test]
    fn agent_activity_merge_combines_source_totals() {
        let genai = vec![AgentActivitySummary {
            agent_name: "Claude".to_string(),
            last_seen_ns: 200,
            total_calls: 2,
            total_tokens: 50,
        }];
        let trajectories = vec![
            TrajectoryAgentActivitySummary {
                agent_name: "claude".to_string(),
                last_seen_ns: 300,
                total_steps: 8,
                total_tokens: 320,
            },
            TrajectoryAgentActivitySummary {
                agent_name: "Qoder".to_string(),
                last_seen_ns: 250,
                total_steps: 5,
                total_tokens: 120,
            },
        ];

        let merged = merge_agent_activity_summaries(genai, trajectories);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].agent_name, "Claude");
        assert_eq!(merged[0].last_seen_ns, 300);
        assert_eq!(merged[0].genai_calls, 2);
        assert_eq!(merged[0].genai_tokens, 50);
        assert_eq!(merged[0].trajectory_steps, 8);
        assert_eq!(merged[0].trajectory_tokens, 320);
        assert_eq!(merged[0].source, "genai_events+trajectories");
        assert_eq!(merged[1].agent_name, "Qoder");
        assert_eq!(merged[1].source, "trajectories");
    }

    #[actix_web::test]
    async fn interruption_handlers_cover_list_stats_counts_detail_and_resolve() {
        let interruption_path = unique_handler_db("interruptions");
        let istore = Arc::new(
            crate::storage::sqlite::InterruptionStore::new_with_path(&interruption_path).unwrap(),
        );
        istore
            .insert(&make_interruption_event(
                "int-handler-1",
                "sess-handler",
                "conv-handler-i",
                crate::interruption::InterruptionType::RateLimit,
            ))
            .unwrap();
        istore
            .insert(&make_interruption_event(
                "int-handler-2",
                "sess-handler",
                "conv-handler-i",
                crate::interruption::InterruptionType::AuthError,
            ))
            .unwrap();
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_interruption_store(Arc::clone(&istore)))
                .service(list_interruptions)
                .service(interruption_count)
                .service(interruption_stats)
                .service(interruption_session_counts)
                .service(interruption_conversation_counts)
                .service(list_session_interruptions)
                .service(list_conversation_interruptions)
                .service(get_interruption)
                .route(
                    "/interruptions/{interruption_id}/resolve",
                    web::post().to(resolve_interruption),
                ),
        )
        .await;

        let list = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        assert_eq!(list.status(), StatusCode::OK);
        assert_eq!(
            service_response_json(list).await.as_array().unwrap().len(),
            2
        );

        let count = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions/count?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        let count_body = service_response_json(count).await;
        assert_eq!(count_body["total"], 2);
        assert_eq!(count_body["by_severity"]["high"], 2);

        let stats = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions/stats?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        assert_eq!(
            service_response_json(stats).await.as_array().unwrap().len(),
            2
        );

        let session_counts = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions/session-counts?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        assert_eq!(service_response_json(session_counts).await[0]["total"], 2);

        let conversation_counts = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions/conversation-counts?start_ns=0&end_ns=9223372036854775807")
                .to_request(),
        )
        .await;
        let conversation_row = service_response_json(conversation_counts).await;
        assert_eq!(conversation_row[0]["total"], 2);
        // The owning session must ship with the row; the dashboard keys the
        // breakdown on (session_id, conversation_id).
        assert_eq!(conversation_row[0]["session_id"], "sess-handler");
        assert_eq!(conversation_row[0]["conversation_id"], "conv-handler-i");

        let by_session = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/sessions/sess-handler/interruptions")
                .to_request(),
        )
        .await;
        assert_eq!(
            service_response_json(by_session)
                .await
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let by_conversation = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/conversations/conv-handler-i/interruptions")
                .to_request(),
        )
        .await;
        assert_eq!(
            service_response_json(by_conversation)
                .await
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let detail = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions/int-handler-1")
                .to_request(),
        )
        .await;
        assert_eq!(
            service_response_json(detail).await["interruption_id"],
            "int-handler-1"
        );

        let resolved = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri("/interruptions/int-handler-1/resolve")
                .to_request(),
        )
        .await;
        assert_eq!(resolved.status(), StatusCode::OK);
        assert_eq!(service_response_json(resolved).await["status"], "resolved");

        let missing = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/interruptions/missing-id")
                .to_request(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        cleanup_db(&interruption_path);
    }

    #[actix_web::test]
    async fn interruption_handlers_return_unavailable_without_store() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .service(list_interruptions)
                .service(interruption_count)
                .service(interruption_stats),
        )
        .await;

        for uri in [
            "/interruptions",
            "/interruptions/count",
            "/interruptions/stats",
        ] {
            let response =
                awtest::call_service(&app, awtest::TestRequest::get().uri(uri).to_request()).await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[actix_web::test]
    async fn atif_export_handlers_return_documents_and_not_found() {
        let db_path = unique_handler_db("atif_exports");
        write_completed_conversation_event(&db_path, "conv-atif");
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_storage(db_path.clone()))
                .service(export_atif_trace)
                .service(export_atif_session)
                .service(export_atif_conversation),
        )
        .await;

        for uri in [
            "/export/atif/trace/trace-conv-atif",
            "/export/atif/session/session-conv-atif",
            "/export/atif/conversation/conv-atif",
        ] {
            let response =
                awtest::call_service(&app, awtest::TestRequest::get().uri(uri).to_request()).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{uri} should export ATIF"
            );
            let body = service_response_json(response).await;
            assert!(body.is_object(), "{uri} should return a JSON document");
            // The export must speak the shared ATIF schema, same as the
            // collector-ingested trajectories served by /api/trajectories.
            assert_eq!(
                body["schema_version"],
                agentsight_atif::ATIF_SCHEMA_VERSION,
                "{uri} should export the shared ATIF schema version"
            );
            assert!(
                body["session_id"].is_string(),
                "{uri} should carry the queried id as session_id"
            );
            assert!(
                body["steps"].as_array().is_some_and(|s| !s.is_empty()),
                "{uri} should export at least one step"
            );
            // Round-trip through the shared validator: catches field drift and
            // non-contiguous step_ids that a plain is_object() check would miss.
            agentsight_atif::validate_trajectory_str(&body.to_string())
                .unwrap_or_else(|e| panic!("{uri} export failed ATIF validation: {e}"));
        }

        for (uri, message) in [
            ("/export/atif/trace/missing-trace", "trace not found"),
            ("/export/atif/session/missing-session", "session not found"),
            (
                "/export/atif/conversation/missing-conversation",
                "conversation not found",
            ),
        ] {
            let response =
                awtest::call_service(&app, awtest::TestRequest::get().uri(uri).to_request()).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(service_response_json(response).await["error"], message);
        }

        cleanup_db(&db_path);
    }

    #[actix_web::test]
    async fn skill_metrics_handlers_return_reports_for_all_metric_variants() {
        let db_path = unique_handler_db("skill_metrics");
        write_completed_conversation_event(&db_path, "conv-skill-metrics");
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state_with_storage(db_path.clone()))
                .service(skill_metrics_all)
                .service(skill_metrics_downloads)
                .service(skill_metrics_loads)
                .service(skill_metrics_usage_ratio)
                .service(skill_metrics_distribution)
                .service(skill_metrics_hotness),
        )
        .await;

        for uri in [
            "/skill-metrics?start_ns=0&end_ns=9223372036854775807&granularity=day",
            "/skill-metrics/downloads?start_ns=0&end_ns=9223372036854775807",
            "/skill-metrics/loads?start_ns=0&end_ns=9223372036854775807",
            "/skill-metrics/usage-ratio?start_ns=0&end_ns=9223372036854775807",
            "/skill-metrics/distribution?start_ns=0&end_ns=9223372036854775807",
            "/skill-metrics/hotness?start_ns=0&end_ns=9223372036854775807&granularity=day",
        ] {
            let response =
                awtest::call_service(&app, awtest::TestRequest::get().uri(uri).to_request()).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{uri} should return report"
            );
            let body = service_response_json(response).await;
            assert!(body.is_object(), "{uri} should return JSON object");
        }

        cleanup_db(&db_path);
    }

    #[actix_web::test]
    async fn storage_backed_handlers_report_database_open_errors() {
        let root = temp_root("handler_open_errors");
        std::fs::write(&root, b"not a directory").unwrap();
        let blocked_db = root.join("events.db");
        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        let app = awtest::init_service(
            App::new()
                .app_data(web::Data::new(AppState {
                    reuse_store: None,
                    reuse_llm_judge_enabled: false,
                    causal_store: None,
                    storage_path: blocked_db.clone(),
                    start_time: Instant::now(),
                    health_store: Arc::new(RwLock::new(HealthStore::new())),
                    interruption_store: None,
                    evaluation_store: Arc::new(
                        EvaluationStore::new_with_path(std::path::Path::new(":memory:")).unwrap(),
                    ),
                    enforcement: None,
                    containment: None,
                    audit_service: Arc::new(agentsight_audit::AuditService::new(
                        crate::security::SecurityStore::open_in_memory()
                            .unwrap()
                            .audit_store(),
                    )),
                    security_observability: super::super::SecurityObservabilityConfig {
                        timeout_ms: 0,
                    },
                    auth,
                    optimize: None,
                    trajectory_store: Arc::new(RwLock::new(None)),
                }))
                .service(list_sessions)
                .service(list_agent_names)
                .service(get_timeseries)
                .service(metrics)
                .service(export_atif_trace)
                .service(skill_metrics_all),
        )
        .await;

        for (uri, status) in [
            ("/sessions", StatusCode::INTERNAL_SERVER_ERROR),
            ("/agent-names", StatusCode::INTERNAL_SERVER_ERROR),
            ("/timeseries", StatusCode::INTERNAL_SERVER_ERROR),
            (
                "/export/atif/trace/trace-1",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            ("/skill-metrics", StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            let response =
                awtest::call_service(&app, awtest::TestRequest::get().uri(uri).to_request()).await;
            assert_eq!(response.status(), status, "{uri} should fail opening DB");
        }

        let metrics_response = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/metrics").to_request(),
        )
        .await;
        assert_eq!(metrics_response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = actix_web::body::to_bytes(metrics_response.into_body())
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("# ERROR opening database"));

        let _ = std::fs::remove_file(&root);
    }
}

// ─── Prometheus metrics endpoint ─────────────────────────────────────────────

/// GET /metrics — Prometheus text format token usage metrics
///
/// Exposes per-agent counters for input tokens, output tokens, total tokens,
/// and LLM request count, aggregated over all recorded history.
/// The response Content-Type is `text/plain; version=0.0.4` as required by
/// the Prometheus exposition format.
#[get("/metrics")]
pub async fn metrics(data: web::Data<AppState>) -> impl Responder {
    let db_path = &data.storage_path;

    let summaries = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(store) => match store.get_agent_token_summary() {
            Ok(v) => v,
            Err(e) => {
                return HttpResponse::InternalServerError()
                    .content_type("text/plain; version=0.0.4")
                    .body(format!("# ERROR querying metrics: {e}\n"));
            }
        },
        Err(e) => {
            return HttpResponse::InternalServerError()
                .content_type("text/plain; version=0.0.4")
                .body(format!("# ERROR opening database: {e}\n"));
        }
    };

    let mut out = String::with_capacity(512 + summaries.len() * 128);

    // agentsight_token_input_total
    out.push_str(
        "# HELP agentsight_token_input_total Total input tokens consumed by agent (all-time)\n",
    );
    out.push_str("# TYPE agentsight_token_input_total counter\n");
    for s in &summaries {
        out.push_str(&format!(
            "agentsight_token_input_total{{agent=\"{}\"}} {}\n",
            escape_label(&s.agent_name),
            s.input_tokens
        ));
    }
    out.push('\n');

    // agentsight_token_output_total
    out.push_str(
        "# HELP agentsight_token_output_total Total output tokens consumed by agent (all-time)\n",
    );
    out.push_str("# TYPE agentsight_token_output_total counter\n");
    for s in &summaries {
        out.push_str(&format!(
            "agentsight_token_output_total{{agent=\"{}\"}} {}\n",
            escape_label(&s.agent_name),
            s.output_tokens
        ));
    }
    out.push('\n');

    // agentsight_token_total_total
    out.push_str("# HELP agentsight_token_total_total Total tokens (input+output) consumed by agent (all-time)\n");
    out.push_str("# TYPE agentsight_token_total_total counter\n");
    for s in &summaries {
        out.push_str(&format!(
            "agentsight_token_total_total{{agent=\"{}\"}} {}\n",
            escape_label(&s.agent_name),
            s.total_tokens
        ));
    }
    out.push('\n');

    // agentsight_llm_requests_total
    out.push_str(
        "# HELP agentsight_llm_requests_total Total LLM requests made by agent (all-time)\n",
    );
    out.push_str("# TYPE agentsight_llm_requests_total counter\n");
    for s in &summaries {
        out.push_str(&format!(
            "agentsight_llm_requests_total{{agent=\"{}\"}} {}\n",
            escape_label(&s.agent_name),
            s.request_count
        ));
    }
    out.push('\n');

    // agentsight_interruptions_total (per type, all-time)
    if let Some(ref istore) = data.interruption_store {
        // `None`: a Prometheus counter must never decrease, so resolving an
        // event may not remove it from this total.
        if let Ok(stats) = istore.stats(0, i64::MAX, None) {
            out.push_str(
                "# HELP agentsight_interruptions_total Total interruption events by type\n",
            );
            out.push_str("# TYPE agentsight_interruptions_total counter\n");
            for s in &stats {
                out.push_str(&format!(
                    "agentsight_interruptions_total{{type=\"{}\"}} {}\n",
                    escape_label(&s.interruption_type),
                    s.count
                ));
            }
            out.push('\n');
        }
    }

    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .body(out)
}

// ─── Agent health endpoint ──────────────────────────────────────────────────

/// Response body for /api/agent-health
#[derive(Debug, Serialize)]
pub struct AgentHealthResponse {
    /// Historical activity summaries sorted by most recent observation.
    pub agents: Vec<AgentActivity>,
    /// Data sources that could not be queried; omitted when all available sources succeeded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Live process-health response retained for diagnostics and recovery actions.
#[derive(Debug, Serialize)]
pub struct AgentProcessHealthResponse {
    pub agents: Vec<AgentHealthStatus>,
    pub last_scan_time: u64,
    /// Entries hidden by the default view (Cosh + healthy client agents).
    pub filtered_count: usize,
}

/// Historical activity for one Agent across the available SQLite stores.
#[derive(Debug, Clone, Serialize)]
pub struct AgentActivity {
    /// Canonical Agent display name.
    pub agent_name: String,
    /// Most recent observation in either source, as Unix nanoseconds.
    pub last_seen_ns: i64,
    /// LLM calls recorded in `genai_events.db`.
    pub genai_calls: i64,
    /// Tokens recorded in `genai_events.db`.
    pub genai_tokens: i64,
    /// Steps recorded in `trajectories.db`.
    pub trajectory_steps: i64,
    /// Tokens recorded in `trajectories.db`.
    pub trajectory_tokens: i64,
    /// SQLite stores that contributed to this Agent summary.
    pub source: String,
}

fn merge_agent_activity_summaries(
    genai: Vec<crate::storage::sqlite::genai::AgentActivitySummary>,
    trajectories: Vec<agentsight_trajectory_collector::TrajectoryAgentActivitySummary>,
) -> Vec<AgentActivity> {
    let mut by_name = HashMap::new();
    for summary in genai {
        let key = summary.agent_name.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        by_name.insert(
            key,
            AgentActivity {
                agent_name: summary.agent_name,
                last_seen_ns: summary.last_seen_ns,
                genai_calls: summary.total_calls,
                genai_tokens: summary.total_tokens,
                trajectory_steps: 0,
                trajectory_tokens: 0,
                source: "genai_events".to_string(),
            },
        );
    }

    for summary in trajectories {
        let key = summary.agent_name.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        if let Some(existing) = by_name.get_mut(&key) {
            existing.last_seen_ns = existing.last_seen_ns.max(summary.last_seen_ns);
            existing.trajectory_steps = summary.total_steps;
            existing.trajectory_tokens = summary.total_tokens;
            existing.source = "genai_events+trajectories".to_string();
        } else {
            by_name.insert(
                key,
                AgentActivity {
                    agent_name: summary.agent_name,
                    last_seen_ns: summary.last_seen_ns,
                    genai_calls: 0,
                    genai_tokens: 0,
                    trajectory_steps: summary.total_steps,
                    trajectory_tokens: summary.total_tokens,
                    source: "trajectories".to_string(),
                },
            );
        }
    }

    let mut agents: Vec<_> = by_name.into_values().collect();
    agents.sort_by(|left, right| {
        right
            .last_seen_ns
            .cmp(&left.last_seen_ns)
            .then_with(|| left.agent_name.cmp(&right.agent_name))
    });
    agents
}

/// GET /api/agent-health
///
/// Returns every Agent observed in either the GenAI event or trajectory store.
#[get("/agent-health")]
pub async fn get_agent_health(data: web::Data<AppState>) -> impl Responder {
    let storage_path = data.storage_path.clone();
    let app_state = data.clone();
    let loaded = web::block(move || {
        let genai = GenAISqliteStore::new_with_path(
            &storage_path,
            crate::config::InsertStoragePolicy::default(),
        )
        .map_err(|error| error.to_string())
        .and_then(|store| {
            store
                .list_agent_activity_summaries()
                .map_err(|error| error.to_string())
        });
        let trajectories = app_state.trajectory_store().map(|store| {
            store
                .list_agent_activity_summaries()
                .map_err(|error| error.to_string())
        });
        (genai, trajectories)
    })
    .await;

    let Ok((genai_result, trajectory_result)) = loaded else {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Agent activity query worker failed"
        }));
    };

    let mut warnings = Vec::new();
    let (genai, genai_failed) = match genai_result {
        Ok(summaries) => (summaries, false),
        Err(error) => {
            log::warn!("Failed to query GenAI Agent activity: {error}");
            warnings.push("genai_events unavailable".to_string());
            (Vec::new(), true)
        }
    };
    let (trajectories, trajectory_available) = match trajectory_result {
        Some(Ok(summaries)) => (summaries, true),
        Some(Err(error)) => {
            log::warn!("Failed to query trajectory Agent activity: {error}");
            warnings.push("trajectories unavailable".to_string());
            (Vec::new(), false)
        }
        None => (Vec::new(), false),
    };

    if genai_failed && !trajectory_available {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "No Agent activity data source is available",
            "warnings": warnings
        }));
    }

    HttpResponse::Ok().json(AgentHealthResponse {
        agents: merge_agent_activity_summaries(genai, trajectories),
        warnings,
    })
}

/// GET /api/agent-process-health
///
/// Returns live process-health snapshots used by hung detection and recovery UI.
#[get("/agent-process-health")]
pub async fn get_agent_process_health(
    data: web::Data<AppState>,
    req: actix_web::HttpRequest,
) -> impl Responder {
    let include_clients = req.query_string().contains("include_clients=true");
    let store = data.health_store.read().unwrap_or_else(|e| e.into_inner());
    let all = store.all_agents();
    let total = all.len();
    let agents: Vec<AgentHealthStatus> = all
        .into_iter()
        .filter(|agent| agent.agent_name != "Cosh")
        .filter(|agent| {
            include_clients
                || agent.role == crate::health::store::AgentRole::Gateway
                || agent.status == crate::health::store::AgentHealthState::Offline
        })
        .collect();
    let filtered_count = total.saturating_sub(agents.len());

    HttpResponse::Ok().json(AgentProcessHealthResponse {
        agents,
        last_scan_time: store.last_scan_time,
        filtered_count,
    })
}

/// DELETE /api/agent-health/{pid}
///
/// User-acknowledges an offline agent and removes it from the store.
pub async fn delete_agent_health(
    data: web::Data<AppState>,
    path: web::Path<u32>,
) -> impl Responder {
    let pid = path.into_inner();
    let removed = data.health_store.write().unwrap().remove_by_pid(pid);
    if removed {
        HttpResponse::Ok().json(serde_json::json!({"ok": true}))
    } else {
        HttpResponse::NotFound().json(serde_json::json!({"error": "pid not found"}))
    }
}

/// POST /api/agent-health/{pid}/restart
///
/// Kill the hung process and re-launch it with its original command line.
#[post("/agent-health/{pid}/restart")]
pub async fn restart_agent_health(
    data: web::Data<AppState>,
    path: web::Path<u32>,
) -> impl Responder {
    let pid = path.into_inner();

    let restart_cmd = {
        let store = data.health_store.read().unwrap();
        store
            .all_agents()
            .into_iter()
            .find(|a| a.pid == pid)
            .and_then(|a| a.restart_cmd)
    };

    let cmd = match restart_cmd {
        Some(c) if !c.is_empty() => c,
        _ => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "no restart command available for this pid"}));
        }
    };

    // Step 1: kill -9
    use std::process::Command;
    let kill_result = Command::new("kill").args(["-9", &pid.to_string()]).output();

    if let Err(e) = kill_result {
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": format!("kill failed: {}", e)}));
    }

    // Step 2: short wait for process to exit
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Step 3: re-exec (background, don't wait)
    let exe = &cmd[0];
    let args = &cmd[1..];
    match Command::new(exe).args(args).spawn() {
        Ok(child) => {
            let new_pid = child.id();
            log::info!("Restarted agent pid={pid} -> new pid={new_pid}, cmd={cmd:?}");
            data.health_store.write().unwrap().remove_by_pid(pid);
            HttpResponse::Ok().json(serde_json::json!({
                "ok": true,
                "new_pid": new_pid,
                "cmd": cmd,
            }))
        }
        Err(e) => HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": format!("re-exec failed: {}", e)})),
    }
}

// ─── ATIF export endpoints ──────────────────────────────────────────────────

/// GET /api/export/atif/trace/{trace_id}
///
/// Exports a single trace as an ATIF trajectory document (shared v1.7 schema).
#[get("/export/atif/trace/{trace_id}")]
pub async fn export_atif_trace(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let trace_id = path.into_inner();

    let store = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let events = match store.get_trace_events(&trace_id) {
        Ok(e) => e,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    if events.is_empty() {
        return HttpResponse::NotFound().json(serde_json::json!({"error": "trace not found"}));
    }

    match crate::atif::convert_trace_to_atif(&trace_id, events) {
        Ok(doc) => HttpResponse::Ok().json(doc),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/export/atif/session/{session_id}
///
/// Exports a full session (all traces) as an ATIF trajectory document (v1.7).
#[get("/export/atif/session/{session_id}")]
pub async fn export_atif_session(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let session_id = path.into_inner();

    let store = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let events = match store.get_events_by_session(&session_id) {
        Ok(e) => e,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    if events.is_empty() {
        return HttpResponse::NotFound().json(serde_json::json!({"error": "session not found"}));
    }

    match crate::atif::convert_session_to_atif(&session_id, events) {
        Ok(doc) => HttpResponse::Ok().json(doc),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/export/atif/conversation/{conversation_id}
///
/// Exports all LLM calls for a conversation as an ATIF trajectory document (v1.7).
#[get("/export/atif/conversation/{conversation_id}")]
pub async fn export_atif_conversation(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let conversation_id = path.into_inner();

    let store = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let events = match store.get_events_by_conversation(&conversation_id) {
        Ok(e) => e,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    if events.is_empty() {
        return HttpResponse::NotFound()
            .json(serde_json::json!({"error": "conversation not found"}));
    }

    match crate::atif::convert_trace_to_atif(&conversation_id, events) {
        Ok(doc) => HttpResponse::Ok().json(doc),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

// ─── Interruption endpoints ────────────────────────────────────────────────────

/// Query parameters for /api/interruptions
#[derive(Debug, Deserialize)]
pub struct InterruptionQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub agent_name: Option<String>,
    /// Filter by type: llm_error | sse_truncated | agent_crash | token_limit | context_overflow
    pub interruption_type: Option<String>,
    pub severity: Option<String>,
    pub resolved: Option<bool>,
    pub limit: Option<i64>,
}

/// GET /api/interruptions
///
/// Returns a list of interruption events matching the query.
#[get("/interruptions")]
pub async fn list_interruptions(
    data: web::Data<AppState>,
    query: web::Query<InterruptionQuery>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64); // 24 h
    let limit = query.limit.unwrap_or(200);

    match istore.list(
        start_ns,
        end_ns,
        query.agent_name.as_deref(),
        query.interruption_type.as_deref(),
        query.severity.as_deref(),
        query.resolved,
        limit,
    ) {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/interruptions/count?start_ns=<i64>&end_ns=<i64>&agent_name=<str>
///
/// Returns total interruption count + breakdown by severity within a time range.
/// Counts unresolved events only, so the total always equals the sum of the
/// `session-counts` / `conversation-counts` breakdowns.
/// Response: { total, by_severity: { critical, high, medium, low } }
#[get("/interruptions/count")]
pub async fn interruption_count(
    data: web::Data<AppState>,
    query: web::Query<InterruptionQuery>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);

    match istore.stats(start_ns, end_ns, Some(false)) {
        Ok(stats) => {
            let mut total = 0u64;
            let mut critical = 0u64;
            let mut high = 0u64;
            let mut medium = 0u64;
            let mut low = 0u64;
            for s in &stats {
                total += s.count as u64;
                match s.severity.as_str() {
                    "critical" => critical += s.count as u64,
                    "high" => high += s.count as u64,
                    "medium" => medium += s.count as u64,
                    _ => low += s.count as u64,
                }
            }
            HttpResponse::Ok().json(serde_json::json!({
                "total": total,
                "by_severity": {
                    "critical": critical,
                    "high":     high,
                    "medium":   medium,
                    "low":      low
                }
            }))
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/interruptions/stats
///
/// Returns per-type count statistics within a time range. Unresolved only, to
/// stay consistent with the overview card whose tooltip this feeds.
#[get("/interruptions/stats")]
pub async fn interruption_stats(
    data: web::Data<AppState>,
    query: web::Query<InterruptionQuery>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);

    match istore.stats(start_ns, end_ns, Some(false)) {
        Ok(stats) => HttpResponse::Ok().json(stats),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/interruptions/session-counts?start_ns=<i64>&end_ns=<i64>
///
/// Returns unresolved interruption breakdown per session_id, grouped by severity and type.
#[get("/interruptions/session-counts")]
pub async fn interruption_session_counts(
    data: web::Data<AppState>,
    query: web::Query<InterruptionQuery>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);

    match istore.count_unresolved_by_session_detailed(start_ns, end_ns, query.agent_name.as_deref())
    {
        Ok(rows) => {
            let mut map: std::collections::HashMap<
                String,
                (
                    i64,
                    std::collections::HashMap<String, i64>,
                    Vec<serde_json::Value>,
                ),
            > = std::collections::HashMap::new();
            for (sid, severity, itype, cnt) in rows {
                let entry = map
                    .entry(sid)
                    .or_insert_with(|| (0, std::collections::HashMap::new(), Vec::new()));
                entry.0 += cnt;
                *entry.1.entry(severity.clone()).or_insert(0) += cnt;
                entry.2.push(serde_json::json!({
                    "interruption_type": itype,
                    "severity": severity,
                    "count": cnt,
                }));
            }
            let json: Vec<_> = map
                .into_iter()
                .map(|(sid, (total, by_sev, types))| {
                    serde_json::json!({
                        "session_id": sid,
                        "total": total,
                        "by_severity": {
                            "critical": by_sev.get("critical").copied().unwrap_or(0),
                            "high": by_sev.get("high").copied().unwrap_or(0),
                            "medium": by_sev.get("medium").copied().unwrap_or(0),
                            "low": by_sev.get("low").copied().unwrap_or(0),
                        },
                        "types": types,
                    })
                })
                .collect();
            HttpResponse::Ok().json(json)
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/interruptions/conversation-counts?start_ns=<i64>&end_ns=<i64>
///
/// Returns unresolved interruption breakdown per (session_id, conversation_id),
/// grouped by severity and type.
///
/// The pair is the grouping key, not `conversation_id` alone: the dashboard
/// nests conversation rows under a session, so a session-less event must not be
/// attributed to whichever session owns its conversation — the
/// unassigned-session row already accounts for it.
#[get("/interruptions/conversation-counts")]
pub async fn interruption_conversation_counts(
    data: web::Data<AppState>,
    query: web::Query<InterruptionQuery>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);

    match istore.count_unresolved_by_conversation_detailed(
        start_ns,
        end_ns,
        query.agent_name.as_deref(),
    ) {
        Ok(rows) => {
            let mut map: std::collections::HashMap<
                (String, String),
                (
                    i64,
                    std::collections::HashMap<String, i64>,
                    Vec<serde_json::Value>,
                ),
            > = std::collections::HashMap::new();
            for (sid, cid, severity, itype, cnt) in rows {
                let entry = map
                    .entry((sid, cid))
                    .or_insert_with(|| (0, std::collections::HashMap::new(), Vec::new()));
                entry.0 += cnt;
                *entry.1.entry(severity.clone()).or_insert(0) += cnt;
                entry.2.push(serde_json::json!({
                    "interruption_type": itype,
                    "severity": severity,
                    "count": cnt,
                }));
            }
            let json: Vec<_> = map
                .into_iter()
                .map(|((sid, cid), (total, by_sev, types))| {
                    serde_json::json!({
                        "session_id": sid,
                        "conversation_id": cid,
                        "total": total,
                        "by_severity": {
                            "critical": by_sev.get("critical").copied().unwrap_or(0),
                            "high": by_sev.get("high").copied().unwrap_or(0),
                            "medium": by_sev.get("medium").copied().unwrap_or(0),
                            "low": by_sev.get("low").copied().unwrap_or(0),
                        },
                        "types": types,
                    })
                })
                .collect();
            HttpResponse::Ok().json(json)
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/sessions/{session_id}/interruptions
///
/// Returns all interruption events for a specific session.
#[get("/sessions/{session_id}/interruptions")]
pub async fn list_session_interruptions(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let session_id = path.into_inner();
    match istore.list_by_session(&session_id) {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/conversations/{conversation_id}/interruptions
///
/// Returns all interruption events for a specific conversation.
#[get("/conversations/{conversation_id}/interruptions")]
pub async fn list_conversation_interruptions(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let conversation_id = path.into_inner();
    match istore.list_by_conversation(&conversation_id) {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// POST /api/interruptions/{interruption_id}/resolve
///
/// Mark a specific interruption event as resolved.
pub async fn resolve_interruption(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let interruption_id = path.into_inner();
    match istore.resolve(&interruption_id) {
        Ok(true) => HttpResponse::Ok().json(serde_json::json!({"status": "resolved"})),
        Ok(false) => {
            HttpResponse::NotFound().json(serde_json::json!({"error": "Interruption not found"}))
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/interruptions/{interruption_id}
///
/// Get a single interruption event by ID.
#[get("/interruptions/{interruption_id}")]
pub async fn get_interruption(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let Some(ref istore) = data.interruption_store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "Interruption store not initialized"}));
    };

    let interruption_id = path.into_inner();
    match istore.get_by_id(&interruption_id) {
        Ok(Some(row)) => HttpResponse::Ok().json(row),
        Ok(None) => {
            HttpResponse::NotFound().json(serde_json::json!({"error": "Interruption not found"}))
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

// ─── Trajectory collection endpoints ────────────────────────────────────

/// Query parameters for `/api/trajectories`.
#[derive(Debug, Deserialize)]
pub struct TrajectoryQuery {
    pub project: Option<String>,
    pub source: Option<String>,
    pub agent_name: Option<String>,
    /// Max rows returned (default 200).
    pub limit: Option<i64>,
    /// Keep only trajectories whose effective reuse label is one of these
    /// (comma-separated: `good,bad`). Sessions never triaged match nothing —
    /// an agent asking for `good` history must not be served unassessed work.
    pub label: Option<String>,
    /// Drop trajectories whose label is one of these (`useless` by contract).
    pub exclude_label: Option<String>,
    /// Keep only trajectories a person settled (`confirm`/`override`).
    pub human_backed: Option<bool>,
}

/// Default and hard-cap for the trajectory list `limit` parameter.
const TRAJECTORY_DEFAULT_LIMIT: i64 = 200;
const TRAJECTORY_MAX_LIMIT: i64 = 1000;

/// GET /api/trajectories?project=&source=&agent_name=&limit=
///
/// Lists collected trajectories (newest first) without the bulky `atif_json`
/// column. When the trajectory store is unavailable (collection never ran),
/// returns an empty list with 200 for graceful UI degradation.
#[get("/trajectories")]
pub async fn list_trajectories(
    data: web::Data<AppState>,
    query: web::Query<TrajectoryQuery>,
) -> impl Responder {
    let Some(tstore) = data.trajectory_store() else {
        return HttpResponse::Ok().json(Vec::<serde_json::Value>::new());
    };
    // Normalize: <= 0 falls back to default; cap at hard upper bound to
    // prevent unbounded full-table scans (SQLite treats negative LIMIT as
    // "no limit").
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
            filter_rows_by_reuse_labels(&data, &query, &mut rows);
            HttpResponse::Ok().json(rows)
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Applies the reuse-label query parameters to trajectory summary rows.
///
/// The label lives in `reuse.db`, the summary in `trajectories.db`; rather
/// than teach the collector crate about labels (it owns storage, not reuse
/// policy), the handler resolves the label set once and keeps rows whose id is
/// in it. Unlabelled sessions drop out of every label filter — including
/// `exclude_label`, which only removes what has actually been assessed.
fn filter_rows_by_reuse_labels(
    data: &web::Data<AppState>,
    query: &TrajectoryQuery,
    rows: &mut Vec<agentsight_trajectory_collector::TrajectorySummary>,
) {
    let labels_needed =
        query.label.is_some() || query.exclude_label.is_some() || query.human_backed == Some(true);
    if !labels_needed {
        return;
    }
    let Some(labels) = data.reuse_store.as_deref() else {
        // No label store: nothing has been assessed, so a positive filter
        // matches nothing. Serve the empty truth rather than unfiltered rows.
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
///
/// Returns distinct project / source / agent_name values for the list UI
/// dropdowns. Must be registered before `/trajectories/{session_id}`.
#[get("/trajectories/filters")]
pub async fn trajectory_filters(data: web::Data<AppState>) -> impl Responder {
    let Some(tstore) = data.trajectory_store() else {
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
///
/// Returns the stored ATIF v1.7 JSON document for one trajectory. When the
/// session has subagent rows (`<session_id>:subagent:%`), they are embedded
/// into the document's `subagent_trajectories` array so the frontend can
/// navigate them via breadcrumb.
#[get("/trajectories/{session_id}")]
pub async fn get_trajectory_detail(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let Some(tstore) = data.trajectory_store() else {
        return HttpResponse::NotFound().json(
            serde_json::json!({"error": "not_found", "message": "Trajectory store not available"}),
        );
    };
    let session_id = path.into_inner();
    match tstore.get_atif_json(&session_id) {
        Ok(Some(atif_json)) => {
            // Inject subagent_trajectories if any exist.
            let enriched = match tstore.get_subagent_atif_jsons(&session_id) {
                Ok(subs) if !subs.is_empty() => inject_subagents(&atif_json, &subs),
                _ => atif_json,
            };
            HttpResponse::Ok()
                .content_type("application/json")
                .body(enriched)
        }
        Ok(None) => HttpResponse::NotFound()
            .json(serde_json::json!({"error": "not_found", "message": "Trajectory not found"})),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Query parameters for `/api/trajectories/steps`.
#[derive(Debug, Deserialize)]
pub struct TrajectoryStepQuery {
    /// Comma-separated step categories, matched as OR. Omit for every step.
    pub category: Option<String>,
    pub agent_name: Option<String>,
    pub project: Option<String>,
    pub source: Option<String>,
    pub session_id: Option<String>,
    /// Max hits returned (default 50).
    pub limit: Option<i64>,
    /// Neighbouring steps returned on each side of a hit (default 3).
    pub context: Option<i64>,
    /// Max trajectories read from disk (default 500).
    pub max_scan: Option<i64>,
}

/// Defaults and hard caps for `/api/trajectories/steps`.
const STEP_DEFAULT_LIMIT: i64 = 50;
const STEP_MAX_LIMIT: i64 = 500;
const STEP_DEFAULT_CONTEXT: i64 = 3;
const STEP_MAX_CONTEXT: i64 = 10;
const STEP_DEFAULT_MAX_SCAN: i64 = 500;
const STEP_MAX_MAX_SCAN: i64 = 2000;

/// GET /api/trajectories/steps?category=&agent_name=&project=&source=&session_id=&limit=&context=&max_scan=
///
/// Finds steps by derived category (`user_input`, `system`, `agent_message`,
/// `thinking`, `tool_call`, `tool_result`), each returned with its neighbouring
/// steps so a hit can be read in context. Categories are multi-label: a step
/// that reasons and calls a tool matches either filter.
///
/// Must be registered before `/trajectories/{session_id}`.
#[get("/trajectories/steps")]
pub async fn list_trajectory_steps(
    data: web::Data<AppState>,
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

    let Some(tstore) = data.trajectory_store() else {
        return HttpResponse::Ok().json(serde_json::json!({
            "hits": [], "scanned_trajectories": 0,
            "truncated": false, "skipped_unparsable": 0
        }));
    };

    // Normalize: <= 0 falls back to default and every value is capped, so a
    // hostile `limit` cannot turn into an unbounded scan. `context` may be 0.
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

/// Parse the parent ATIF JSON, embed subagent documents into
/// `subagent_trajectories`, and re-serialize.
fn inject_subagents(parent_json: &str, sub_jsons: &[String]) -> String {
    let mut doc: serde_json::Value = match serde_json::from_str(parent_json) {
        Ok(v) => v,
        Err(_) => return parent_json.to_string(),
    };
    let subs: Vec<serde_json::Value> = sub_jsons
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    if subs.is_empty() {
        return parent_json.to_string();
    }
    doc["subagent_trajectories"] = serde_json::Value::Array(subs);
    serde_json::to_string(&doc).unwrap_or_else(|_| parent_json.to_string())
}

// ─── Skill Metrics endpoints ─────────────────────────────────────────────────

/// Query parameters for skill metrics endpoints.
#[derive(Debug, Deserialize)]
pub struct SkillMetricsQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub agent_name: Option<String>,
    /// Granularity for hotness trend: "day" or "week" (default: "week")
    pub granularity: Option<String>,
}

/// GET /api/skill-metrics — full skill metrics report
#[get("/skill-metrics")]
pub async fn skill_metrics_all(
    data: web::Data<AppState>,
    query: web::Query<SkillMetricsQuery>,
) -> impl Responder {
    compute_skill_metrics_response(
        &data.storage_path,
        &query,
        crate::skill_metrics::MetricOptions::all(),
    )
}

/// GET /api/skill-metrics/downloads
#[get("/skill-metrics/downloads")]
pub async fn skill_metrics_downloads(
    data: web::Data<AppState>,
    query: web::Query<SkillMetricsQuery>,
) -> impl Responder {
    compute_skill_metrics_response(
        &data.storage_path,
        &query,
        crate::skill_metrics::MetricOptions {
            downloads: true,
            ..Default::default()
        },
    )
}

/// GET /api/skill-metrics/loads
#[get("/skill-metrics/loads")]
pub async fn skill_metrics_loads(
    data: web::Data<AppState>,
    query: web::Query<SkillMetricsQuery>,
) -> impl Responder {
    compute_skill_metrics_response(
        &data.storage_path,
        &query,
        crate::skill_metrics::MetricOptions {
            loads: true,
            ..Default::default()
        },
    )
}

/// GET /api/skill-metrics/usage-ratio
#[get("/skill-metrics/usage-ratio")]
pub async fn skill_metrics_usage_ratio(
    data: web::Data<AppState>,
    query: web::Query<SkillMetricsQuery>,
) -> impl Responder {
    compute_skill_metrics_response(
        &data.storage_path,
        &query,
        crate::skill_metrics::MetricOptions {
            usage_ratio: true,
            ..Default::default()
        },
    )
}

/// GET /api/skill-metrics/distribution
#[get("/skill-metrics/distribution")]
pub async fn skill_metrics_distribution(
    data: web::Data<AppState>,
    query: web::Query<SkillMetricsQuery>,
) -> impl Responder {
    compute_skill_metrics_response(
        &data.storage_path,
        &query,
        crate::skill_metrics::MetricOptions {
            distribution: true,
            ..Default::default()
        },
    )
}

/// GET /api/skill-metrics/hotness
#[get("/skill-metrics/hotness")]
pub async fn skill_metrics_hotness(
    data: web::Data<AppState>,
    query: web::Query<SkillMetricsQuery>,
) -> impl Responder {
    compute_skill_metrics_response(
        &data.storage_path,
        &query,
        crate::skill_metrics::MetricOptions {
            hotness: true,
            ..Default::default()
        },
    )
}

/// Shared implementation for all skill metrics endpoints.
fn compute_skill_metrics_response(
    storage_path: &std::path::Path,
    query: &SkillMetricsQuery,
    mut options: crate::skill_metrics::MetricOptions,
) -> HttpResponse {
    // Apply granularity from query params
    if let Some(ref g) = query.granularity {
        if g == "day" {
            options.hotness_granularity = crate::skill_metrics::HotnessGranularity::Day;
        }
    }

    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    // Default: 7 days
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 7 * 86_400_000_000_000i64);

    let store = match GenAISqliteStore::new_with_path(
        storage_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let events = match store.get_events_in_time_range(start_ns, end_ns, query.agent_name.as_deref())
    {
        Ok(e) => e,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let report = crate::skill_metrics::compute_skill_metrics(&events, &options);
    HttpResponse::Ok().json(report)
}
