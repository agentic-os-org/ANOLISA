use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use agentsight_enforcement_protocol::{
    CredentialExfiltrationPolicy, CredentialPolicySnapshot, DestinationScope, EventIdentity,
    FileAction, PolicyMode, ReplacePolicy, ReplacementPolicy, SecurityEvent, SecurityEventKind,
};

use super::enforcer::{
    ContainmentReadinessLease, ContainmentReadinessStamp, StampedBinding, StampedBindings,
};
use super::*;
use crate::enforcement::{
    ApplyPolicy, Binding, BindingState, TransitionDirection, TransitionKey, read_process_start_time,
};
use crate::security::{ContainmentLifecycle, RiskCase, RiskSeverity};

const SECOND_NS: u64 = 1_000_000_000;
const SYNC_TIMEOUT: Duration = Duration::from_secs(5);

struct LeasePause {
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}

impl LeasePause {
    fn enter_and_wait(self) {
        self.entered
            .send(())
            .expect("lease waiter should still be observing entry");
        self.resume
            .recv_timeout(SYNC_TIMEOUT)
            .expect("lease waiter should be resumed before the test timeout");
    }
}

struct LeasePauseHandle {
    entered: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}

impl LeasePauseHandle {
    fn wait_until_entered(&self) {
        self.entered
            .recv_timeout(SYNC_TIMEOUT)
            .expect("reconciler should reach the lease before the test timeout");
    }

    fn resume(&self) {
        self.resume
            .send(())
            .expect("lease waiter should still be waiting for resume");
    }
}

fn pause_next(slot: &Mutex<Option<LeasePause>>) -> LeasePauseHandle {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    *slot.lock().expect("lease pause should lock") = Some(LeasePause {
        entered: entered_tx,
        resume: resume_rx,
    });
    LeasePauseHandle {
        entered: entered_rx,
        resume: resume_tx,
    }
}

struct ApplyingGenerationEnforcer {
    generation: Mutex<u64>,
    stamped_generation: Mutex<u64>,
    bindings: Mutex<Vec<Binding>>,
    source_policy_snapshot: Mutex<Option<CredentialPolicySnapshot>>,
    transitions: Mutex<HashMap<TransitionKey, (ReplacePolicy, Binding)>>,
    lease_pause: Mutex<Option<LeasePause>>,
    apply_calls: AtomicUsize,
    detach_calls: AtomicUsize,
}

struct TestReadinessLease;

impl ContainmentReadinessLease for TestReadinessLease {}

impl ApplyingGenerationEnforcer {
    fn new(source: Binding) -> Self {
        let source_policy_snapshot = CredentialPolicySnapshot::capture(credential_policy(
            "/root/secret.txt",
            300,
            PolicyMode::Audit,
        ))
        .expect("source policy should be valid");
        Self {
            generation: Mutex::new(1),
            stamped_generation: Mutex::new(1),
            bindings: Mutex::new(vec![source]),
            source_policy_snapshot: Mutex::new(Some(source_policy_snapshot)),
            transitions: Mutex::new(HashMap::new()),
            lease_pause: Mutex::new(None),
            apply_calls: AtomicUsize::new(0),
            detach_calls: AtomicUsize::new(0),
        }
    }

    fn pause_next_lease(&self) -> LeasePauseHandle {
        pause_next(&self.lease_pause)
    }

    fn stamp(&self) {
        let generation = *self.generation.lock().expect("generation should lock");
        *self
            .stamped_generation
            .lock()
            .expect("stamped generation should lock") = generation;
    }

    fn advance_generation(&self) {
        let mut generation = self.generation.lock().expect("generation should lock");
        *generation = generation.saturating_add(1);
    }

    fn remove_source_policy_snapshot(&self) {
        *self
            .source_policy_snapshot
            .lock()
            .expect("source snapshot should lock") = None;
    }
}

impl ContainmentEnforcer for ApplyingGenerationEnforcer {
    fn foreground_claim_lease(&self) -> Duration {
        Duration::from_secs(20)
    }

    fn begin_transition(
        &self,
        key: TransitionKey,
        transition: ReplacePolicy,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        self.stamp();
        self.apply_calls.fetch_add(1, Ordering::AcqRel);
        let binding = match transition.replacement.clone() {
            ReplacementPolicy::Credential(request) => binding(
                request.binding_id,
                request.root_pid,
                request.process_start_time,
                enforce_policy(&request.policy.source_patterns[0]),
            ),
            ReplacementPolicy::Generic(request) => Binding {
                request,
                state: BindingState::Enforced,
                message: None,
                domain_id: transition.expected.domain_id.or(Some(1)),
            },
        };
        let mut bindings = self.bindings.lock().expect("bindings should lock");
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
            .expect("transitions should lock")
            .insert(key, (transition, binding.clone()));
        Ok(StampedBinding::stable(binding))
    }

    fn resume_transition(
        &self,
        key: &TransitionKey,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        self.stamp();
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
        let (request, acknowledgement) = self
            .transitions
            .lock()
            .expect("transitions should lock")
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
        self.detach_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn bindings(&self) -> Result<StampedBindings, ContainmentEnforcerError> {
        self.stamp();
        Ok(StampedBindings::stable(
            self.bindings.lock().expect("bindings should lock").clone(),
        ))
    }

    fn credential_policy_snapshot(
        &self,
        _: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, ContainmentEnforcerError> {
        Ok(self
            .source_policy_snapshot
            .lock()
            .expect("source snapshot should lock")
            .clone())
    }

    fn lease_ready(
        &self,
        _: ContainmentReadinessStamp,
    ) -> Result<Box<dyn ContainmentReadinessLease + '_>, ContainmentEnforcerError> {
        if let Some(pause) = self
            .lease_pause
            .lock()
            .expect("lease pause should lock")
            .take()
        {
            pause.enter_and_wait();
        }
        let generation = *self.generation.lock().expect("generation should lock");
        let stamped = *self
            .stamped_generation
            .lock()
            .expect("stamped generation should lock");
        if generation != stamped {
            return Err(ContainmentEnforcerError::Unavailable(
                "ingestion changed".into(),
            ));
        }
        Ok(Box::new(TestReadinessLease))
    }
}

#[test]
fn apply_ack_from_generation_a_cannot_activate_under_generation_b() {
    let source_binding_id = Uuid::new_v4();
    let action_binding_id = Uuid::new_v4();
    let mut target = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("live test target should start");
    let pid = i32::try_from(target.id()).expect("test PID should fit in i32");
    let process_start_time =
        read_process_start_time(pid).expect("live test process should have a start time");
    let enforcer = Arc::new(ApplyingGenerationEnforcer::new(binding(
        source_binding_id,
        pid,
        process_start_time,
        audit_policy("/root/secret.txt"),
    )));
    let (security_store, case_id, action) = security_fixture(
        source_binding_id,
        action_binding_id,
        pid,
        process_start_time,
    );
    let pause = enforcer.pause_next_lease();
    let enforcer_trait: Arc<dyn ContainmentEnforcer> = enforcer.clone();
    let containment = Arc::new(ContainmentCoordinator::new(
        Arc::clone(&security_store),
        enforcer_trait,
    ));
    let reconciling = Arc::clone(&containment);
    let worker = thread::spawn(move || reconciling.reconcile_once(1_000));
    pause.wait_until_entered();

    enforcer.advance_generation();
    pause.resume();
    assert!(matches!(
        worker.join().expect("reconciler should stop"),
        Err(ContainmentError::Enforcer(_))
    ));
    assert_pending(&security_store, case_id, &action);
    assert_eq!(enforcer.apply_calls.load(Ordering::Acquire), 1);
    assert_eq!(enforcer.detach_calls.load(Ordering::Acquire), 0);
    let transition = enforcer
        .transitions
        .lock()
        .expect("transitions should lock")
        .values()
        .next()
        .expect("forward transition should be durable")
        .0
        .clone();
    assert_eq!(
        transition
            .source
            .credential_snapshot()
            .expect("source snapshot should be captured")
            .policy()
            .expect("source snapshot should validate")
            .taint_ttl_secs,
        300
    );
    let ReplacementPolicy::Credential(target_policy) = transition.replacement else {
        panic!("containment should remain a structured credential policy");
    };
    assert_eq!(target_policy.policy.taint_ttl_secs, 300);

    containment
        .reconcile_once(1_000 + SECOND_NS)
        .expect("generation B should recover the exact binding");
    assert_active(&security_store, case_id, &action);
    assert_eq!(enforcer.apply_calls.load(Ordering::Acquire), 1);
    assert_eq!(enforcer.detach_calls.load(Ordering::Acquire), 0);
    target.kill().expect("live test target should stop");
    target.wait().expect("live test target should be reaped");
}

#[test]
fn missing_source_policy_snapshot_retains_the_audit_binding() {
    let source_binding_id = Uuid::new_v4();
    let source = binding(
        source_binding_id,
        999_999,
        42,
        audit_policy("/root/secret.txt"),
    );
    let enforcer = Arc::new(ApplyingGenerationEnforcer::new(source.clone()));
    enforcer.remove_source_policy_snapshot();
    let (security_store, case_id, _) =
        security_fixture(source_binding_id, Uuid::new_v4(), 999_999, 42);
    let enforcer_trait: Arc<dyn ContainmentEnforcer> = enforcer.clone();
    let containment = ContainmentCoordinator::new(security_store, enforcer_trait);

    assert!(matches!(
        containment.plan(case_id, Vec::new()),
        Err(ContainmentError::SourcePolicyUnavailable(id)) if id == case_id
    ));
    assert_eq!(
        enforcer
            .bindings
            .lock()
            .expect("bindings should lock")
            .as_slice(),
        &[source]
    );
    assert_eq!(enforcer.apply_calls.load(Ordering::Acquire), 0);
    assert_eq!(enforcer.detach_calls.load(Ordering::Acquire), 0);
}

fn security_fixture(
    source_binding_id: Uuid,
    action_binding_id: Uuid,
    pid: i32,
    process_start_time: u64,
) -> (Arc<SecurityStore>, Uuid, ContainmentAction) {
    let store = Arc::new(SecurityStore::open_in_memory().expect("store should open"));
    let event = evidence(source_binding_id, pid, process_start_time);
    store.insert_event(&event).expect("evidence should persist");
    let case_id = Uuid::new_v4();
    store
        .upsert_case(&risk_case(case_id), &[event.event_id])
        .expect("case should persist");
    let action = pending_action(
        case_id,
        source_binding_id,
        action_binding_id,
        pid,
        process_start_time,
    );
    store
        .insert_containment_action(&action)
        .expect("action should persist");
    (store, case_id, action)
}

fn assert_pending(store: &SecurityStore, case_id: Uuid, expected: &ContainmentAction) {
    let action = latest_action(store, case_id);
    assert_eq!(action.action_id, expected.action_id);
    assert_eq!(action.binding_id, expected.binding_id);
    assert_eq!(action.lifecycle_state, ContainmentLifecycle::Pending);
    assert_eq!(action.attempt_count, 1);
    assert_eq!(action.next_retry_at_ns, Some(1_000 + SECOND_NS));
}

fn assert_active(store: &SecurityStore, case_id: Uuid, expected: &ContainmentAction) {
    let action = latest_action(store, case_id);
    assert_eq!(action.action_id, expected.action_id);
    assert_eq!(action.binding_id, expected.binding_id);
    assert_eq!(action.lifecycle_state, ContainmentLifecycle::Active);
}

fn latest_action(store: &SecurityStore, case_id: Uuid) -> ContainmentAction {
    store
        .latest_containment_action(case_id)
        .expect("action query should work")
        .expect("action should exist")
}

fn binding(
    binding_id: Uuid,
    root_pid: i32,
    process_start_time: u64,
    policy_dsl: String,
) -> Binding {
    Binding {
        request: ApplyPolicy {
            binding_id,
            agent_id: "hermes-test".into(),
            session_id: Some("session-1".into()),
            root_pid,
            process_start_time,
            policy_id: "credential-exfiltration".into(),
            policy_revision: "3".into(),
            policy_dsl,
            policy_mode: None,
        },
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(1),
    }
}

fn audit_policy(source: &str) -> String {
    compiled_policy("notify", source)
}

fn enforce_policy(source: &str) -> String {
    compiled_policy("block", source)
}

fn compiled_policy(action: &str, source: &str) -> String {
    format!(
        "source AGENT = exec \"**\"\nsource CREDENTIAL = file \"{source}\"\nrule agentsight-credential-exfiltration:\n  {action} connect endpoint \"*\" if CREDENTIAL unless target \"trusted.example:443\"\n  because \"credential-derived data reached an untrusted network target\"\n"
    )
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

fn evidence(binding_id: Uuid, pid: i32, process_start_time: u64) -> SecurityEvent {
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
            process_start_time,
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

fn pending_action(
    case_id: Uuid,
    source_binding_id: Uuid,
    binding_id: Uuid,
    root_pid: i32,
    process_start_time: u64,
) -> ContainmentAction {
    ContainmentAction {
        action_id: Uuid::new_v4(),
        case_id,
        binding_id,
        source_binding_id: Some(source_binding_id),
        agent_id: "hermes-test".into(),
        root_pid,
        process_start_time,
        source_path: "/root/secret.txt".into(),
        duration_secs: Some(60),
        expires_at_ns: Some(3 * SECOND_NS),
        lifecycle_state: ContainmentLifecycle::Pending,
        blocked_at_ns: None,
        requested_by: "principal:test-operator".into(),
        failure_stage: None,
        failure_reason: None,
        attempt_count: 0,
        next_retry_at_ns: Some(1_000),
        created_at_ns: 10,
        updated_at_ns: 10,
    }
}
