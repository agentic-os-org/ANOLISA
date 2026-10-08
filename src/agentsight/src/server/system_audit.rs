//! AgentSight-owned System Audit API backed by local normalized security events.

use std::time::{SystemTime, UNIX_EPOCH};

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, get, post, web};
use agentsight_enforcement_protocol::{SecurityEvent, SecurityEventKind};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::AppState;
use crate::security::{
    RiskCaseStatus, SecurityEventFilter, SecuritySessionPage, SecurityStoreError,
};

#[derive(Debug, Default, Deserialize)]
pub(super) struct AuditQuery {
    start_ns: Option<u64>,
    end_ns: Option<u64>,
    event_type: Option<String>,
    result: Option<String>,
    policy_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    binding_id: Option<Uuid>,
    status: Option<String>,
    blocked: Option<bool>,
    limit: Option<usize>,
    offset: Option<i64>,
}

/// Returns local event totals and recent evidence for the System Audit overview.
#[get("/audit/summary")]
pub(super) async fn summary(
    data: web::Data<AppState>,
    query: web::Query<AuditQuery>,
) -> HttpResponse {
    if let Some(response) = reject_unrepresentable_window(&query) {
        return response;
    }
    if let Some(response) = reject_unknown_filter_tokens(&query) {
        return response;
    }
    let filter = event_filter(&query);
    let summary = match data.audit_service.summary(&filter) {
        Ok(summary) => summary,
        Err(error) => return store_error(error),
    };
    let latest_events = match data.audit_service.events(&SecurityEventFilter {
        limit: query.limit.unwrap_or(10).clamp(1, 100),
        ..filter.clone()
    }) {
        Ok(page) => page.items,
        Err(error) => return store_error(error),
    };
    let affected_sessions = match data.audit_service.sessions(&SecurityEventFilter {
        limit: 1,
        offset: 0,
        ..filter
    }) {
        Ok(page) => page.total,
        Err(error) => return store_error(error),
    };
    let risk_cases = match data.audit_service.case_summary() {
        Ok(summary) => summary,
        Err(error) => return store_error(error),
    };
    response(
        StatusCode::OK,
        if summary.total_events == 0 {
            "empty"
        } else {
            "ok"
        },
        json!({
            "total": summary.total_events,
            "blocked": summary.blocked_events,
            "evidence_loss": summary.evidence_loss_events,
            "affected_sessions": affected_sessions,
            "affected_runs": affected_sessions,
            "risk_cases_total": risk_cases.total,
            "risk_cases_open": risk_cases.open,
            "risk_cases_blocked": risk_cases.blocked,
            "latest_events": latest_events.iter().map(event_view).collect::<Vec<_>>(),
        }),
    )
}

/// Lists local security events without changing Security Observability endpoints.
#[get("/audit/events")]
pub(super) async fn events(
    data: web::Data<AppState>,
    query: web::Query<AuditQuery>,
) -> HttpResponse {
    if let Some(response) = reject_unrepresentable_window(&query) {
        return response;
    }
    if let Some(response) = reject_unknown_filter_tokens(&query) {
        return response;
    }
    match data.audit_service.events(&event_filter(&query)) {
        Ok(page) => {
            let state = if page.items.is_empty() { "empty" } else { "ok" };
            response(
                StatusCode::OK,
                state,
                json!({
                    "items": page.items.iter().map(event_view).collect::<Vec<_>>(),
                    "total": page.total,
                    "limit": page.limit,
                    "offset": page.offset,
                    "next_offset": ((page.offset as u64).saturating_add(page.items.len() as u64) < page.total)
                        .then(|| page.offset.saturating_add(page.limit as i64)),
                }),
            )
        }
        Err(error) => store_error(error),
    }
}

/// Groups local events into session summaries for audit navigation.
#[get("/audit/sessions")]
pub(super) async fn sessions(
    data: web::Data<AppState>,
    query: web::Query<AuditQuery>,
) -> HttpResponse {
    if let Some(response) = reject_unrepresentable_window(&query) {
        return response;
    }
    if let Some(response) = reject_unknown_filter_tokens(&query) {
        return response;
    }
    let page = match data.audit_service.sessions(&event_filter(&query)) {
        Ok(page) => page,
        Err(error) => return store_error(error),
    };
    let data = session_page_view(&page);
    response(
        StatusCode::OK,
        if page.items.is_empty() { "empty" } else { "ok" },
        data,
    )
}

fn session_page_view(page: &SecuritySessionPage) -> Value {
    let items = page
        .items
        .iter()
        .map(|session| {
            json!({
                "session_id": session.session_id,
                "first_seen_ns": session.first_seen_ns,
                "last_seen_ns": session.last_seen_ns,
                "security_event_count": session.security_event_count,
                "observability_event_count": 0,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "items": items,
        "total": page.total,
        "limit": page.limit,
        "offset": page.offset,
        "next_offset": ((page.offset as u64).saturating_add(page.items.len() as u64) < page.total)
            .then(|| page.offset.saturating_add(page.limit as i64)),
    })
}

/// Lists correlated risk cases.
#[get("/audit/cases")]
pub(super) async fn cases(
    data: web::Data<AppState>,
    query: web::Query<AuditQuery>,
) -> HttpResponse {
    if let Some(response) = reject_unknown_filter_tokens(&query) {
        return response;
    }
    if let Some(response) = reject_event_filters_on_cases(&query) {
        return response;
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 1_000);
    let offset = query.offset.unwrap_or(0).max(0);
    let agent_id = query.agent_id.as_deref();
    let status = query.status.as_deref();
    let blocked = query.blocked;
    let total = match data.audit_service.case_count(agent_id, status, blocked) {
        Ok(total) => total,
        Err(error) => return store_error(error),
    };
    match data
        .audit_service
        .cases(limit, offset, agent_id, status, blocked)
    {
        Ok(items) => response(
            StatusCode::OK,
            if items.is_empty() { "empty" } else { "ok" },
            json!({ "total": total, "items": items, "limit": limit, "offset": offset }),
        ),
        Err(error) => store_error(error),
    }
}

/// Reject event filters the correlated-case query cannot apply.
///
/// `AuditQuery` is shared by the four read handlers, but `/audit/cases` reads
/// the `risk_cases` table: `list_cases` and `case_count` take only `agent_id`,
/// `status` and `blocked`. The event-level fields were accepted and dropped,
/// so a caller that filtered cases by `event_type`, `policy_id` or a time
/// window received every case with a 200 — a plausible-looking wrong result,
/// the shape the interruption aggregates refuse with the same 400.
fn reject_event_filters_on_cases(query: &AuditQuery) -> Option<HttpResponse> {
    let unsupported = [
        ("start_ns", query.start_ns.is_some()),
        ("end_ns", query.end_ns.is_some()),
        ("event_type", query.event_type.is_some()),
        ("result", query.result.is_some()),
        ("policy_id", query.policy_id.is_some()),
        ("session_id", query.session_id.is_some()),
        ("binding_id", query.binding_id.is_some()),
    ];
    let unsupported: Vec<&str> = unsupported
        .iter()
        .filter_map(|(name, present)| present.then_some(*name))
        .collect();
    if unsupported.is_empty() {
        return None;
    }
    Some(bad_request(&format!(
        "GET /api/audit/cases accepts only agent_id, status, blocked, limit and offset; \
         {} must be applied on GET /api/audit/events, which filters individual events",
        unsupported.join(", ")
    )))
}

/// Returns one case and its ordered evidence chain.
#[get("/audit/cases/{case_id}")]
pub(super) async fn case_detail(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    let case_id = match Uuid::parse_str(&path.into_inner()) {
        Ok(case_id) => case_id,
        Err(_) => return bad_request("case_id must be a UUID"),
    };
    match data.audit_service.case(case_id) {
        Ok(detail) => match data.audit_service.latest_containment(case_id) {
            Ok(action) => response(
                StatusCode::OK,
                "found",
                super::containment::case_detail_view(json!(detail), action.as_ref()),
            ),
            Err(error) => store_error(error),
        },
        Err(SecurityStoreError::MissingCase(_)) => response(
            StatusCode::NOT_FOUND,
            "not_found",
            json!({ "case_id": case_id }),
        ),
        Err(error) => store_error(error),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ReviewRequest {
    status: RiskCaseStatus,
}

/// Records a human review disposition without mutating immutable evidence.
#[post("/audit/cases/{case_id}/review")]
pub(super) async fn review_case(
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let case_id = match Uuid::parse_str(&path.into_inner()) {
        Ok(case_id) => case_id,
        Err(_) => return bad_request("case_id must be a UUID"),
    };
    if body.status == RiskCaseStatus::Open {
        return bad_request("status must be confirmed, false_positive, accepted_risk, or resolved");
    }
    match data.audit_service.review(case_id, body.status, now_ns()) {
        Ok(case) => response(StatusCode::OK, "updated", json!(case)),
        Err(SecurityStoreError::MissingCase(_)) => response(
            StatusCode::NOT_FOUND,
            "not_found",
            json!({ "case_id": case_id }),
        ),
        Err(error) => store_error(error),
    }
}

/// Closed sets the read filters compare literally, mirrored from the writers.
///
/// `status` is written by `risk_status`, and `event_type` / `result` by
/// `EventMetadata::from_event`, so an unknown token can never match a stored
/// row. Accepting one and answering an empty 200 makes a typo
/// indistinguishable from a genuinely empty result — the write endpoint here
/// and the trajectory label filters already reject that with 400 — while the
/// open-ended identifiers (`policy_id`, `agent_id`, `session_id`,
/// `binding_id`) stay unchecked on purpose: a valid value may simply have no
/// rows yet.
const CASE_STATUS_TOKENS: [&str; 5] = [
    "open",
    "confirmed",
    "false_positive",
    "accepted_risk",
    "resolved",
];
const EVENT_TYPE_TOKENS: [&str; 5] = [
    "file_action",
    "taint_transition",
    "network_action",
    "policy_decision",
    "enforcement_state",
];
const EVENT_RESULT_TOKENS: [&str; 6] = [
    "allowed", "failed", "changed", "blocked", "ready", "degraded",
];

/// Reject a closed-set filter token that names no row the store can hold.
fn reject_unknown_filter_tokens(query: &AuditQuery) -> Option<HttpResponse> {
    let unknown = |value: Option<&String>, allowed: &[&str], name: &str| {
        value
            .filter(|value| !allowed.contains(&value.as_str()))
            .map(|value| format!("{name} '{value}' is not one of {}", allowed.join(", ")))
    };
    let message = unknown(query.status.as_ref(), &CASE_STATUS_TOKENS, "status")
        .or_else(|| unknown(query.event_type.as_ref(), &EVENT_TYPE_TOKENS, "event_type"))
        .or_else(|| unknown(query.result.as_ref(), &EVENT_RESULT_TOKENS, "result"))?;
    Some(bad_request(&message))
}

/// Rejects a window the store cannot represent, and one that runs backwards.
///
/// `AuditQuery` parses the bounds as `u64`, but the store keeps timestamps as
/// `i64` nanoseconds and fails the conversion with `TimestampOutOfRange` —
/// which `store_error` renders as a retryable "store unavailable". That reads
/// as a transient failure and invites the client to retry input that can never
/// succeed, so the range is refused up front instead.
///
/// An inverted range is representable but just as malformed, and the store's
/// `occurred_at_ns >= start AND <= end` predicate matches nothing for it, so
/// these endpoints answered an empty 200 — the misreading the window-guard
/// family (`reject_inverted_window`) eliminated everywhere else. Refuse it
/// with the same 400 and body, after the range check the cast relies on.
fn reject_unrepresentable_window(query: &AuditQuery) -> Option<HttpResponse> {
    let too_large = |value: Option<u64>| value.is_some_and(|v| v > i64::MAX as u64);
    if too_large(query.start_ns) || too_large(query.end_ns) {
        return Some(HttpResponse::BadRequest().json(json!({
            "error": {
                "code": "bad_request",
                "message": "start_ns and end_ns must fit in an i64 nanosecond timestamp",
                "retryable": false,
            }
        })));
    }
    super::handlers::reject_inverted_window(
        query.start_ns.map(|value| value as i64),
        query.end_ns.map(|value| value as i64),
    )
}

fn event_filter(query: &AuditQuery) -> SecurityEventFilter {
    SecurityEventFilter {
        start_ns: query.start_ns,
        end_ns: query.end_ns,
        event_type: query.event_type.clone(),
        result: query.result.clone(),
        policy_id: query.policy_id.clone(),
        agent_id: query.agent_id.clone(),
        session_id: query.session_id.clone(),
        binding_id: query.binding_id,
        limit: query.limit.unwrap_or(100),
        offset: query.offset.unwrap_or(0),
    }
}

fn event_view(event: &SecurityEvent) -> Value {
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(fields) = &mut value {
        fields.insert("timestamp_ns".into(), json!(event.occurred_at_ns));
        fields.insert("session_id".into(), json!(event.identity.session_id));
        fields.insert("tool_call_id".into(), json!(event.identity.tool_call_id));
        fields.insert("pid".into(), json!(event.identity.pid));
        fields.insert("category".into(), json!("system"));
        fields.insert("result".into(), json!(event_result(event)));
    }
    value
}

fn event_result(event: &SecurityEvent) -> &'static str {
    match &event.kind {
        SecurityEventKind::FileAction(action) => {
            if action.succeeded {
                "allowed"
            } else {
                "failed"
            }
        }
        SecurityEventKind::TaintTransition(_) => "changed",
        SecurityEventKind::NetworkAction(action) => {
            if action.succeeded {
                "allowed"
            } else {
                "blocked"
            }
        }
        SecurityEventKind::PolicyDecision(decision) => {
            if decision.blocked {
                "blocked"
            } else {
                "allowed"
            }
        }
        SecurityEventKind::EnforcementState(state) => {
            if state.ready {
                "ready"
            } else {
                "degraded"
            }
        }
    }
}

pub(super) fn response(status: StatusCode, state: &str, data: Value) -> HttpResponse {
    HttpResponse::build(status).json(json!({
        "state": state,
        "data": data,
        "meta": { "source": "agentsight" },
    }))
}

/// Maps a store failure to a sanitized API error by failure class.
///
/// Data-corruption errors must not be reported as store unavailability:
/// the store is reachable and retrying cannot succeed, so misclassifying
/// them sends operators down the wrong troubleshooting path.
pub(super) fn store_error(error: SecurityStoreError) -> HttpResponse {
    log::error!("system audit security store failed: {error}");
    match error {
        SecurityStoreError::Serialization(_) | SecurityStoreError::InvalidData(_) => {
            invalid_stored_data()
        }
        SecurityStoreError::Lifecycle(_)
        | SecurityStoreError::Open(_)
        | SecurityStoreError::Sqlite(_)
        | SecurityStoreError::InvalidFilter(_)
        | SecurityStoreError::MissingCase(_)
        | SecurityStoreError::TimestampOutOfRange(_)
        | SecurityStoreError::Poisoned
        | SecurityStoreError::PolicyRevisionConflict { .. } => store_unavailable(),
    }
}

// Details stay in the log above; the response must not leak paths or
// raw serde output.
fn invalid_stored_data() -> HttpResponse {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid_stored_data",
        "stored security event data is invalid",
        false,
    )
}

// Private so handlers cannot bypass the classification in store_error().
fn store_unavailable() -> HttpResponse {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "security_store_unavailable",
        "security data store is unavailable",
        true,
    )
}

pub(super) fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
    retryable: bool,
) -> HttpResponse {
    HttpResponse::build(status).json(json!({
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
        }
    }))
}

fn bad_request(message: &str) -> HttpResponse {
    HttpResponse::BadRequest().json(json!({
        "error": { "code": "bad_request", "message": message, "retryable": false }
    }))
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use actix_web::body::to_bytes;

    use crate::security::{SecuritySession, SecuritySessionPage, SecurityStoreError};

    use super::{reject_unrepresentable_window, session_page_view, store_error};

    #[test]
    fn audit_window_rejects_timestamps_the_store_cannot_hold() {
        // u64 parses happily, but the store keeps timestamps as i64 ns: without
        // this guard the request comes back as a retryable 500.
        let query = |start_ns, end_ns| super::AuditQuery {
            start_ns,
            end_ns,
            event_type: None,
            result: None,
            policy_id: None,
            agent_id: None,
            session_id: None,
            binding_id: None,
            status: None,
            blocked: None,
            limit: None,
            offset: None,
        };

        let oversized = i64::MAX as u64 + 1;
        assert!(reject_unrepresentable_window(&query(Some(oversized), None)).is_some());
        assert!(reject_unrepresentable_window(&query(None, Some(oversized))).is_some());
        assert!(reject_unrepresentable_window(&query(Some(i64::MAX as u64), None)).is_none());
        assert!(reject_unrepresentable_window(&query(Some(1), Some(2))).is_none());
        assert!(reject_unrepresentable_window(&query(None, None)).is_none());
    }

    #[test]
    fn audit_window_rejects_an_inverted_range() {
        // The store's `occurred_at_ns >= start AND <= end` predicate matches
        // nothing when start > end, so without this refusal the endpoints
        // answer an empty 200 for a malformed window — the misreading the
        // window-guard family (`reject_inverted_window`) already eliminated
        // on every sibling endpoint.
        let query = |start_ns, end_ns| super::AuditQuery {
            start_ns,
            end_ns,
            event_type: None,
            result: None,
            policy_id: None,
            agent_id: None,
            session_id: None,
            binding_id: None,
            status: None,
            blocked: None,
            limit: None,
            offset: None,
        };

        assert!(reject_unrepresentable_window(&query(Some(2000), Some(1000))).is_some());
        // Controls: ascending and equal bounds, and one-sided windows, pass.
        assert!(reject_unrepresentable_window(&query(Some(1000), Some(2000))).is_none());
        assert!(reject_unrepresentable_window(&query(Some(2000), Some(2000))).is_none());
        assert!(reject_unrepresentable_window(&query(Some(2000), None)).is_none());
        assert!(reject_unrepresentable_window(&query(None, Some(1000))).is_none());
    }

    #[actix_web::test]
    async fn audit_events_rejects_an_inverted_window() {
        use actix_web::{App, test as awtest};
        use std::sync::{Arc, RwLock};
        use std::time::Instant;

        use crate::server::AppState;

        // An inverted window must draw the family's 400, not the empty 200
        // the store's range predicate produces for it.
        let dir = std::env::temp_dir().join(format!("audit-inverted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let auth_config = crate::config::ServerAuthConfig { enabled: false };
        let auth = Arc::new(crate::server::auth::DashboardAuth::init(&auth_config, &dir));
        let state = actix_web::web::Data::new(AppState {
            storage_path: dir.join("agentsight.db"),
            genai_store: None,
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(crate::health::HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::new(
                crate::grader::EvaluationStore::new_with_path(&dir.join("evaluation.db")).unwrap(),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: crate::server::SecurityObservabilityConfig::default(),
            auth,
            optimize: None,
            reuse_store: None,
            trajectory_store: Arc::new(RwLock::new(None)),
            reuse_llm_judge_enabled: false,
            causal_store: None,
        });

        let app = awtest::init_service(App::new().app_data(state).service(super::events)).await;
        let request = awtest::TestRequest::get()
            .uri("/audit/events?start_ns=2000&end_ns=1000")
            .to_request();
        let response = awtest::call_service(&app, request).await;
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::BAD_REQUEST,
            "an inverted window must be rejected, not answered with an empty 200"
        );
        let body: serde_json::Value = awtest::read_body_json(response).await;
        assert_eq!(body["error"], "start_ns must not exceed end_ns");

        // Control: an ascending window on the same instance is a normal 200.
        let request = awtest::TestRequest::get()
            .uri("/audit/events?start_ns=1000&end_ns=2000")
            .to_request();
        let response = awtest::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);

        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn error_body(response: actix_web::HttpResponse) -> serde_json::Value {
        let body = to_bytes(response.into_body())
            .await
            .expect("error body should render");
        serde_json::from_slice(&body).expect("error body should be JSON")
    }

    #[actix_web::test]
    async fn store_errors_do_not_expose_internal_paths() {
        let value = error_body(store_error(SecurityStoreError::InvalidData(
            "database /private/db is corrupt".into(),
        )))
        .await;
        assert_eq!(value["error"]["code"], "invalid_stored_data");
        assert_eq!(
            value["error"]["message"],
            "stored security event data is invalid"
        );
        assert_eq!(value["error"]["retryable"], false);
        assert!(!value.to_string().contains("/private/db"));
    }

    #[actix_web::test]
    async fn persistence_errors_stay_store_unavailable() {
        let value = error_body(store_error(SecurityStoreError::Poisoned)).await;
        assert_eq!(value["error"]["code"], "security_store_unavailable");
        assert_eq!(value["error"]["retryable"], true);
    }

    #[actix_web::test]
    async fn serialization_errors_map_to_invalid_stored_data() {
        let serde_error = serde_json::from_str::<serde_json::Value>("not json")
            .expect_err("parsing garbage should fail");
        let value = error_body(store_error(SecurityStoreError::Serialization(serde_error))).await;
        assert_eq!(value["error"]["code"], "invalid_stored_data");
        assert_eq!(value["error"]["retryable"], false);
    }

    #[test]
    fn session_api_preserves_grouped_total_and_pagination() {
        let page = SecuritySessionPage {
            items: vec![SecuritySession {
                session_id: "session-1000".into(),
                first_seen_ns: 10,
                last_seen_ns: 20,
                security_event_count: 2_500,
            }],
            total: 1_005,
            limit: 1,
            offset: 1_000,
        };

        let data = session_page_view(&page);

        assert_eq!(data["total"], 1_005);
        assert_eq!(data["offset"], 1_000);
        assert_eq!(data["next_offset"], 1_001);
        assert_eq!(data["items"][0]["security_event_count"], 2_500);
    }

    #[test]
    fn session_api_survives_an_offset_near_the_i64_limit() {
        // The client-supplied offset is only clamped at zero, so it can be
        // i64::MAX. `then_some(page.offset + page.limit)` evaluated the sum
        // eagerly: with overflow checks on the handler panicked, without
        // them a wrapped negative next_offset reached the client.
        let page = SecuritySessionPage {
            items: vec![SecuritySession {
                session_id: "session-edge".into(),
                first_seen_ns: 10,
                last_seen_ns: 20,
                security_event_count: 1,
            }],
            total: i64::MAX as u64,
            limit: 100,
            offset: i64::MAX,
        };

        let data = session_page_view(&page);

        assert_eq!(data["offset"], i64::MAX);
        assert_eq!(
            data["next_offset"],
            serde_json::Value::Null,
            "no further page exists past the last offset"
        );
    }
}
