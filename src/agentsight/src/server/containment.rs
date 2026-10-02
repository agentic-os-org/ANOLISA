//! HTTP boundary for case-level containment planning and activation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, get, post, web};
use agentsight_audit::AuditStore;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{AppState, system_audit};
use crate::enforcement::read_process_start_time;
use crate::health::{AgentHealthState, AgentHealthStatus, HealthStore};
use crate::security::{
    ContainmentAction, ContainmentCandidate, ContainmentCoordinator, ContainmentError,
    ContainmentFailureStage, ContainmentLifecycle, ContainmentPlan, ContainmentRequest,
    SecurityStoreError,
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const REQUESTED_BY: &str = "dashboard-token";

#[derive(Debug, Deserialize)]
pub(super) struct ContainCaseRequest {
    root_pid: i32,
    #[serde(default)]
    duration_secs: RequestedDuration,
}

#[derive(Debug, Default)]
enum RequestedDuration {
    #[default]
    Missing,
    Provided(Option<u64>),
}

impl<'de> Deserialize<'de> for RequestedDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<u64>::deserialize(deserializer).map(Self::Provided)
    }
}

#[derive(Debug)]
enum OperationError {
    Containment(ContainmentError),
    HealthStoreUnavailable,
}

/// Builds a confirmation plan from fresh trusted process health.
#[get("/audit/cases/{case_id}/containment-plan")]
pub(super) async fn containment_plan(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    let case_id = match parse_case_id(path.into_inner()) {
        Ok(case_id) => case_id,
        Err(response) => return response,
    };
    let Some(coordinator) = data.containment.clone() else {
        return unavailable();
    };
    let health_store = Arc::clone(&data.health_store);
    let security_store = Arc::clone(data.audit_service.store());
    match web::block(move || {
        let candidates = candidate_snapshot(&health_store, &security_store, case_id)?;
        coordinator
            .plan(case_id, candidates)
            .map_err(OperationError::Containment)
    })
    .await
    {
        Ok(Ok(plan)) => {
            system_audit::response(StatusCode::OK, "found", containment_plan_view(&plan))
        }
        Ok(Err(error)) => operation_error(error),
        Err(error) => blocking_error(error),
    }
}

/// Revalidates a selected process and activates case-derived enforcement.
#[post("/audit/cases/{case_id}/contain")]
pub(super) async fn contain_case(
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: Result<web::Json<ContainCaseRequest>, actix_web::Error>,
) -> HttpResponse {
    let case_id = match parse_case_id(path.into_inner()) {
        Ok(case_id) => case_id,
        Err(response) => return response,
    };
    let request = match body {
        Ok(body) => body.into_inner(),
        Err(_) => return request_error("request body must be valid JSON"),
    };
    let duration_secs = match request.duration_secs {
        RequestedDuration::Missing => return request_error("duration_secs is required"),
        RequestedDuration::Provided(duration_secs) => duration_secs,
    };
    let Some(coordinator) = data.containment.clone() else {
        return unavailable();
    };
    let health_store = Arc::clone(&data.health_store);
    let security_store = Arc::clone(data.audit_service.store());
    match web::block(move || {
        let candidates = candidate_snapshot(&health_store, &security_store, case_id)?;
        coordinator
            .contain(
                case_id,
                ContainmentRequest {
                    root_pid: request.root_pid,
                    duration_secs,
                },
                &candidates,
                REQUESTED_BY,
            )
            .map_err(OperationError::Containment)
    })
    .await
    {
        Ok(Ok(action)) => {
            let state = if action.blocked_at_ns.is_some() {
                "contained"
            } else {
                "policy_active"
            };
            system_audit::response(StatusCode::OK, state, containment_action_view(&action))
        }
        Ok(Err(error)) => operation_error(error),
        Err(error) => blocking_error(error),
    }
}

pub(super) fn start_reconciler(
    coordinator: &ContainmentCoordinator,
) -> Result<std::thread::JoinHandle<()>, ContainmentError> {
    coordinator.start_reconciler(RECONCILE_INTERVAL)
}

pub(super) fn stop_reconciler(
    coordinator: &ContainmentCoordinator,
    worker: std::thread::JoinHandle<()>,
) {
    coordinator.stop();
    if worker.join().is_err() {
        log::error!("containment reconciler panicked during shutdown");
    }
}

fn candidate_snapshot(
    health_store: &Arc<RwLock<HealthStore>>,
    security_store: &AuditStore,
    case_id: Uuid,
) -> Result<Vec<ContainmentCandidate>, OperationError> {
    let detail = security_store
        .case_detail(case_id)
        .map_err(|error| match error {
            SecurityStoreError::MissingCase(_) => {
                OperationError::Containment(ContainmentError::MissingCase(case_id))
            }
            error => OperationError::Containment(ContainmentError::Store(error)),
        })?;
    let statuses = health_store
        .read()
        .map_err(|_| OperationError::HealthStoreUnavailable)?
        .all_agents();
    trusted_candidates(
        statuses,
        security_store,
        &detail.case.agent_id,
        detail.case.session_id.as_deref(),
    )
    .map_err(ContainmentError::from)
    .map_err(OperationError::Containment)
}

fn trusted_candidates(
    statuses: Vec<AgentHealthStatus>,
    security_store: &AuditStore,
    agent_id: &str,
    session_id: Option<&str>,
) -> Result<Vec<ContainmentCandidate>, SecurityStoreError> {
    let mut candidates = Vec::new();
    for status in statuses {
        if status.status == AgentHealthState::Offline {
            continue;
        }
        let Ok(root_pid) = i32::try_from(status.pid) else {
            continue;
        };
        let Ok(process_start_time) = read_process_start_time(root_pid) else {
            continue;
        };
        if !security_store.process_identity_matches(
            root_pid,
            process_start_time,
            agent_id,
            session_id,
        )? {
            continue;
        }
        let display_name = if status.agent_name.trim().is_empty() {
            agent_id.to_string()
        } else {
            status.agent_name
        };
        candidates.push(ContainmentCandidate {
            agent_id: agent_id.to_string(),
            root_pid,
            process_start_time,
            display_name,
        });
    }
    Ok(resolve_candidate_identities(candidates))
}

fn resolve_candidate_identities(
    mut candidates: Vec<ContainmentCandidate>,
) -> Vec<ContainmentCandidate> {
    candidates.sort_by(|left, right| {
        (
            &left.agent_id,
            left.root_pid,
            left.process_start_time,
            &left.display_name,
        )
            .cmp(&(
                &right.agent_id,
                right.root_pid,
                right.process_start_time,
                &right.display_name,
            ))
    });
    let mut pid_identities = BTreeMap::<i32, BTreeSet<(String, u64)>>::new();
    for candidate in &candidates {
        pid_identities
            .entry(candidate.root_pid)
            .or_default()
            .insert((candidate.agent_id.clone(), candidate.process_start_time));
    }
    candidates.retain(|candidate| {
        pid_identities
            .get(&candidate.root_pid)
            .is_some_and(|identities| identities.len() == 1)
    });
    candidates.dedup_by(|left, right| {
        left.agent_id == right.agent_id
            && left.root_pid == right.root_pid
            && left.process_start_time == right.process_start_time
    });
    candidates
}

fn containment_plan_view(plan: &ContainmentPlan) -> Value {
    json!({
        "case_id": plan.case_id,
        "source_path": plan.source_path,
        "original_target": plan.original_target.as_ref().map(containment_candidate_view),
        "original_target_valid": plan.original_target_valid,
        "candidates": plan.candidates.iter().map(containment_candidate_view).collect::<Vec<_>>(),
        "default_duration_secs": plan.default_duration_secs,
        "min_duration_secs": plan.min_duration_secs,
        "max_duration_secs": plan.max_duration_secs,
        "existing_action": plan.existing_action.as_ref().map(containment_action_view),
    })
}

fn containment_candidate_view(candidate: &ContainmentCandidate) -> Value {
    json!({
        "agent_id": candidate.agent_id,
        "root_pid": candidate.root_pid,
        "process_start_time": candidate.process_start_time,
        "display_name": candidate.display_name,
    })
}

pub(super) fn containment_action_view(action: &ContainmentAction) -> Value {
    json!({
        "action_id": action.action_id,
        "case_id": action.case_id,
        "binding_id": action.binding_id,
        "agent_id": action.agent_id,
        "root_pid": action.root_pid,
        "process_start_time": action.process_start_time,
        "duration_secs": action.duration_secs,
        "expires_at_ns": action.expires_at_ns,
        "lifecycle_state": action.lifecycle_state,
        "blocked_at_ns": action.blocked_at_ns,
        "requested_by": action.requested_by,
        "failure_stage": action.failure_stage,
        "failure_summary": failure_summary(action.lifecycle_state, action.failure_stage),
        "attempt_count": action.attempt_count,
        "next_retry_at_ns": action.next_retry_at_ns,
        "created_at_ns": action.created_at_ns,
        "updated_at_ns": action.updated_at_ns,
    })
}

pub(super) fn case_detail_view(mut detail: Value, action: Option<&ContainmentAction>) -> Value {
    if let Some(object) = detail.as_object_mut() {
        object.insert(
            "containment".into(),
            action.map(containment_action_view).unwrap_or(Value::Null),
        );
    }
    detail
}

fn failure_summary(
    lifecycle: ContainmentLifecycle,
    stage: Option<ContainmentFailureStage>,
) -> Option<&'static str> {
    match stage {
        Some(ContainmentFailureStage::Attach) => {
            Some("策略挂载失败，请确认 Agent 与执行器状态后重试")
        }
        Some(ContainmentFailureStage::Detach) => Some("策略解除失败，请确认执行器状态后重试"),
        Some(ContainmentFailureStage::Reconcile) => {
            Some("策略状态恢复失败，请确认执行器状态后重试")
        }
        None if lifecycle == ContainmentLifecycle::Failed => {
            Some("策略执行失败，请确认 Agent 与执行器状态后重试")
        }
        None => None,
    }
}

fn parse_case_id(value: String) -> Result<Uuid, HttpResponse> {
    Uuid::parse_str(&value).map_err(|_| request_error("case_id must be a UUID"))
}

fn operation_error(error: OperationError) -> HttpResponse {
    match error {
        OperationError::Containment(error) => containment_error(error),
        OperationError::HealthStoreUnavailable => system_audit::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "health_store_unavailable",
            "trusted Agent health is unavailable",
            true,
        ),
    }
}

fn containment_error(error: ContainmentError) -> HttpResponse {
    use ContainmentError::*;
    log::error!("containment request failed: {error}");
    let (status, code, message, retryable) = match error {
        MissingCase(_) => (
            StatusCode::NOT_FOUND,
            "case_not_found",
            "audit case was not found",
            false,
        ),
        SourcePolicyUnavailable(_) => (
            StatusCode::CONFLICT,
            "source_policy_unavailable",
            "source policy is unavailable",
            false,
        ),
        RootProcessStale(_) => (
            StatusCode::CONFLICT,
            "root_process_stale",
            "selected process identity is stale",
            true,
        ),
        AmbiguousCandidate(_) => (
            StatusCode::CONFLICT,
            "ambiguous_candidate",
            "selected process identity is ambiguous",
            false,
        ),
        IneligibleCase { .. } => (
            StatusCode::CONFLICT,
            "case_not_eligible",
            "audit case is not eligible for containment",
            false,
        ),
        InvalidDuration => (
            StatusCode::BAD_REQUEST,
            "invalid_duration",
            "duration must be null or between 60 and 86400 seconds",
            false,
        ),
        InvalidRequestedBy => (
            StatusCode::BAD_REQUEST,
            "invalid_requester",
            "requester identity is invalid",
            false,
        ),
        IncompatibleAction(_) => (
            StatusCode::CONFLICT,
            "incompatible_action",
            "an incompatible containment action is active",
            false,
        ),
        ContainmentInProgress(_) => (
            StatusCode::CONFLICT,
            "action_in_progress",
            "containment action is still in progress",
            true,
        ),
        ContainmentExpiring(_) => (
            StatusCode::CONFLICT,
            "action_expiring",
            "containment action is expiring",
            true,
        ),
        CaseEligibilityChanged { .. } => (
            StatusCode::CONFLICT,
            "case_eligibility_changed",
            "audit case eligibility changed",
            false,
        ),
        CleanupRequired { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "cleanup_required",
            "containment cleanup is required",
            true,
        ),
        Enforcer(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "enforcer_unavailable",
            "enforcement service is unavailable",
            true,
        ),
        Store(error) => {
            // Same classification as the audit handlers: data corruption
            // must not be reported as store unavailability.
            return system_audit::store_error(error);
        }
        AlreadyRunning => (
            StatusCode::CONFLICT,
            "reconciler_already_running",
            "containment reconciler is already running",
            true,
        ),
        ReconcilerThread(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "reconciler_unavailable",
            "containment reconciler is unavailable",
            true,
        ),
        RecoveryFailed { .. } => (
            StatusCode::CONFLICT,
            "recovery_failed",
            "containment recovery failed",
            false,
        ),
        ClaimLost(_) => (
            StatusCode::CONFLICT,
            "claim_lost",
            "containment lifecycle claim was lost",
            true,
        ),
        CorruptActions { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_actions",
            "stored containment actions are invalid",
            true,
        ),
    };
    system_audit::error_response(status, code, message, retryable)
}

fn unavailable() -> HttpResponse {
    system_audit::error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "containment_disabled",
        "containment coordinator is not configured",
        true,
    )
}

fn request_error(message: &str) -> HttpResponse {
    system_audit::error_response(StatusCode::BAD_REQUEST, "bad_request", message, false)
}

fn blocking_error(error: actix_web::error::BlockingError) -> HttpResponse {
    log::error!("containment blocking worker failed: {error}");
    system_audit::error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "blocking_worker_failed",
        "containment worker failed",
        true,
    )
}

#[cfg(test)]
#[path = "containment_error_tests.rs"]
mod error_tests;

#[cfg(test)]
#[path = "containment_candidate_tests.rs"]
mod candidate_tests;

#[cfg(test)]
#[path = "containment_tests.rs"]
mod tests;
