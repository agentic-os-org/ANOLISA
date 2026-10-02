#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use agentsight::enforcement::{
    ApplyPolicy, Binding, BindingState, TransitionDirection, TransitionKey,
};
use agentsight::security::{
    ContainmentAction, ContainmentCandidate, ContainmentCoordinator, ContainmentEnforcer,
    ContainmentEnforcerError, ContainmentError, ContainmentFailureStage, ContainmentLifecycle,
    ContainmentReadinessLease, ContainmentReadinessStamp, ContainmentRequest, RiskCase,
    RiskCaseStatus, RiskSeverity, SecurityStore, StampedBinding, StampedBindings,
    stable_readiness_lease,
};
use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, CredentialExfiltrationPolicy, CredentialPolicySnapshot,
    DestinationScope, EventIdentity, FileAction, PolicyMode, ReplacePolicy, ReplacementPolicy,
    ReplacementSource, SecurityEvent, SecurityEventKind,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
enum AckMutation {
    State(BindingState),
    Session,
    Source,
    TrustedEndpoint,
    Notify,
}

struct ApplyPause {
    entered: Barrier,
    resume: Barrier,
}

#[derive(Default)]
struct FakeEnforcer {
    bindings: Mutex<Vec<Binding>>,
    policy_snapshots: Mutex<HashMap<Uuid, CredentialPolicySnapshot>>,
    transitions: Mutex<HashMap<TransitionKey, (ReplacePolicy, Binding)>>,
    applied: Mutex<Vec<ApplyCredentialPolicy>>,
    failure: Mutex<Option<ContainmentEnforcerError>>,
    detach_failure: Mutex<Option<String>>,
    acknowledgement: Mutex<Option<AckMutation>>,
    pause: Mutex<Option<Arc<ApplyPause>>>,
    bindings_pause: Mutex<Option<Arc<ApplyPause>>>,
    bindings_failure: Mutex<Option<String>>,
    detached: Mutex<Vec<Uuid>>,
    apply_calls: AtomicUsize,
    panic_bindings: AtomicBool,
}

impl FakeEnforcer {
    fn set_bindings(&self, bindings: Vec<Binding>) {
        *self.bindings.lock().expect("bindings should lock") = bindings;
    }

    fn set_source_policy(&self, binding_id: Uuid, policy: CredentialExfiltrationPolicy) {
        let snapshot = CredentialPolicySnapshot::capture(policy)
            .expect("fixture credential policy should be valid");
        self.policy_snapshots
            .lock()
            .expect("policy snapshots should lock")
            .insert(binding_id, snapshot);
    }

    fn fail_next_apply(&self, message: &str) {
        *self.failure.lock().expect("failure should lock") =
            Some(ContainmentEnforcerError::Unavailable(message.into()));
    }

    fn reject_next_apply(&self, message: &str) {
        *self.failure.lock().expect("failure should lock") =
            Some(ContainmentEnforcerError::Rejected(message.into()));
    }

    fn fail_next_detach(&self, message: &str) {
        *self
            .detach_failure
            .lock()
            .expect("detach failure should lock") = Some(message.into());
    }

    fn fail_next_bindings(&self, message: &str) {
        *self
            .bindings_failure
            .lock()
            .expect("bindings failure should lock") = Some(message.into());
    }

    fn mutate_ack(&self, mutation: AckMutation) {
        *self.acknowledgement.lock().expect("ack should lock") = Some(mutation);
    }

    fn pause_apply(&self) -> Arc<ApplyPause> {
        let pause = Arc::new(ApplyPause {
            entered: Barrier::new(2),
            resume: Barrier::new(2),
        });
        *self.pause.lock().expect("pause should lock") = Some(Arc::clone(&pause));
        pause
    }

    fn pause_bindings(&self) -> Arc<ApplyPause> {
        let pause = Arc::new(ApplyPause {
            entered: Barrier::new(2),
            resume: Barrier::new(2),
        });
        *self.bindings_pause.lock().expect("pause should lock") = Some(Arc::clone(&pause));
        pause
    }

    fn panic_next_bindings(&self) {
        self.panic_bindings.store(true, Ordering::Release);
    }

    fn apply_calls(&self) -> usize {
        self.apply_calls.load(Ordering::Acquire)
    }

    fn applied(&self) -> Vec<ApplyCredentialPolicy> {
        self.applied.lock().expect("applied should lock").clone()
    }

    fn detached(&self) -> Vec<Uuid> {
        self.detached.lock().expect("detached should lock").clone()
    }

    fn detach_calls(&self) -> usize {
        self.detached.lock().expect("detached should lock").len()
    }

    fn binding_state(&self, binding_id: Uuid) -> Option<BindingState> {
        self.bindings
            .lock()
            .expect("bindings should lock")
            .iter()
            .find(|binding| binding.request.binding_id == binding_id)
            .map(|binding| binding.state)
    }

    fn binding(&self, binding_id: Uuid) -> Option<Binding> {
        self.bindings
            .lock()
            .expect("bindings should lock")
            .iter()
            .find(|binding| binding.request.binding_id == binding_id)
            .cloned()
    }

    fn seed_completed_forward(&self, action: &ContainmentAction, target: Binding) {
        let mut bindings = self.bindings.lock().expect("bindings should lock");
        let source = bindings
            .iter_mut()
            .find(|binding| Some(binding.request.binding_id) == action.source_binding_id)
            .expect("source binding should exist");
        let mut expected = source.clone();
        source.state = BindingState::Detached;
        source.domain_id = None;
        expected.state = BindingState::Enforced;
        match bindings
            .iter_mut()
            .find(|binding| binding.request.binding_id == target.request.binding_id)
        {
            Some(binding) => *binding = target.clone(),
            None => bindings.push(target.clone()),
        }
        drop(bindings);
        self.transitions
            .lock()
            .expect("transitions should lock")
            .insert(
                TransitionKey {
                    action_id: action.action_id,
                    direction: TransitionDirection::Forward,
                },
                (
                    ReplacePolicy {
                        expected,
                        source: ReplacementSource::Credential(
                            self.policy_snapshots
                                .lock()
                                .expect("policy snapshots should lock")
                                .get(
                                    &action
                                        .source_binding_id
                                        .expect("source binding should exist"),
                                )
                                .cloned()
                                .expect("source policy snapshot should exist"),
                        ),
                        replacement: ReplacementPolicy::Generic(target.request.clone()),
                    },
                    target,
                ),
            );
    }

    fn clear_transitions(&self) {
        self.transitions
            .lock()
            .expect("transitions should lock")
            .clear();
    }
}

impl ContainmentEnforcer for FakeEnforcer {
    fn foreground_claim_lease(&self) -> Duration {
        Duration::from_secs(80)
    }

    fn begin_transition(
        &self,
        key: TransitionKey,
        transition: ReplacePolicy,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        self.apply_calls.fetch_add(1, Ordering::AcqRel);
        transition
            .validate()
            .map_err(|error| ContainmentEnforcerError::Rejected(error.to_string()))?;
        if let Some(error) = self.failure.lock().expect("failure should lock").take() {
            return Err(error);
        }
        let durable_request = transition.clone();
        let ReplacementPolicy::Credential(request) = transition.replacement else {
            let ReplacementPolicy::Generic(request) = durable_request.replacement.clone() else {
                unreachable!("replacement variants are exhaustive");
            };
            let binding = Binding {
                request,
                state: BindingState::Enforced,
                message: None,
                domain_id: transition.expected.domain_id.or(Some(1)),
            };
            self.finish_transition(key, durable_request, binding.clone());
            return Ok(StampedBinding::stable(binding));
        };
        self.applied
            .lock()
            .expect("applied should lock")
            .push(request.clone());
        let pause = self.pause.lock().expect("pause should lock").take();
        if let Some(pause) = pause {
            pause.entered.wait();
            pause.resume.wait();
        }
        let mutation = self.acknowledgement.lock().expect("ack should lock").take();
        let state = match mutation {
            Some(AckMutation::State(state)) => state,
            _ => BindingState::Enforced,
        };
        let session_id = match mutation {
            Some(AckMutation::Session) => Some("session-other".into()),
            _ => request.session_id,
        };
        let source = match mutation {
            Some(AckMutation::Source) => "/root/other.txt",
            _ => &request.policy.source_patterns[0],
        };
        let trusted = match mutation {
            Some(AckMutation::TrustedEndpoint) => Some("other.example:443"),
            _ => request.policy.trusted_endpoints.first().map(String::as_str),
        };
        let action = if matches!(mutation, Some(AckMutation::Notify)) {
            "notify"
        } else {
            "block"
        };
        let binding = Binding {
            request: ApplyPolicy {
                binding_id: request.binding_id,
                agent_id: request.agent_id,
                session_id,
                root_pid: request.root_pid,
                process_start_time: request.process_start_time,
                policy_id: request.policy.policy_id,
                policy_revision: request.policy.revision.to_string(),
                policy_dsl: compiled_policy(action, source, trusted),
                policy_mode: Some(request.policy.mode),
            },
            state,
            message: None,
            domain_id: Some(7),
        };
        self.finish_transition(key, durable_request, binding.clone());
        Ok(StampedBinding::stable(binding))
    }

    fn resume_transition(
        &self,
        key: &TransitionKey,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        self.transitions
            .lock()
            .expect("transitions should lock")
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
        let (forward_request, forward_acknowledgement) = self
            .transitions
            .lock()
            .expect("transitions should lock")
            .get(&forward_key)
            .cloned()
            .ok_or(ContainmentEnforcerError::MissingTransition(action_id))?;
        self.detached
            .lock()
            .expect("reverse attempts should lock")
            .push(forward_acknowledgement.request.binding_id);
        if let Some(message) = self
            .detach_failure
            .lock()
            .expect("reverse failure should lock")
            .take()
        {
            return Err(ContainmentEnforcerError::Unavailable(message));
        }
        self.begin_transition(
            TransitionKey {
                action_id,
                direction: TransitionDirection::Reverse,
            },
            forward_request.reverse(forward_acknowledgement),
        )
    }

    fn detach(&self, binding_id: Uuid) -> Result<(), String> {
        self.detached
            .lock()
            .expect("detached should lock")
            .push(binding_id);
        match self
            .detach_failure
            .lock()
            .expect("detach failure should lock")
            .take()
        {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    fn bindings(&self) -> Result<StampedBindings, ContainmentEnforcerError> {
        assert!(
            !self.panic_bindings.swap(false, Ordering::AcqRel),
            "test-only bindings panic"
        );
        if let Some(message) = self
            .bindings_failure
            .lock()
            .expect("bindings failure should lock")
            .take()
        {
            return Err(ContainmentEnforcerError::Unavailable(message));
        }
        let pause = self
            .bindings_pause
            .lock()
            .expect("bindings pause should lock")
            .take();
        if let Some(pause) = pause {
            pause.entered.wait();
            pause.resume.wait();
        }
        Ok(StampedBindings::stable(
            self.bindings.lock().expect("bindings should lock").clone(),
        ))
    }

    fn credential_policy_snapshot(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, ContainmentEnforcerError> {
        Ok(self
            .policy_snapshots
            .lock()
            .expect("policy snapshots should lock")
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

impl FakeEnforcer {
    fn finish_transition(&self, key: TransitionKey, request: ReplacePolicy, binding: Binding) {
        if let ReplacementPolicy::Credential(target) = &request.replacement {
            self.set_source_policy(binding.request.binding_id, target.policy.clone());
        }
        let mut bindings = self.bindings.lock().expect("bindings should lock");
        for existing in bindings.iter_mut() {
            if existing.request.binding_id == request.expected.request.binding_id {
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
        self.transitions
            .lock()
            .expect("transitions should lock")
            .insert(key, (request, binding));
    }
}

struct Fixture {
    case_id: Uuid,
    binding_id: Uuid,
    store: Arc<SecurityStore>,
    enforcer: Arc<FakeEnforcer>,
    coordinator: Arc<ContainmentCoordinator>,
}

impl Fixture {
    fn new(source_policy: Option<&str>, root_pid: i32, process_start_time: u64) -> Self {
        let store = Arc::new(SecurityStore::open_in_memory().expect("fixture store should open"));
        Self::with_store(store, source_policy, root_pid, process_start_time)
    }

    fn at_path(
        path: &std::path::Path,
        source_policy: Option<&str>,
        root_pid: i32,
        process_start_time: u64,
    ) -> Self {
        let store = Arc::new(SecurityStore::open(path).expect("fixture store should open"));
        Self::with_store(store, source_policy, root_pid, process_start_time)
    }

    fn with_identities(
        source_policy: Option<&str>,
        root_pid: i32,
        process_start_time: u64,
        evidence_pid: i32,
        evidence_start_time: u64,
    ) -> Self {
        let store = Arc::new(SecurityStore::open_in_memory().expect("fixture store should open"));
        Self::with_store_and_evidence(
            store,
            source_policy,
            root_pid,
            process_start_time,
            evidence_pid,
            evidence_start_time,
        )
    }

    fn with_store(
        store: Arc<SecurityStore>,
        source_policy: Option<&str>,
        root_pid: i32,
        process_start_time: u64,
    ) -> Self {
        Self::with_store_and_evidence(
            store,
            source_policy,
            root_pid,
            process_start_time,
            root_pid,
            process_start_time,
        )
    }

    fn with_store_and_evidence(
        store: Arc<SecurityStore>,
        source_policy: Option<&str>,
        root_pid: i32,
        process_start_time: u64,
        evidence_pid: i32,
        evidence_start_time: u64,
    ) -> Self {
        let enforcer = Arc::new(FakeEnforcer::default());
        let binding_id = Uuid::new_v4();
        if let Some(policy_dsl) = source_policy {
            enforcer.set_bindings(vec![binding(
                binding_id,
                root_pid,
                process_start_time,
                policy_dsl,
            )]);
            enforcer.set_source_policy(
                binding_id,
                credential_policy("/root/secret.txt", 300, PolicyMode::Audit),
            );
        }
        let event = evidence(binding_id, evidence_pid, evidence_start_time);
        store.insert_event(&event).expect("evidence should persist");
        let case_id = Uuid::new_v4();
        store
            .upsert_case(&risk_case(case_id), &[event.event_id])
            .expect("case should persist");
        let enforcer_trait: Arc<dyn ContainmentEnforcer> = enforcer.clone();
        let coordinator = Arc::new(ContainmentCoordinator::new(
            Arc::clone(&store),
            enforcer_trait,
        ));
        Self {
            case_id,
            binding_id,
            store,
            enforcer,
            coordinator,
        }
    }

    fn set_status(&self, status: RiskCaseStatus) {
        self.store
            .review_case(self.case_id, status, 2)
            .expect("case status should update");
    }

    fn status(&self) -> RiskCaseStatus {
        self.store
            .case_detail(self.case_id)
            .expect("case should load")
            .case
            .status
    }

    fn candidate(&self) -> ContainmentCandidate {
        let identity = &self
            .store
            .case_detail(self.case_id)
            .expect("case should load")
            .evidence[0]
            .identity;
        ContainmentCandidate {
            agent_id: identity.agent_id.clone(),
            root_pid: identity.pid,
            process_start_time: identity.process_start_time,
            display_name: "Hermes test".into(),
        }
    }

    fn contain_as(
        &self,
        duration_secs: Option<u64>,
        candidates: &[ContainmentCandidate],
        requested_by: &str,
    ) -> Result<agentsight::security::ContainmentAction, ContainmentError> {
        let root_pid = candidates.first().map_or(0, |candidate| candidate.root_pid);
        self.coordinator.contain(
            self.case_id,
            ContainmentRequest {
                root_pid,
                duration_secs,
            },
            candidates,
            requested_by,
        )
    }

    fn contain(
        &self,
        duration_secs: Option<u64>,
    ) -> Result<agentsight::security::ContainmentAction, ContainmentError> {
        self.contain_as(
            duration_secs,
            &[self.candidate()],
            "principal:test-operator",
        )
    }

    fn latest_action(&self) -> agentsight::security::ContainmentAction {
        self.store
            .latest_containment_action(self.case_id)
            .expect("action query should work")
            .expect("action should exist")
    }

    fn insert_action(
        &self,
        lifecycle_state: ContainmentLifecycle,
        duration_secs: Option<u64>,
        expires_at_ns: Option<u64>,
        attempt_count: u32,
        next_retry_at_ns: Option<u64>,
    ) -> ContainmentAction {
        let candidate = self.candidate();
        let action = ContainmentAction {
            action_id: Uuid::new_v4(),
            case_id: self.case_id,
            binding_id: Uuid::new_v4(),
            source_binding_id: Some(self.binding_id),
            agent_id: "hermes-test".into(),
            root_pid: candidate.root_pid,
            process_start_time: candidate.process_start_time,
            source_path: "/root/secret.txt".into(),
            duration_secs,
            expires_at_ns,
            lifecycle_state,
            blocked_at_ns: None,
            requested_by: "principal:test-operator".into(),
            failure_stage: (lifecycle_state == ContainmentLifecycle::Expiring && attempt_count > 0)
                .then_some(ContainmentFailureStage::Detach),
            failure_reason: (lifecycle_state == ContainmentLifecycle::Expiring
                && attempt_count > 0)
                .then(|| "prior detach uncertainty".into()),
            attempt_count,
            next_retry_at_ns,
            created_at_ns: 10,
            updated_at_ns: 10,
        };
        self.store
            .insert_containment_action(&action)
            .expect("action should persist");
        if matches!(
            lifecycle_state,
            ContainmentLifecycle::Active | ContainmentLifecycle::Expiring
        ) {
            self.enforcer
                .seed_completed_forward(&action, containment_binding(&action));
        }
        action
    }

    fn reconstructed_coordinator(&self) -> Arc<ContainmentCoordinator> {
        coordinator(Arc::clone(&self.store), Arc::clone(&self.enforcer))
    }
}

fn coordinator(
    store: Arc<SecurityStore>,
    enforcer: Arc<FakeEnforcer>,
) -> Arc<ContainmentCoordinator> {
    let enforcer_trait: Arc<dyn ContainmentEnforcer> = enforcer;
    Arc::new(ContainmentCoordinator::new(store, enforcer_trait))
}

fn contain_candidate(
    coordinator: &ContainmentCoordinator,
    case_id: Uuid,
    candidate: ContainmentCandidate,
    duration_secs: Option<u64>,
) -> Result<agentsight::security::ContainmentAction, ContainmentError> {
    coordinator.contain(
        case_id,
        ContainmentRequest {
            root_pid: candidate.root_pid,
            duration_secs,
        },
        std::slice::from_ref(&candidate),
        "principal:test-operator",
    )
}

fn binding(binding_id: Uuid, root_pid: i32, start_time: u64, policy_dsl: &str) -> Binding {
    Binding {
        request: ApplyPolicy {
            binding_id,
            agent_id: "hermes-test".into(),
            session_id: Some("session-1".into()),
            root_pid,
            process_start_time: start_time,
            policy_id: "credential-exfiltration".into(),
            policy_revision: "3".into(),
            policy_dsl: policy_dsl.into(),
            policy_mode: None,
        },
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(1),
    }
}

fn evidence(binding_id: Uuid, pid: i32, start_time: u64) -> SecurityEvent {
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
            pid,
            process_start_time: start_time,
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

fn compiled_policy(action: &str, source: &str, trusted: Option<&str>) -> String {
    let trusted = trusted
        .map(|value| format!(" unless target \"{value}\""))
        .unwrap_or_default();
    format!(
        "source AGENT = exec \"**\"\nsource CREDENTIAL = file \"{source}\"\nrule agentsight-credential-exfiltration:\n  {action} connect endpoint \"*\" if CREDENTIAL{trusted}\n  because \"credential-derived data reached an untrusted network target\"\n"
    )
}

fn policy(source: &str) -> String {
    compiled_policy("notify", source, Some("trusted.example:443"))
}

fn credential_policy(
    source: &str,
    taint_ttl_secs: u64,
    mode: PolicyMode,
) -> CredentialExfiltrationPolicy {
    CredentialExfiltrationPolicy {
        policy_id: "credential-exfiltration".into(),
        revision: 3,
        source_patterns: vec![source.into()],
        trusted_endpoints: vec!["trusted.example:443".into()],
        taint_label: "CREDENTIAL".into(),
        taint_ttl_secs,
        destination_scope: DestinationScope::PublicIpv4,
        mode,
    }
}

fn containment_binding(action: &ContainmentAction) -> Binding {
    binding(
        action.binding_id,
        action.root_pid,
        action.process_start_time,
        &compiled_policy("block", &action.source_path, Some("trusted.example:443")),
    )
}

#[test]
fn legacy_active_without_forward_transition_remains_expiring() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action =
        fixture.insert_action(ContainmentLifecycle::Active, Some(60), Some(1_000), 0, None);
    fixture.enforcer.clear_transitions();
    let restarted = fixture.reconstructed_coordinator();

    assert!(matches!(
        restarted.reconcile_once(1_000),
        Err(ContainmentError::Enforcer(_))
    ));

    let stored = fixture.latest_action();
    assert_eq!(stored.action_id, action.action_id);
    assert_eq!(stored.lifecycle_state, ContainmentLifecycle::Expiring);
    assert!(stored.next_retry_at_ns.is_some());
    assert!(fixture.enforcer.detached().is_empty());
}

#[test]
fn explicit_removal_restores_audit_before_expiring_action() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = fixture.insert_action(ContainmentLifecycle::Active, None, None, 0, None);

    assert!(
        fixture
            .coordinator
            .remove_binding(action.binding_id)
            .expect("explicit removal should restore audit")
    );

    assert_eq!(
        fixture.latest_action().lifecycle_state,
        ContainmentLifecycle::Expired
    );
    assert_eq!(
        fixture.enforcer.binding_state(fixture.binding_id),
        Some(BindingState::Enforced)
    );
    assert_eq!(
        fixture.enforcer.binding_state(action.binding_id),
        Some(BindingState::Detached)
    );
}

#[test]
fn failed_handoff_managed_delete_never_falls_back_to_generic_detach() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = fixture.insert_action(ContainmentLifecycle::Failed, None, None, 5, None);

    assert!(matches!(
        fixture.coordinator.remove_binding(action.binding_id),
        Err(ContainmentError::CleanupRequired { action_id, .. }) if action_id == action.action_id
    ));
    assert_eq!(fixture.enforcer.detach_calls(), 0);
    assert_eq!(
        fixture.latest_action().lifecycle_state,
        ContainmentLifecycle::Failed
    );
}

#[test]
fn explicit_removal_targets_requested_action_beyond_global_batch_limit() {
    let store = Arc::new(SecurityStore::open_in_memory().expect("fixture store should open"));
    let mut older = Vec::new();
    for _ in 0..101 {
        let fixture = Fixture::with_store(
            Arc::clone(&store),
            Some(&policy("/root/secret.txt")),
            999_999,
            42,
        );
        fixture.insert_action(ContainmentLifecycle::Expiring, None, None, 1, Some(1));
        older.push(fixture);
    }
    let target = Fixture::with_store(store, Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = target.insert_action(ContainmentLifecycle::Active, None, None, 0, None);

    assert!(
        target
            .coordinator
            .remove_binding(action.binding_id)
            .unwrap()
    );
    assert_eq!(
        target.latest_action().lifecycle_state,
        ContainmentLifecycle::Expired
    );
    assert!(older.iter().all(|fixture| {
        fixture.latest_action().lifecycle_state == ContainmentLifecycle::Expiring
    }));
}

#[test]
fn pending_restart_activates_an_existing_exact_binding_without_reapply() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = fixture.insert_action(
        ContainmentLifecycle::Pending,
        Some(60),
        Some(2_000),
        0,
        Some(1_000),
    );
    fixture.enforcer.set_bindings(vec![
        binding(fixture.binding_id, 999_999, 42, &policy("/root/secret.txt")),
        containment_binding(&action),
    ]);
    fixture
        .enforcer
        .seed_completed_forward(&action, containment_binding(&action));

    fixture
        .reconstructed_coordinator()
        .reconcile_once(1_000)
        .expect("exact acknowledged binding should activate");

    assert_eq!(
        fixture.latest_action().lifecycle_state,
        ContainmentLifecycle::Active
    );
    assert_eq!(fixture.status(), RiskCaseStatus::Resolved);
    assert_eq!(fixture.enforcer.apply_calls(), 0);
}

#[test]
fn persistent_pending_restart_is_immediately_recoverable() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = fixture.insert_action(ContainmentLifecycle::Pending, None, None, 0, Some(10));
    fixture.enforcer.set_bindings(vec![
        binding(fixture.binding_id, 999_999, 42, &policy("/root/secret.txt")),
        containment_binding(&action),
    ]);
    fixture
        .enforcer
        .seed_completed_forward(&action, containment_binding(&action));

    fixture
        .reconstructed_coordinator()
        .reconcile_once(10)
        .expect("persistent pending intent should recover immediately");

    let stored = fixture.latest_action();
    assert_eq!(stored.lifecycle_state, ContainmentLifecycle::Active);
    assert_eq!(stored.next_retry_at_ns, None);
    assert_eq!(fixture.enforcer.apply_calls(), 0);
}

#[test]
fn expired_pending_restart_never_reapplies_enforcement() {
    for exact_binding_exists in [false, true] {
        let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
        let action = fixture.insert_action(
            ContainmentLifecycle::Pending,
            Some(60),
            Some(1_000),
            0,
            Some(10),
        );
        let mut bindings = vec![binding(
            fixture.binding_id,
            999_999,
            42,
            &policy("/root/secret.txt"),
        )];
        if exact_binding_exists {
            bindings.push(containment_binding(&action));
        }
        fixture.enforcer.set_bindings(bindings);
        if exact_binding_exists {
            fixture
                .enforcer
                .seed_completed_forward(&action, containment_binding(&action));
        }

        fixture
            .reconstructed_coordinator()
            .reconcile_once(1_000)
            .expect("expired pending intent should converge without applying");

        assert_eq!(
            fixture.latest_action().lifecycle_state,
            ContainmentLifecycle::Expired
        );
        let applied = fixture.enforcer.applied();
        assert_eq!(applied.len(), usize::from(exact_binding_exists));
        assert!(
            applied
                .iter()
                .all(|request| request.policy.mode == PolicyMode::Audit)
        );
        assert_eq!(
            fixture.enforcer.apply_calls(),
            usize::from(exact_binding_exists)
        );
        if exact_binding_exists {
            assert_eq!(
                fixture.enforcer.binding_state(fixture.binding_id),
                Some(BindingState::Enforced)
            );
            assert_eq!(
                fixture.enforcer.binding_state(action.binding_id),
                Some(BindingState::Detached)
            );
        }
        assert_eq!(
            fixture.enforcer.detach_calls(),
            usize::from(exact_binding_exists)
        );
    }
}

#[test]
fn pending_restart_fails_safely_without_original_binding_provenance() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = fixture.insert_action(
        ContainmentLifecycle::Pending,
        Some(60),
        Some(2_000),
        0,
        Some(1_000),
    );
    fixture.enforcer.set_bindings(Vec::new());

    let error = fixture
        .reconstructed_coordinator()
        .reconcile_once(1_000)
        .expect_err("unproved source provenance must fail safely");

    assert!(matches!(
        error,
        ContainmentError::RecoveryFailed { action_id, .. } if action_id == action.action_id
    ));
    let stored = fixture.latest_action();
    assert_eq!(stored.lifecycle_state, ContainmentLifecycle::Failed);
    assert_eq!(
        stored.failure_stage,
        Some(ContainmentFailureStage::Reconcile)
    );
    assert!(
        stored
            .failure_reason
            .as_deref()
            .is_some_and(|reason| !reason.chars().any(char::is_control))
    );
    assert_eq!(fixture.enforcer.apply_calls(), 0);
}

#[test]
fn unavailable_binding_snapshot_keeps_pending_recovery_retryable() {
    const SECOND_NS: u64 = 1_000_000_000;

    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let action = fixture.insert_action(
        ContainmentLifecycle::Pending,
        Some(60),
        Some(2 * SECOND_NS),
        0,
        Some(1_000),
    );
    fixture
        .enforcer
        .fail_next_bindings("enforcement transport unavailable");

    assert!(matches!(
        fixture.reconstructed_coordinator().reconcile_once(1_000),
        Err(ContainmentError::Enforcer(message))
            if message == "enforcement transport unavailable"
    ));
    let stored = fixture.latest_action();
    assert_eq!(stored.action_id, action.action_id);
    assert_eq!(stored.lifecycle_state, ContainmentLifecycle::Pending);
    assert_eq!(stored.attempt_count, 1);
    assert_eq!(stored.next_retry_at_ns, Some(1_000 + SECOND_NS));
    assert_eq!(fixture.status(), RiskCaseStatus::Open);
}

#[test]
fn expiring_restart_retries_temporary_and_explicit_persistent_cleanup() {
    for duration_secs in [Some(60), None] {
        let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
        let action = fixture.insert_action(
            ContainmentLifecycle::Expiring,
            duration_secs,
            duration_secs.map(|_| 1_000),
            1,
            Some(1_000),
        );

        fixture
            .reconstructed_coordinator()
            .reconcile_once(1_000)
            .expect("explicit cleanup retry should detach");

        assert_eq!(
            fixture.latest_action().lifecycle_state,
            ContainmentLifecycle::Expired
        );
        assert_eq!(fixture.enforcer.detached(), [action.binding_id]);
    }
}

#[test]
fn persistent_actions_without_cleanup_retry_never_expire() {
    for lifecycle in [ContainmentLifecycle::Active, ContainmentLifecycle::Expiring] {
        let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
        let action = fixture.insert_action(lifecycle, None, None, 0, None);

        fixture
            .reconstructed_coordinator()
            .reconcile_once(u64::MAX / 2)
            .expect("persistent action should be a no-op");

        assert_eq!(fixture.latest_action().lifecycle_state, lifecycle);
        assert_eq!(fixture.latest_action().action_id, action.action_id);
        assert_eq!(fixture.enforcer.detach_calls(), 0);
    }
}

#[test]
fn fifth_detach_failure_is_terminal_after_four_backoffs() {
    const SECOND_NS: u64 = 1_000_000_000;

    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    fixture.insert_action(ContainmentLifecycle::Active, Some(60), Some(1_000), 0, None);
    let restarted = fixture.reconstructed_coordinator();
    let mut due_at = 1_000;

    for (attempt, delay_secs) in [(1, 1), (2, 2), (3, 4), (4, 8)] {
        fixture
            .enforcer
            .fail_next_detach(&format!("detach failure {attempt}\nsecret"));
        assert!(matches!(
            restarted.reconcile_once(due_at),
            Err(ContainmentError::Enforcer(message))
                if message == format!("detach failure {attempt}secret")
        ));
        let stored = fixture.latest_action();
        assert_eq!(stored.lifecycle_state, ContainmentLifecycle::Expiring);
        assert_eq!(
            stored.failure_stage,
            Some(ContainmentFailureStage::Reconcile)
        );
        assert_eq!(stored.attempt_count, attempt);
        assert_eq!(
            stored.next_retry_at_ns,
            Some(due_at + delay_secs * SECOND_NS)
        );
        assert!(
            stored
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !reason.chars().any(char::is_control))
        );
        due_at += delay_secs * SECOND_NS;
    }

    fixture
        .enforcer
        .fail_next_detach("fifth detach attempt failed");
    assert!(matches!(
        restarted.reconcile_once(due_at),
        Err(ContainmentError::Enforcer(message))
            if message == "fifth detach attempt failed"
    ));
    let failed = fixture.latest_action();
    assert_eq!(failed.lifecycle_state, ContainmentLifecycle::Failed);
    assert_eq!(
        failed.failure_stage,
        Some(ContainmentFailureStage::Reconcile)
    );
    assert_eq!(failed.attempt_count, 5);
    assert_eq!(failed.next_retry_at_ns, None);
    assert_eq!(fixture.enforcer.detach_calls(), 5);
}

#[test]
fn detach_acknowledgement_is_required_before_expired_state() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    fixture.insert_action(ContainmentLifecycle::Active, Some(60), Some(1_000), 0, None);
    let restarted = fixture.reconstructed_coordinator();
    fixture.enforcer.fail_next_detach("not acknowledged");

    assert!(matches!(
        restarted.reconcile_once(1_000),
        Err(ContainmentError::Enforcer(message)) if message == "not acknowledged"
    ));
    assert_eq!(
        fixture.latest_action().lifecycle_state,
        ContainmentLifecycle::Expiring
    );

    restarted
        .reconcile_once(1_000 + 1_000_000_000)
        .expect("acknowledged retry should expire");
    assert_eq!(
        fixture.latest_action().lifecycle_state,
        ContainmentLifecycle::Expired
    );
}

#[test]
fn reconciler_worker_rejects_duplicates_and_stops_cleanly() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let coordinator = fixture.reconstructed_coordinator();
    let worker = coordinator
        .start_reconciler(Duration::from_millis(10))
        .expect("reconciler should start");

    assert!(matches!(
        coordinator.start_reconciler(Duration::from_millis(10)),
        Err(ContainmentError::AlreadyRunning)
    ));
    coordinator.stop();
    worker.join().expect("reconciler should stop");

    let restarted = coordinator
        .start_reconciler(Duration::from_millis(10))
        .expect("stopped reconciler should restart");
    coordinator.stop();
    restarted.join().expect("restarted reconciler should stop");
}

#[test]
fn reconciler_restart_is_owned_by_its_worker_generation() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    fixture.insert_action(ContainmentLifecycle::Pending, None, None, 0, Some(10));
    let coordinator = fixture.reconstructed_coordinator();
    let pause = fixture.enforcer.pause_bindings();
    let old_worker = coordinator
        .start_reconciler(Duration::from_secs(60))
        .expect("first generation should start");
    pause.entered.wait();

    coordinator.stop();
    let new_worker = coordinator
        .start_reconciler(Duration::from_secs(60))
        .expect("a stopped generation should be replaceable before teardown");
    pause.resume.wait();
    old_worker.join().expect("old generation should stop");

    assert!(matches!(
        coordinator.start_reconciler(Duration::from_secs(60)),
        Err(ContainmentError::AlreadyRunning)
    ));
    coordinator.stop();
    new_worker.join().expect("new generation should stop");
}

#[test]
fn reconciler_generation_resets_after_worker_panic() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    fixture.insert_action(ContainmentLifecycle::Pending, None, None, 0, Some(10));
    let coordinator = fixture.reconstructed_coordinator();
    fixture.enforcer.panic_next_bindings();

    let panicked = coordinator
        .start_reconciler(Duration::from_secs(60))
        .expect("panicking generation should start");
    assert!(panicked.join().is_err());

    let restarted = coordinator
        .start_reconciler(Duration::from_secs(60))
        .expect("RAII teardown should clear the panicked generation");
    coordinator.stop();
    restarted
        .join()
        .expect("replacement generation should stop");
}

#[test]
fn corrupt_due_row_does_not_block_valid_reconciliation() {
    let path = std::env::temp_dir().join(format!("corrupt-due-{}.db", Uuid::new_v4()));
    let fixture = Fixture::at_path(&path, Some(&policy("/root/secret.txt")), 999_999, 42);
    let corrupt = fixture.insert_action(
        ContainmentLifecycle::Pending,
        Some(60),
        Some(1_000),
        0,
        None,
    );
    let conn = rusqlite::Connection::open(&path).expect("fixture database should open");
    conn.execute(
        "UPDATE containment_actions SET lifecycle_state = 'unknown' WHERE action_id = ?1",
        [corrupt.action_id.to_string()],
    )
    .expect("fixture row should mutate");
    drop(conn);
    let valid = fixture.insert_action(ContainmentLifecycle::Active, Some(60), Some(1_000), 0, None);

    assert!(matches!(
        fixture.coordinator.reconcile_once(1_000),
        Err(ContainmentError::CorruptActions { count: 1 })
    ));

    let quarantined = fixture
        .store
        .containment_action(corrupt.action_id)
        .expect("corrupt action query should work")
        .expect("corrupt action should remain auditable");
    let reconciled = fixture
        .store
        .containment_action(valid.action_id)
        .expect("valid action query should work")
        .expect("valid action should exist");
    assert_eq!(quarantined.lifecycle_state, ContainmentLifecycle::Failed);
    assert_eq!(
        quarantined.failure_stage,
        Some(ContainmentFailureStage::Reconcile)
    );
    assert!(
        quarantined
            .failure_reason
            .as_deref()
            .is_some_and(|reason| !reason.chars().any(char::is_control))
    );
    assert_eq!(reconciled.lifecycle_state, ContainmentLifecycle::Expired);
    assert_eq!(fixture.enforcer.detached(), [valid.binding_id]);

    fixture
        .coordinator
        .reconcile_once(1_000)
        .expect("quarantined row should not recur");
    assert_eq!(fixture.enforcer.detach_calls(), 1);
    drop(fixture);
    std::fs::remove_file(path).expect("fixture database should be removed");
}

#[test]
fn plan_recovers_only_the_original_binding_path() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);

    let plan = fixture
        .coordinator
        .plan(fixture.case_id, Vec::new())
        .expect("plan should load");

    assert_eq!(plan.source_path, "/root/secret.txt");
    assert!(plan.candidates.is_empty());
    assert!(!plan.original_target_valid);
    assert_eq!(plan.default_duration_secs, 900);
    assert_eq!(
        (plan.min_duration_secs, plan.max_duration_secs),
        (60, 86_400)
    );
    let candidate = ContainmentCandidate {
        agent_id: "hermes-test".into(),
        root_pid: 12_345,
        process_start_time: 88,
        display_name: "replacement".into(),
    };
    assert!(matches!(
        fixture
            .coordinator
            .plan(fixture.case_id, vec![candidate.clone(), candidate]),
        Err(ContainmentError::AmbiguousCandidate(12_345))
    ));
}

#[test]
fn missing_original_binding_never_uses_redacted_evidence_path() {
    let fixture = Fixture::new(None, 999_999, 42);
    assert!(matches!(
        fixture.coordinator.plan(fixture.case_id, Vec::new()),
        Err(ContainmentError::SourcePolicyUnavailable(id)) if id == fixture.case_id
    ));
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let mut original = binding(fixture.binding_id, 999_999, 42, &policy("/root/secret.txt"));
    original.state = BindingState::Pending;
    fixture.enforcer.set_bindings(vec![original]);

    assert!(matches!(
        fixture.coordinator.plan(fixture.case_id, Vec::new()),
        Err(ContainmentError::SourcePolicyUnavailable(_))
    ));
}

#[test]
fn malformed_source_declarations_are_rejected() {
    let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
    let malformed = vec![
        "source AGENT = exec \"**\"\n".into(),
        "source CREDENTIAL = file \"relative/secret\"\n".into(),
        "source CREDENTIAL = file \"/root/secret\\\"\n".into(),
        "source CREDENTIAL=file \"/root/secret\"\n".into(),
        "source CREDENTIAL = file \"/root/a\"\nsource CREDENTIAL = file \"/root/b\"\n".into(),
        "source OTHER = file \"/root/secret\"\n".into(),
        policy("/root/./secret"),
        policy("/root/../secret"),
        format!(
            "{}source  CREDENTIAL = file \"/root/other\"\n",
            policy("/root/secret")
        ),
        format!(
            "{}source\tCREDENTIAL = file \"/root/other\"\n",
            policy("/root/secret")
        ),
    ];

    for dsl in malformed {
        fixture
            .enforcer
            .set_bindings(vec![binding(fixture.binding_id, 999_999, 42, &dsl)]);
        assert!(matches!(
            fixture.coordinator.plan(fixture.case_id, Vec::new()),
            Err(ContainmentError::SourcePolicyUnavailable(_))
        ));
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::process::{Child, Command, Stdio};

    use super::*;

    struct LiveProcess(Child);

    impl LiveProcess {
        fn spawn() -> Self {
            Self(
                Command::new("sleep")
                    .arg("60")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("sleep fixture should start"),
            )
        }

        fn pid(&self) -> i32 {
            self.0.id() as i32
        }

        fn start_time(&self) -> u64 {
            let stat = fs::read_to_string(format!("/proc/{}/stat", self.pid()))
                .expect("child stat should load");
            let close = stat.rfind(')').expect("child stat should have a name");
            stat[close + 1..]
                .split_whitespace()
                .nth(19)
                .expect("child stat should have a start time")
                .parse()
                .expect("child start time should parse")
        }
    }

    impl Drop for LiveProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn live_fixture() -> (LiveProcess, Fixture) {
        let process = LiveProcess::spawn();
        let fixture = Fixture::new(
            Some(&policy("/root/secret.txt")),
            process.pid(),
            process.start_time(),
        );
        (process, fixture)
    }

    #[test]
    fn pending_restart_reapplies_absent_binding_with_durable_identity() {
        let (_process, fixture) = live_fixture();
        let action = fixture.insert_action(
            ContainmentLifecycle::Pending,
            Some(60),
            Some(2_000),
            0,
            Some(1_000),
        );

        fixture
            .reconstructed_coordinator()
            .reconcile_once(1_000)
            .expect("absent binding should be safely recovered");

        assert_eq!(
            fixture.latest_action().lifecycle_state,
            ContainmentLifecycle::Active
        );
        assert_eq!(fixture.enforcer.apply_calls(), 1);
        let applied = fixture.enforcer.applied();
        assert_eq!(applied[0].binding_id, action.binding_id);
        assert_eq!(applied[0].root_pid, action.root_pid);
        assert_eq!(applied[0].process_start_time, action.process_start_time);
        assert_eq!(applied[0].policy.source_patterns, [action.source_path]);
    }

    #[test]
    fn unavailable_pending_apply_retries_the_same_durable_binding() {
        let (_process, fixture) = live_fixture();
        let action = fixture.insert_action(
            ContainmentLifecycle::Pending,
            Some(60),
            Some(u64::MAX / 2),
            0,
            Some(1_000),
        );
        fixture.enforcer.fail_next_apply("adapter unavailable");
        let restarted = fixture.reconstructed_coordinator();

        assert!(matches!(
            restarted.reconcile_once(1_000),
            Err(ContainmentError::Enforcer(message)) if message == "adapter unavailable"
        ));
        let pending = fixture.latest_action();
        assert_eq!(pending.action_id, action.action_id);
        assert_eq!(pending.binding_id, action.binding_id);
        assert_eq!(pending.lifecycle_state, ContainmentLifecycle::Pending);
        assert_eq!(pending.attempt_count, 1);
        assert!(pending.next_retry_at_ns.is_some());
        assert_eq!(fixture.status(), RiskCaseStatus::Open);

        restarted
            .reconcile_once(pending.next_retry_at_ns.unwrap_or(u64::MAX))
            .expect("restored enforcer should activate durable intent");
        let active = fixture.latest_action();
        assert_eq!(active.action_id, action.action_id);
        assert_eq!(active.binding_id, action.binding_id);
        assert_eq!(active.lifecycle_state, ContainmentLifecycle::Active);
        assert_eq!(fixture.status(), RiskCaseStatus::Resolved);
        assert_eq!(fixture.enforcer.apply_calls(), 2);
        assert_eq!(fixture.enforcer.applied().len(), 1);
        assert_eq!(fixture.enforcer.applied()[0].binding_id, action.binding_id);
    }

    #[test]
    fn root_binding_accepts_child_process_evidence() {
        let root = LiveProcess::spawn();
        let child = LiveProcess::spawn();
        let fixture = Fixture::with_identities(
            Some(&policy("/root/secret.txt")),
            root.pid(),
            root.start_time(),
            child.pid(),
            child.start_time(),
        );

        let plan = fixture
            .coordinator
            .plan(fixture.case_id, Vec::new())
            .expect("root binding should authorize descendant evidence");

        assert_eq!(
            plan.original_target.expect("original target").root_pid,
            root.pid()
        );
        assert!(plan.original_target_valid);
    }

    #[test]
    fn plan_validates_live_original_without_candidates() {
        let (_process, fixture) = live_fixture();

        let plan = fixture
            .coordinator
            .plan(fixture.case_id, Vec::new())
            .expect("plan should validate its original binding directly");

        assert!(plan.candidates.is_empty());
        assert!(plan.original_target_valid);
        let root_pid = plan.original_target.expect("original target").root_pid;
        let action = fixture
            .coordinator
            .contain(
                fixture.case_id,
                ContainmentRequest {
                    root_pid,
                    duration_secs: Some(900),
                },
                &[],
                "principal:test-operator",
            )
            .expect("original binding should not require a replacement candidate");
        assert_eq!(action.root_pid, root_pid);
    }

    #[test]
    fn contain_uses_original_policy_and_confirms_only_after_enforced_ack() {
        let (_process, fixture) = live_fixture();
        let candidate = fixture.candidate();
        let action = fixture
            .contain_as(Some(900), &[candidate], "  principal:test-operator  ")
            .expect("containment should apply");

        assert_eq!(action.source_path, "/root/secret.txt");
        assert_eq!(action.lifecycle_state, ContainmentLifecycle::Active);
        assert_eq!(action.requested_by, "principal:test-operator");
        assert_eq!(fixture.status(), RiskCaseStatus::Resolved);
        let applied = fixture.enforcer.applied();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].policy.mode, PolicyMode::Enforce);
        assert_eq!(applied[0].policy.revision, 3);
        assert_eq!(applied[0].policy.source_patterns, ["/root/secret.txt"]);
        assert_eq!(applied[0].policy.trusted_endpoints, ["trusted.example:443"]);
        assert_eq!(applied[0].policy.taint_label, "CREDENTIAL");
    }

    #[test]
    fn duration_and_process_identity_are_validated_before_apply() {
        let (_process, fixture) = live_fixture();
        for duration_secs in [Some(59), Some(86_401)] {
            assert!(matches!(
                fixture.contain(duration_secs),
                Err(ContainmentError::InvalidDuration)
            ));
        }
        let stale = ContainmentCandidate {
            agent_id: "hermes-test".into(),
            root_pid: 999_999,
            process_start_time: 1,
            display_name: "stale replacement".into(),
        };
        assert!(matches!(
            fixture.contain_as(
                Some(900),
                std::slice::from_ref(&stale),
                "principal:test-operator",
            ),
            Err(ContainmentError::RootProcessStale(pid)) if pid == stale.root_pid
        ));
        assert_eq!(fixture.enforcer.apply_calls(), 0);
    }

    #[test]
    fn exact_duration_boundaries_and_persistent_mode_are_accepted() {
        for duration_secs in [Some(60), Some(86_400), None] {
            let (_process, fixture) = live_fixture();
            let action = fixture
                .contain(duration_secs)
                .expect("valid duration should apply");
            assert_eq!(action.duration_secs, duration_secs);
            assert_eq!(action.expires_at_ns.is_none(), duration_secs.is_none());
        }
    }

    #[test]
    fn attach_failure_is_persisted_and_does_not_confirm_the_case() {
        let (_process, fixture) = live_fixture();
        fixture.enforcer.reject_next_apply("policy rejected");
        assert!(matches!(
            fixture.contain(Some(900)),
            Err(ContainmentError::Enforcer(message)) if message == "policy rejected"
        ));
        let action = fixture.latest_action();
        assert_eq!(action.lifecycle_state, ContainmentLifecycle::Failed);
        assert_eq!(action.failure_reason.as_deref(), Some("policy rejected"));
        assert_eq!(
            action.failure_stage,
            Some(agentsight::security::ContainmentFailureStage::Attach)
        );
        assert_eq!(fixture.status(), RiskCaseStatus::Open);
    }

    #[test]
    fn unavailable_attach_keeps_the_same_pending_action_without_detach() {
        let (_process, fixture) = live_fixture();
        fixture.enforcer.fail_next_apply("adapter unavailable");
        assert!(matches!(
            fixture.contain(Some(900)),
            Err(ContainmentError::Enforcer(message)) if message == "adapter unavailable"
        ));

        let pending = fixture.latest_action();
        assert_eq!(pending.lifecycle_state, ContainmentLifecycle::Pending);
        assert_eq!(pending.attempt_count, 1);
        assert!(pending.next_retry_at_ns.is_some());
        assert_eq!(fixture.enforcer.detach_calls(), 0);
        assert_eq!(fixture.status(), RiskCaseStatus::Open);

        fixture
            .reconstructed_coordinator()
            .reconcile_once(pending.next_retry_at_ns.unwrap_or(u64::MAX))
            .expect("retry should activate the same durable intent");
        let active = fixture.latest_action();
        assert_eq!(active.action_id, pending.action_id);
        assert_eq!(active.binding_id, pending.binding_id);
        assert_eq!(active.lifecycle_state, ContainmentLifecycle::Active);
        assert_eq!(fixture.enforcer.detach_calls(), 0);
    }

    #[test]
    fn acknowledgement_must_match_exact_enforce_semantics() {
        for mutation in [
            AckMutation::State(BindingState::Pending),
            AckMutation::Session,
            AckMutation::Source,
            AckMutation::TrustedEndpoint,
            AckMutation::Notify,
        ] {
            let (_process, fixture) = live_fixture();
            fixture.enforcer.mutate_ack(mutation);
            let result = fixture.contain(Some(900));
            let expected_lifecycle = match &result {
                Err(ContainmentError::Enforcer(_)) => ContainmentLifecycle::Failed,
                Err(ContainmentError::CleanupRequired { .. }) => ContainmentLifecycle::Expiring,
                _ => panic!("mutation {mutation:?} unexpectedly returned {result:?}"),
            };
            let action = fixture.latest_action();
            assert_eq!(action.lifecycle_state, expected_lifecycle);
            assert_eq!(fixture.enforcer.detached(), [action.binding_id]);
        }
    }

    #[test]
    fn detach_failure_keeps_the_claim_actionable() {
        let (_process, fixture) = live_fixture();
        fixture
            .enforcer
            .mutate_ack(AckMutation::State(BindingState::Pending));
        fixture
            .enforcer
            .fail_next_detach("detach adapter unavailable");

        let error = fixture
            .contain(Some(900))
            .expect_err("invalid acknowledgement must be detached");
        assert!(matches!(
            &error,
            ContainmentError::CleanupRequired { reason, .. }
                if reason.contains("detach adapter unavailable")
        ));
        let action = fixture.latest_action();
        assert!(matches!(
            error,
            ContainmentError::CleanupRequired { action_id, binding_id, .. }
                if action_id == action.action_id && binding_id == action.binding_id
        ));
        assert_eq!(action.lifecycle_state, ContainmentLifecycle::Expiring);
        assert_eq!(
            action.failure_stage,
            Some(ContainmentFailureStage::Reconcile)
        );
        assert_eq!(action.attempt_count, 1);
        assert!(action.next_retry_at_ns.is_some());
        assert!(
            action
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("detach adapter unavailable"))
        );
        assert!(matches!(
            fixture.contain(Some(900)),
            Err(ContainmentError::ContainmentExpiring(id)) if id == action.action_id
        ));
        assert_eq!(fixture.enforcer.apply_calls(), 1);
    }

    #[test]
    fn ineligible_case_states_never_apply() {
        for status in [
            RiskCaseStatus::FalsePositive,
            RiskCaseStatus::AcceptedRisk,
            RiskCaseStatus::Resolved,
        ] {
            let (_process, fixture) = live_fixture();
            fixture.set_status(status);
            assert!(matches!(
                fixture.contain(Some(900)),
                Err(ContainmentError::IneligibleCase { status: actual, .. }) if actual == status
            ));
            assert_eq!(fixture.enforcer.apply_calls(), 0);
        }
    }

    #[test]
    fn replacement_candidate_identity_must_match_proc_start_time() {
        let candidate_process = LiveProcess::spawn();
        let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 1);
        let candidate = ContainmentCandidate {
            agent_id: "hermes-test".into(),
            root_pid: candidate_process.pid(),
            process_start_time: candidate_process.start_time().saturating_add(1),
            display_name: "replacement".into(),
        };
        let result = contain_candidate(
            &fixture.coordinator,
            fixture.case_id,
            candidate.clone(),
            Some(900),
        );
        assert!(matches!(
            result,
            Err(ContainmentError::RootProcessStale(pid)) if pid == candidate.root_pid
        ));
        assert_eq!(fixture.enforcer.apply_calls(), 0);
    }

    #[test]
    fn plan_returns_only_live_candidates_and_creates_no_post_authority() {
        let replacement = LiveProcess::spawn();
        let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 1);
        let candidate = ContainmentCandidate {
            agent_id: "hermes-test".into(),
            root_pid: replacement.pid(),
            process_start_time: replacement.start_time(),
            display_name: "replacement".into(),
        };
        let stale = ContainmentCandidate {
            agent_id: "hermes-test".into(),
            root_pid: 999_999,
            process_start_time: 1,
            display_name: "stale".into(),
        };
        let plan = fixture
            .coordinator
            .plan(fixture.case_id, vec![candidate.clone(), stale])
            .expect("plan should load");
        assert_eq!(plan.candidates.as_slice(), std::slice::from_ref(&candidate));
        assert!(!plan.original_target_valid);

        assert!(matches!(
            fixture.coordinator.contain(
                fixture.case_id,
                ContainmentRequest {
                    root_pid: candidate.root_pid,
                    duration_secs: Some(900),
                },
                &[],
                "principal:test-operator",
            ),
            Err(ContainmentError::RootProcessStale(pid)) if pid == candidate.root_pid
        ));
        assert_eq!(fixture.enforcer.apply_calls(), 0);

        let action = contain_candidate(
            &fixture.coordinator,
            fixture.case_id,
            candidate.clone(),
            Some(900),
        )
        .expect("a freshly validated same-Agent candidate should replace the stale source");
        assert_eq!(action.root_pid, candidate.root_pid);
        assert_eq!(action.process_start_time, candidate.process_start_time);
        assert_eq!(fixture.enforcer.apply_calls(), 1);
        assert_eq!(
            fixture.enforcer.binding_state(fixture.binding_id),
            Some(BindingState::Detached)
        );
        assert_eq!(
            fixture.enforcer.binding_state(action.binding_id),
            Some(BindingState::Enforced)
        );
    }

    #[test]
    fn alternate_process_handoff_restores_audit_on_the_live_process() {
        let candidate_process = LiveProcess::spawn();
        let fixture = Fixture::new(Some(&policy("/root/secret.txt")), 999_999, 42);
        let candidate = ContainmentCandidate {
            agent_id: "hermes-test".into(),
            root_pid: candidate_process.pid(),
            process_start_time: candidate_process.start_time(),
            display_name: "replacement Hermes".into(),
        };
        let action = contain_candidate(
            &fixture.coordinator,
            fixture.case_id,
            candidate.clone(),
            Some(900),
        )
        .expect("alternate process should receive containment");

        assert!(
            fixture
                .coordinator
                .remove_binding(action.binding_id)
                .expect("audit should be restored on the alternate process")
        );

        let restored = fixture
            .enforcer
            .binding(fixture.binding_id)
            .expect("source audit binding should be restored");
        assert_eq!(restored.state, BindingState::Enforced);
        assert_eq!(restored.request.root_pid, candidate.root_pid);
        assert_eq!(
            restored.request.process_start_time,
            candidate.process_start_time
        );
    }

    #[test]
    fn recycled_original_pid_uses_the_validated_candidate_identity() {
        let candidate_process = LiveProcess::spawn();
        let candidate = ContainmentCandidate {
            agent_id: "hermes-test".into(),
            root_pid: candidate_process.pid(),
            process_start_time: candidate_process.start_time(),
            display_name: "replacement Hermes".into(),
        };
        let fixture = Fixture::new(
            Some(&policy("/root/secret.txt")),
            candidate.root_pid,
            candidate.process_start_time.saturating_add(1),
        );

        let action = contain_candidate(
            &fixture.coordinator,
            fixture.case_id,
            candidate.clone(),
            Some(900),
        )
        .expect("a recycled PID should use its validated candidate start time");

        assert_eq!(action.root_pid, candidate.root_pid);
        assert_eq!(action.process_start_time, candidate.process_start_time);
    }

    #[test]
    fn requested_by_is_validated_before_mutation() {
        let (_process, fixture) = live_fixture();
        let candidate = fixture.candidate();
        for requested_by in [
            String::new(),
            "   ".into(),
            "x".repeat(129),
            "principal:\noperator".into(),
        ] {
            assert!(matches!(
                fixture.contain_as(Some(900), std::slice::from_ref(&candidate), &requested_by,),
                Err(ContainmentError::InvalidRequestedBy)
            ));
        }
        assert_eq!(fixture.enforcer.apply_calls(), 0);
        assert_eq!(
            fixture
                .store
                .latest_containment_action(fixture.case_id)
                .expect("action query should work"),
            None
        );
    }

    #[test]
    fn durable_claim_serializes_coordinators_and_types_live_states() {
        let process = LiveProcess::spawn();
        let path = std::env::temp_dir().join(format!("containment-race-{}.db", Uuid::new_v4()));
        let fixture = Fixture::at_path(
            &path,
            Some(&policy("/root/secret.txt")),
            process.pid(),
            process.start_time(),
        );
        let second_store = Arc::new(SecurityStore::open(&path).expect("second store should open"));
        let second = coordinator(Arc::clone(&second_store), Arc::clone(&fixture.enforcer));
        let candidate = fixture.candidate();
        let pause = fixture.enforcer.pause_apply();
        let first = Arc::clone(&fixture.coordinator);
        let first_candidate = candidate.clone();
        let case_id = fixture.case_id;
        let worker = std::thread::spawn(move || {
            contain_candidate(&first, case_id, first_candidate, Some(900))
        });
        pause.entered.wait();
        let pending = fixture.latest_action();
        assert_eq!(pending.lifecycle_state, ContainmentLifecycle::Pending);
        assert!(pending.next_retry_at_ns > Some(pending.created_at_ns));
        let repeat =
            |duration| contain_candidate(&second, fixture.case_id, candidate.clone(), duration);

        assert!(matches!(
            repeat(Some(900)),
            Err(ContainmentError::ContainmentInProgress(_))
        ));
        pause.resume.wait();
        let active = worker
            .join()
            .expect("containment worker should join")
            .expect("first containment should activate");
        fixture.enforcer.set_bindings(Vec::new());
        let repeated = repeat(Some(900)).expect("active claim should be idempotent");
        assert_eq!(repeated.action_id, active.action_id);
        assert_eq!(fixture.enforcer.apply_calls(), 1);
        assert!(matches!(
            repeat(Some(901)),
            Err(ContainmentError::IncompatibleAction(_))
        ));

        let mut expiring = active;
        expiring.lifecycle_state = ContainmentLifecycle::Expiring;
        second_store
            .update_containment_action(&expiring)
            .expect("action should become expiring");
        assert!(matches!(
            repeat(Some(900)),
            Err(ContainmentError::ContainmentExpiring(id)) if id == expiring.action_id
        ));
        drop(fixture);
        drop(second);
        drop(second_store);
        std::fs::remove_file(path).expect("fixture database should be removed");
    }

    #[test]
    fn foreground_apply_lease_covers_complete_operation_bound_and_margin() {
        const PRIOR_LEASE_NS: u64 = 10_000_000_000;
        const COMPLETE_OPERATION_BOUND_NS: u64 = 60_000_000_000;
        const EXPIRED_DURATION_NS: u64 = 61_000_000_000;
        const EXPECTED_MARGIN_NS: u64 = 19_000_000_000;

        let process = LiveProcess::spawn();
        let path = std::env::temp_dir().join(format!("containment-lease-{}.db", Uuid::new_v4()));
        let fixture = Fixture::at_path(
            &path,
            Some(&policy("/root/secret.txt")),
            process.pid(),
            process.start_time(),
        );
        let recovery_store =
            Arc::new(SecurityStore::open(&path).expect("recovery store should open"));
        let recovery = coordinator(Arc::clone(&recovery_store), Arc::clone(&fixture.enforcer));
        let pause = fixture.enforcer.pause_apply();
        let foreground = Arc::clone(&fixture.coordinator);
        let candidate = fixture.candidate();
        let case_id = fixture.case_id;
        let worker = std::thread::spawn(move || {
            contain_candidate(&foreground, case_id, candidate, Some(60))
        });

        pause.entered.wait();
        let pending = fixture.latest_action();
        for elapsed_ns in [
            PRIOR_LEASE_NS,
            COMPLETE_OPERATION_BOUND_NS,
            EXPIRED_DURATION_NS,
            COMPLETE_OPERATION_BOUND_NS + EXPECTED_MARGIN_NS,
        ] {
            recovery
                .reconcile_once(pending.created_at_ns.saturating_add(elapsed_ns))
                .expect("foreground claim should not be due before its derived lease");
            assert_eq!(fixture.enforcer.apply_calls(), 1);
        }

        pause.resume.wait();
        let active = worker
            .join()
            .expect("foreground worker should join")
            .expect("foreground claim should activate");
        assert_eq!(active.lifecycle_state, ContainmentLifecycle::Active);
        assert_eq!(fixture.enforcer.apply_calls(), 1);

        drop(fixture);
        drop(recovery);
        drop(recovery_store);
        std::fs::remove_file(path).expect("fixture database should be removed");
    }

    #[test]
    fn review_race_detaches_and_persists_reconcile_failure() {
        let (_process, fixture) = live_fixture();
        let pause = fixture.enforcer.pause_apply();
        let coordinator = Arc::clone(&fixture.coordinator);
        let candidate = fixture.candidate();
        let case_id = fixture.case_id;
        let worker = std::thread::spawn(move || {
            contain_candidate(&coordinator, case_id, candidate, Some(900))
        });
        pause.entered.wait();
        fixture.set_status(RiskCaseStatus::FalsePositive);
        pause.resume.wait();

        assert!(matches!(
            worker.join().expect("containment worker should join"),
            Err(ContainmentError::CaseEligibilityChanged {
                status: RiskCaseStatus::FalsePositive,
                ..
            })
        ));
        let action = fixture.latest_action();
        assert_eq!(action.lifecycle_state, ContainmentLifecycle::Failed);
        assert_eq!(
            action.failure_stage,
            Some(ContainmentFailureStage::Reconcile)
        );
        assert!(
            action
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !reason.chars().any(char::is_control))
        );
        assert_eq!(fixture.enforcer.detached(), [action.binding_id]);
        assert_eq!(fixture.status(), RiskCaseStatus::FalsePositive);
    }

    #[test]
    fn stale_recovery_claim_cannot_detach_the_takeover_winner() {
        const CLAIM_LEASE_NS: u64 = 1_000_000_000;

        let process = LiveProcess::spawn();
        let path = std::env::temp_dir().join(format!("recovery-race-{}.db", Uuid::new_v4()));
        let fixture = Fixture::at_path(
            &path,
            Some(&policy("/root/secret.txt")),
            process.pid(),
            process.start_time(),
        );
        let action = fixture.insert_action(ContainmentLifecycle::Pending, None, None, 0, Some(10));
        let winner_store = Arc::new(SecurityStore::open(&path).expect("winner store should open"));
        let winner = coordinator(Arc::clone(&winner_store), Arc::clone(&fixture.enforcer));
        let pause = fixture.enforcer.pause_apply();
        let stale = fixture.reconstructed_coordinator();
        let stale_worker = std::thread::spawn(move || stale.reconcile_once(10));
        pause.entered.wait();

        winner
            .reconcile_once(11 + CLAIM_LEASE_NS)
            .expect("takeover worker should activate the durable intent");
        pause.resume.wait();

        assert!(matches!(
            stale_worker.join().expect("stale worker should join"),
            Err(ContainmentError::ClaimLost(id)) if id == action.action_id
        ));
        let stored = winner_store
            .containment_action(action.action_id)
            .expect("action query should work")
            .expect("action should exist");
        assert_eq!(stored.lifecycle_state, ContainmentLifecycle::Active);
        assert_eq!(fixture.enforcer.detach_calls(), 0);

        drop(fixture);
        drop(winner);
        drop(winner_store);
        std::fs::remove_file(path).expect("fixture database should be removed");
    }
}
