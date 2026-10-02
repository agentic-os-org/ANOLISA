use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use actix_web::http::StatusCode;
use actix_web::test as awtest;
use actix_web::{App, web};
use agentsight_enforcement_protocol::{
    Binding, BindingState, CredentialExfiltrationPolicy, CredentialPolicySnapshot,
    DestinationScope, EventIdentity, FileAction, PolicyMode, ReplacePolicy, ReplacementPolicy,
    SecurityEvent, SecurityEventKind,
};
use serde_json::Value;
use uuid::Uuid;

use super::super::auth::{AuthMiddleware, DashboardAuth};
use super::super::{AppState, SecurityObservabilityConfig, configure_routes};
use super::{
    case_detail_view, failure_summary, start_reconciler, stop_reconciler, trusted_candidates,
};
use crate::config::ServerAuthConfig;
use crate::enforcement::{
    ApplyPolicy, TransitionDirection, TransitionKey, read_process_start_time,
};
use crate::grader::EvaluationStore;
use crate::health::store::AgentRole;
use crate::health::{AgentHealthState, AgentHealthStatus, HealthStore};
use crate::security::{
    ContainmentAction, ContainmentCoordinator, ContainmentEnforcer, ContainmentEnforcerError,
    ContainmentError, ContainmentFailureStage, ContainmentLifecycle, ContainmentReadinessLease,
    ContainmentReadinessStamp, RiskCase, RiskCaseStatus, RiskSeverity, SecurityStore,
    StampedBinding, StampedBindings, stable_readiness_lease,
};

#[derive(Default)]
struct FakeEnforcer {
    bindings: Mutex<Vec<Binding>>,
    policy_snapshots: Mutex<HashMap<Uuid, CredentialPolicySnapshot>>,
    transitions: Mutex<HashMap<TransitionKey, (ReplacePolicy, Binding)>>,
    apply_count: AtomicUsize,
}

impl ContainmentEnforcer for FakeEnforcer {
    fn foreground_claim_lease(&self) -> std::time::Duration {
        std::time::Duration::from_secs(20)
    }

    fn begin_transition(
        &self,
        key: TransitionKey,
        transition: ReplacePolicy,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        self.apply_count.fetch_add(1, Ordering::AcqRel);
        let binding = match transition.replacement.clone() {
            ReplacementPolicy::Credential(request) => {
                self.policy_snapshots
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        request.binding_id,
                        CredentialPolicySnapshot::capture(request.policy.clone())
                            .expect("fixture target policy should be valid"),
                    );
                let source = request
                    .policy
                    .source_patterns
                    .first()
                    .cloned()
                    .unwrap_or_default();
                Binding {
                    request: ApplyPolicy {
                        binding_id: request.binding_id,
                        agent_id: request.agent_id,
                        session_id: request.session_id,
                        root_pid: request.root_pid,
                        process_start_time: request.process_start_time,
                        policy_id: request.policy.policy_id,
                        policy_revision: request.policy.revision.to_string(),
                        policy_dsl: policy("block", &source),
                        policy_mode: Some(request.policy.mode),
                    },
                    state: BindingState::Enforced,
                    message: None,
                    domain_id: Some(1),
                }
            }
            ReplacementPolicy::Generic(request) => Binding {
                request,
                state: BindingState::Enforced,
                message: None,
                domain_id: transition.expected.domain_id.or(Some(1)),
            },
        };
        let mut bindings = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for existing in bindings.iter_mut() {
            if existing.request.binding_id == transition.expected.request.binding_id {
                existing.state = BindingState::Detached;
                existing.domain_id = None;
            }
        }
        match bindings
            .iter_mut()
            .find(|existing| existing.request.binding_id == binding.request.binding_id)
        {
            Some(existing) => *existing = binding.clone(),
            None => bindings.push(binding.clone()),
        }
        drop(bindings);
        self.transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, (transition, binding.clone()));
        Ok(StampedBinding::stable(binding))
    }

    fn resume_transition(
        &self,
        key: &TransitionKey,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        self.transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .map(|(_, binding)| binding.clone())
            .map(StampedBinding::stable)
            .ok_or(ContainmentEnforcerError::MissingTransition(key.action_id))
    }

    fn begin_reverse_transition(
        &self,
        action_id: Uuid,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        let forward_key = TransitionKey {
            action_id,
            direction: TransitionDirection::Forward,
        };
        let (request, acknowledgement) = self
            .transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&forward_key)
            .cloned()
            .ok_or(ContainmentEnforcerError::MissingTransition(action_id))?;
        self.begin_transition(
            TransitionKey {
                action_id,
                direction: TransitionDirection::Reverse,
            },
            request.reverse(acknowledgement),
        )
    }

    fn detach(&self, _: Uuid) -> Result<(), String> {
        Ok(())
    }

    fn bindings(&self) -> Result<StampedBindings, ContainmentEnforcerError> {
        Ok(StampedBindings::stable(
            self.bindings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        ))
    }

    fn credential_policy_snapshot(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, ContainmentEnforcerError> {
        Ok(self
            .policy_snapshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&binding_id)
            .cloned())
    }

    fn lease_ready(
        &self,
        _: ContainmentReadinessStamp,
    ) -> Result<Box<dyn ContainmentReadinessLease + '_>, ContainmentEnforcerError> {
        Ok(stable_readiness_lease())
    }
}

struct ApiFixture {
    case_id: Uuid,
    child: Child,
    store: Arc<SecurityStore>,
    health: Arc<RwLock<HealthStore>>,
    coordinator: Arc<ContainmentCoordinator>,
    enforcer: Arc<FakeEnforcer>,
    auth: Arc<DashboardAuth>,
    auth_dir: PathBuf,
}

impl ApiFixture {
    fn new(auth_enabled: bool) -> Self {
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("fixture process should start");
        let live_pid = child.id() as i32;
        let health = Arc::new(RwLock::new(HealthStore::new()));
        health
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update(
                live_pid as u32,
                health_status(live_pid as u32, "Hermes Display"),
            );

        let store = Arc::new(SecurityStore::open_in_memory().expect("fixture store should open"));
        let enforcer = Arc::new(FakeEnforcer::default());
        let source_binding_id = Uuid::new_v4();
        enforcer
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(source_binding(source_binding_id));
        enforcer
            .policy_snapshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                source_binding_id,
                CredentialPolicySnapshot::capture(source_policy())
                    .expect("fixture source policy should be valid"),
            );
        let event = evidence(source_binding_id);
        store.insert_event(&event).expect("evidence should persist");
        let mut process_event = evidence(Uuid::new_v4());
        process_event.event_id = Uuid::new_v4();
        process_event.identity.pid = live_pid;
        process_event.identity.process_start_time =
            read_process_start_time(live_pid).expect("fixture process should be readable");
        store
            .insert_event(&process_event)
            .expect("process identity should persist");
        let case_id = Uuid::new_v4();
        store
            .upsert_case(&risk_case(case_id), &[event.event_id])
            .expect("case should persist");
        let enforcer_trait: Arc<dyn ContainmentEnforcer> = enforcer.clone();
        let coordinator = Arc::new(ContainmentCoordinator::new(
            Arc::clone(&store),
            enforcer_trait,
        ));

        let auth_dir = std::env::temp_dir().join(format!("containment-auth-{case_id}"));
        std::fs::create_dir_all(&auth_dir).expect("auth fixture directory should exist");
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig {
                enabled: auth_enabled,
            },
            &auth_dir,
        ));
        Self {
            case_id,
            child,
            store,
            health,
            coordinator,
            enforcer,
            auth,
            auth_dir,
        }
    }

    fn data(&self) -> web::Data<AppState> {
        self.data_with_containment(Some(Arc::clone(&self.coordinator)))
    }

    fn data_with_containment(
        &self,
        containment: Option<Arc<ContainmentCoordinator>>,
    ) -> web::Data<AppState> {
        web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: PathBuf::from(":memory:"),
            genai_store: None,
            start_time: Instant::now(),
            health_store: Arc::clone(&self.health),
            interruption_store: None,
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(std::path::Path::new(":memory:"))
                    .expect("evaluation fixture should open"),
            ),
            enforcement: None,
            containment,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                self.store.audit_store(),
            )),
            security_observability: SecurityObservabilityConfig::default(),
            auth: Arc::clone(&self.auth),
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    fn contain_uri(&self) -> String {
        format!("/api/audit/cases/{}/contain", self.case_id)
    }
}

impl Drop for ApiFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.auth_dir);
    }
}

#[actix_web::test]
async fn plan_uses_live_health_and_omits_sensitive_policy_fields() {
    let fixture = ApiFixture::new(false);
    let app = awtest::init_service(
        App::new()
            .app_data(fixture.data())
            .configure(configure_routes),
    )
    .await;

    let request = awtest::TestRequest::get()
        .uri(&format!(
            "/api/audit/cases/{}/containment-plan",
            fixture.case_id
        ))
        .to_request();
    let response = awtest::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = awtest::read_body(response).await;
    let value: Value = serde_json::from_slice(&body).expect("plan should be JSON");
    assert_eq!(value["data"]["original_target_valid"], false);
    assert_eq!(
        value["data"]["candidates"][0]["root_pid"],
        fixture.child.id()
    );
    assert_eq!(value["data"]["candidates"][0]["agent_id"], "hermes-test");
    assert_eq!(
        value["data"]["candidates"][0]["display_name"],
        "Hermes Display"
    );
    assert_eq!(value["data"]["source_path"], "/root/secret.txt");
    let text = String::from_utf8_lossy(&body);
    assert!(!text.contains("policy_dsl"));
}

#[actix_web::test]
async fn post_rebuilds_candidates_instead_of_trusting_the_plan() {
    let fixture = ApiFixture::new(false);
    let app = awtest::init_service(
        App::new()
            .app_data(fixture.data())
            .configure(configure_routes),
    )
    .await;
    fixture
        .health
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove_by_pid(fixture.child.id());

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri(&fixture.contain_uri())
            .set_json(serde_json::json!({
                "root_pid": fixture.child.id(),
                "duration_secs": 60,
            }))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let value: Value = awtest::read_body_json(response).await;
    assert_eq!(value["error"]["code"], "root_process_stale");
}

#[actix_web::test]
async fn unknown_case_is_not_found_for_plan_and_contain_endpoints() {
    let fixture = ApiFixture::new(false);
    let missing_case_id = Uuid::new_v4();
    let app = awtest::init_service(
        App::new()
            .app_data(fixture.data())
            .configure(configure_routes),
    )
    .await;

    let requests = [
        awtest::TestRequest::get()
            .uri(&format!(
                "/api/audit/cases/{missing_case_id}/containment-plan"
            ))
            .to_request(),
        awtest::TestRequest::post()
            .uri(&format!("/api/audit/cases/{missing_case_id}/contain"))
            .set_json(serde_json::json!({
                "root_pid": fixture.child.id(),
                "duration_secs": 60,
            }))
            .to_request(),
    ];

    for request in requests {
        let response = awtest::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value: Value = awtest::read_body_json(response).await;
        assert_eq!(value["error"]["code"], "case_not_found");
    }
}

#[actix_web::test]
async fn post_requires_duration_field_even_for_persistent_containment() {
    let fixture = ApiFixture::new(false);
    let app = awtest::init_service(
        App::new()
            .app_data(fixture.data())
            .configure(configure_routes),
    )
    .await;

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri(&fixture.contain_uri())
            .set_json(serde_json::json!({ "root_pid": fixture.child.id() }))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        fixture
            .store
            .latest_containment_action(fixture.case_id)
            .expect("action query should work")
            .is_none()
    );
}

#[actix_web::test]
async fn post_distinguishes_persistent_and_temporary_durations() {
    for (duration, expected) in [
        (serde_json::Value::Null, None),
        (serde_json::json!(60), Some(60)),
    ] {
        let fixture = ApiFixture::new(false);
        let app = awtest::init_service(
            App::new()
                .app_data(fixture.data())
                .configure(configure_routes),
        )
        .await;
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri(&fixture.contain_uri())
                .set_json(serde_json::json!({
                    "root_pid": fixture.child.id(),
                    "duration_secs": duration,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let stored = fixture
            .store
            .latest_containment_action(fixture.case_id)
            .expect("action query should work")
            .expect("action should persist");
        assert_eq!(stored.duration_secs, expected);
    }
}

#[actix_web::test]
async fn active_post_is_idempotent_and_persists_only_dashboard_principal() {
    let fixture = ApiFixture::new(true);
    let token = fixture.auth.token().unwrap_or_default().to_string();
    let app = awtest::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Arc::clone(&fixture.auth)))
            .app_data(fixture.data())
            .configure(configure_routes),
    )
    .await;
    let mut action_id = None;
    for _ in 0..2 {
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri(&fixture.contain_uri())
                .insert_header(("Authorization", format!("Bearer {token}")))
                .set_json(serde_json::json!({
                    "root_pid": fixture.child.id(),
                    "duration_secs": 60,
                }))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value = awtest::read_body_json(response).await;
        assert_eq!(value["state"], "policy_active");
        assert_eq!(value["data"]["requested_by"], "dashboard-token");
        assert_eq!(value["data"]["lifecycle_state"], "active");
        let observed = value["data"]["action_id"].clone();
        assert!(
            action_id
                .as_ref()
                .is_none_or(|expected| expected == &observed)
        );
        action_id = Some(observed);
    }
    assert_eq!(fixture.enforcer.apply_count.load(Ordering::Acquire), 1);
    let stored = fixture
        .store
        .latest_containment_action(fixture.case_id)
        .expect("action query should work")
        .expect("action should persist");
    assert_eq!(stored.requested_by, "dashboard-token");
    assert!(!stored.requested_by.contains(&token));
    fixture
        .store
        .mark_containment_blocked(stored.binding_id, stored.updated_at_ns.saturating_add(1))
        .expect("block correlation should persist");
    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri(&fixture.contain_uri())
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "root_pid": fixture.child.id(),
                "duration_secs": 60,
            }))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = awtest::read_body_json(response).await;
    assert_eq!(value["state"], "contained");
}

#[actix_web::test]
async fn unavailable_and_invalid_requests_use_json_error_envelopes() {
    let fixture = ApiFixture::new(false);
    let data = fixture.data_with_containment(None);
    let app = awtest::init_service(App::new().app_data(data).configure(configure_routes)).await;

    for (request, expected_status) in [
        (
            awtest::TestRequest::get()
                .uri("/api/audit/cases/not-a-uuid/containment-plan")
                .to_request(),
            StatusCode::BAD_REQUEST,
        ),
        (
            awtest::TestRequest::get()
                .uri(&format!(
                    "/api/audit/cases/{}/containment-plan",
                    fixture.case_id
                ))
                .to_request(),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            awtest::TestRequest::post()
                .uri(&fixture.contain_uri())
                .insert_header(("content-type", "application/json"))
                .set_payload("{")
                .to_request(),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = awtest::call_service(&app, request).await;
        assert_eq!(response.status(), expected_status);
        let value: Value = awtest::read_body_json(response).await;
        assert!(value["error"]["code"].is_string());
        assert!(value["error"]["retryable"].is_boolean());
    }
}

#[test]
fn trusted_health_candidates_are_filtered_and_deterministic() {
    let mut first = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("first process");
    let mut second = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("second process");
    let mut statuses = vec![
        health_status(first.id(), "z-agent"),
        health_status(second.id(), "a-agent"),
        health_status(1, "protected"),
        health_status(first.id(), ""),
    ];
    statuses.push(statuses[0].clone());
    let mut offline = health_status(second.id(), "offline");
    offline.status = AgentHealthState::Offline;
    statuses.push(offline);

    let store = SecurityStore::open_in_memory().expect("fixture store should open");
    for (pid, agent_id) in [
        (first.id() as i32, "z-agent"),
        (second.id() as i32, "a-agent"),
    ] {
        let mut event = evidence(Uuid::new_v4());
        event.event_id = Uuid::new_v4();
        event.identity.pid = pid;
        event.identity.process_start_time =
            read_process_start_time(pid).expect("fixture process should be readable");
        event.identity.agent_id = agent_id.into();
        store.insert_event(&event).expect("identity should persist");
    }

    let mut candidates = trusted_candidates(statuses, &store, "a-agent", Some("session-1"))
        .expect("trusted candidates should resolve");
    candidates.extend(
        trusted_candidates(
            vec![health_status(first.id(), "z-agent")],
            &store,
            "z-agent",
            Some("session-1"),
        )
        .expect("trusted candidates should resolve"),
    );
    candidates.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].agent_id, "a-agent");
    assert_eq!(candidates[1].agent_id, "z-agent");
    let _ = first.kill();
    let _ = first.wait();
    let _ = second.kill();
    let _ = second.wait();
}

#[test]
fn reconciler_lifecycle_stops_joins_and_allows_restart() {
    let store = Arc::new(SecurityStore::open_in_memory().expect("fixture store should open"));
    let enforcer: Arc<dyn ContainmentEnforcer> = Arc::new(FakeEnforcer::default());
    let coordinator = ContainmentCoordinator::new(store, enforcer);
    let first = start_reconciler(&coordinator).expect("worker should start");
    assert!(matches!(
        start_reconciler(&coordinator),
        Err(ContainmentError::AlreadyRunning)
    ));
    stop_reconciler(&coordinator, first);
    let second = start_reconciler(&coordinator).expect("worker should restart");
    stop_reconciler(&coordinator, second);
}

#[test]
fn failure_summaries_are_stable_and_do_not_include_internal_details() {
    let cases = [
        (
            ContainmentLifecycle::Failed,
            Some(ContainmentFailureStage::Attach),
            Some("策略挂载失败，请确认 Agent 与执行器状态后重试"),
        ),
        (
            ContainmentLifecycle::Expiring,
            Some(ContainmentFailureStage::Detach),
            Some("策略解除失败，请确认执行器状态后重试"),
        ),
        (
            ContainmentLifecycle::Pending,
            Some(ContainmentFailureStage::Reconcile),
            Some("策略状态恢复失败，请确认执行器状态后重试"),
        ),
        (
            ContainmentLifecycle::Failed,
            None,
            Some("策略执行失败，请确认 Agent 与执行器状态后重试"),
        ),
        (ContainmentLifecycle::Active, None, None),
    ];
    for (lifecycle, stage, expected) in cases {
        assert_eq!(failure_summary(lifecycle, stage), expected);
    }
}

#[test]
fn empty_case_projection_has_an_explicit_containment_field() {
    let view = case_detail_view(serde_json::json!({ "case_id": Uuid::nil() }), None);
    assert!(view["containment"].is_null());
}

fn action_with_failure(
    lifecycle_state: ContainmentLifecycle,
    failure_stage: ContainmentFailureStage,
) -> ContainmentAction {
    ContainmentAction {
        action_id: Uuid::new_v4(),
        case_id: Uuid::nil(),
        binding_id: Uuid::new_v4(),
        source_binding_id: Some(Uuid::new_v4()),
        agent_id: "hermes-test".into(),
        root_pid: 42,
        process_start_time: 7,
        source_path: "/root/private-credential".into(),
        duration_secs: Some(900),
        expires_at_ns: None,
        lifecycle_state,
        blocked_at_ns: None,
        requested_by: "dashboard".into(),
        failure_stage: Some(failure_stage),
        failure_reason: Some("internal socket /run/private.sock policy_dsl".into()),
        attempt_count: 2,
        next_retry_at_ns: None,
        created_at_ns: 1,
        updated_at_ns: 2,
    }
}

#[test]
fn case_projection_exposes_only_sanitized_containment() {
    let action = action_with_failure(
        ContainmentLifecycle::Pending,
        ContainmentFailureStage::Reconcile,
    );
    let rendered =
        case_detail_view(serde_json::json!({ "status": "resolved" }), Some(&action)).to_string();
    assert!(rendered.contains("策略状态恢复失败"));
    for secret in [
        action.source_path.as_str(),
        "/run/private.sock",
        "policy_dsl",
    ] {
        assert!(!rendered.contains(secret));
    }
}

fn health_status(pid: u32, agent_name: &str) -> AgentHealthStatus {
    AgentHealthStatus {
        pid,
        agent_name: agent_name.into(),
        category: "test".into(),
        exe_path: "/usr/bin/sleep".into(),
        workspace_path: Some("/workspace".into()),
        ports: Vec::new(),
        status: AgentHealthState::Healthy,
        last_check_time: 1,
        latency_ms: None,
        error_message: None,
        restart_cmd: None,
        offline_since: None,
        role: AgentRole::Client,
        parent_pid: None,
        has_crash: false,
    }
}

fn source_binding(binding_id: Uuid) -> Binding {
    Binding {
        request: ApplyPolicy {
            binding_id,
            agent_id: "hermes-test".into(),
            session_id: Some("session-1".into()),
            root_pid: 999_999,
            process_start_time: 42,
            policy_id: "credential-exfiltration".into(),
            policy_revision: "3".into(),
            policy_dsl: policy("notify", "/root/secret.txt"),
            policy_mode: Some(PolicyMode::Audit),
        },
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(1),
    }
}

fn source_policy() -> CredentialExfiltrationPolicy {
    CredentialExfiltrationPolicy {
        policy_id: "credential-exfiltration".into(),
        revision: 3,
        source_patterns: vec!["/root/secret.txt".into()],
        trusted_endpoints: vec!["trusted.example:443".into()],
        taint_label: "CREDENTIAL".into(),
        taint_ttl_secs: 300,
        destination_scope: DestinationScope::PublicIpv4,
        mode: PolicyMode::Audit,
    }
}

fn evidence(binding_id: Uuid) -> SecurityEvent {
    SecurityEvent {
        event_id: Uuid::new_v4(),
        occurred_at_ns: 1,
        observed_at_ns: 1,
        identity: EventIdentity {
            binding_id,
            agent_id: "hermes-test".into(),
            agent_name: Some("Hermes test".into()),
            session_id: Some("session-1".into()),
            conversation_id: None,
            tool_call_id: None,
            pid: 999_999,
            process_start_time: 42,
            ppid: None,
            cgroup_id: None,
            protocol_version: 1,
            enforcer_version: "test".into(),
            actplane_revision: "test".into(),
        },
        kind: SecurityEventKind::FileAction(FileAction {
            policy_id: "credential-exfiltration".into(),
            policy_revision: 3,
            operation: "read".into(),
            path: "~/redacted-secret".into(),
            resource_class: "credential".into(),
            succeeded: true,
            errno: None,
            rule_id: None,
        }),
    }
}

fn risk_case(case_id: Uuid) -> RiskCase {
    RiskCase {
        case_id,
        correlation_key: format!("case-{case_id}"),
        policy_id: "credential-exfiltration".into(),
        policy_revision: 3,
        agent_id: "hermes-test".into(),
        session_id: Some("session-1".into()),
        severity: RiskSeverity::High,
        risk_score: 85,
        status: RiskCaseStatus::Open,
        blocked: false,
        opened_at_ns: 1,
        updated_at_ns: 1,
        summary: "credential reached an untrusted target".into(),
    }
}

fn policy(action: &str, source: &str) -> String {
    format!(
        "source AGENT = exec \"**\"\nsource CREDENTIAL = file \"{source}\"\nrule agentsight-credential-exfiltration:\n  {action} connect endpoint \"*\" if CREDENTIAL unless target \"trusted.example:443\"\n  because \"credential-derived data reached an untrusted network target\"\n"
    )
}
