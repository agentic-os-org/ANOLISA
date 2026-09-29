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
                        .then_some(page.offset + page.limit as i64),
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
            .then_some(page.offset + page.limit as i64),
    })
}

/// Lists correlated risk cases.
#[get("/audit/cases")]
pub(super) async fn cases(
    data: web::Data<AppState>,
    query: web::Query<AuditQuery>,
) -> HttpResponse {
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

    use super::{session_page_view, store_error};

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
}
