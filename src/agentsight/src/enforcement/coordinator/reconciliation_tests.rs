use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, CredentialExfiltrationPolicy, CredentialPolicySnapshot,
    DestinationScope, PolicyMode, ProtocolError, ReplaceFailureCode, ReplaceOutcome, ReplacePolicy,
    ReplacementPolicy, ReplacementSource,
};
use rusqlite::{Connection, params};

use super::super::transition::execute_transition;
use super::*;
use crate::enforcement::{
    EnforcementStoreError, PolicyTransition, TransitionDirection, TransitionKey, TransitionPhase,
};

struct TestDatabase {
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "agentsight-reconciliation-{}.db",
                uuid::Uuid::new_v4()
            )),
        }
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(format!("{}-wal", self.path.display()));
        let _ = fs::remove_file(format!("{}-shm", self.path.display()));
    }
}

struct ScriptedClient {
    actual: Mutex<Vec<Binding>>,
    apply_results: Mutex<VecDeque<Result<Binding, EnforcementError>>>,
    credential_apply_results: Mutex<VecDeque<Result<Binding, EnforcementError>>>,
    replace_results: Mutex<VecDeque<Result<ReplaceOutcome, EnforcementError>>>,
    applied: Mutex<Vec<uuid::Uuid>>,
    credential_applied: Mutex<Vec<ApplyCredentialPolicy>>,
    operations: Mutex<Vec<String>>,
}

impl ScriptedClient {
    fn new(apply_results: Vec<Result<Binding, EnforcementError>>) -> Self {
        Self {
            actual: Mutex::new(Vec::new()),
            apply_results: Mutex::new(apply_results.into()),
            credential_apply_results: Mutex::new(VecDeque::new()),
            replace_results: Mutex::new(VecDeque::new()),
            applied: Mutex::new(Vec::new()),
            credential_applied: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
        }
    }

    fn with_replacements(
        actual: Vec<Binding>,
        replace_results: Vec<Result<ReplaceOutcome, EnforcementError>>,
    ) -> Self {
        Self {
            actual: Mutex::new(actual),
            apply_results: Mutex::new(VecDeque::new()),
            credential_apply_results: Mutex::new(VecDeque::new()),
            replace_results: Mutex::new(replace_results.into()),
            applied: Mutex::new(Vec::new()),
            credential_applied: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
        }
    }

    fn with_credential_applies(
        actual: Vec<Binding>,
        apply_results: Vec<Result<Binding, EnforcementError>>,
        replace_results: Vec<Result<ReplaceOutcome, EnforcementError>>,
    ) -> Self {
        Self {
            actual: Mutex::new(actual),
            apply_results: Mutex::new(VecDeque::new()),
            credential_apply_results: Mutex::new(apply_results.into()),
            replace_results: Mutex::new(replace_results.into()),
            applied: Mutex::new(Vec::new()),
            credential_applied: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
        }
    }

    fn applied(&self) -> Vec<uuid::Uuid> {
        self.applied
            .lock()
            .expect("applied record should not be poisoned")
            .clone()
    }

    fn credential_applied(&self) -> Vec<ApplyCredentialPolicy> {
        self.credential_applied
            .lock()
            .expect("credential applied record should not be poisoned")
            .clone()
    }

    fn operations(&self) -> Vec<String> {
        self.operations
            .lock()
            .expect("operation record should not be poisoned")
            .clone()
    }
}

impl ReplacementClient for ScriptedClient {
    fn replace(
        &self,
        _request: ReplacePolicy,
        _required_subscription_id: uuid::Uuid,
    ) -> Result<ReplaceOutcome, EnforcementError> {
        self.replace_results
            .lock()
            .expect("replace results should not be poisoned")
            .pop_front()
            .expect("scripted replace result should exist")
    }
}

impl DesiredStateClient for ScriptedClient {
    fn bindings(&self) -> Result<Vec<Binding>, EnforcementError> {
        Ok(self
            .actual
            .lock()
            .expect("actual bindings should not be poisoned")
            .clone())
    }

    fn apply(
        &self,
        request: ApplyPolicy,
        _required_subscription_id: uuid::Uuid,
    ) -> Result<Binding, EnforcementError> {
        self.applied
            .lock()
            .expect("applied record should not be poisoned")
            .push(request.binding_id);
        let result = self
            .apply_results
            .lock()
            .expect("apply results should not be poisoned")
            .pop_front()
            .expect("scripted apply result should exist");
        if let Ok(binding) = &result {
            let mut actual = self
                .actual
                .lock()
                .expect("actual bindings should not be poisoned");
            actual.retain(|existing| existing.request.binding_id != binding.request.binding_id);
            actual.push(binding.clone());
        }
        result
    }

    fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
        _required_subscription_id: uuid::Uuid,
    ) -> Result<Binding, EnforcementError> {
        self.operations
            .lock()
            .expect("operation record should not be poisoned")
            .push(format!("apply:{}", request.binding_id));
        self.credential_applied
            .lock()
            .expect("credential applied record should not be poisoned")
            .push(request);
        let result = self
            .credential_apply_results
            .lock()
            .expect("credential apply results should not be poisoned")
            .pop_front()
            .expect("scripted credential apply result should exist");
        if let Ok(binding) = &result {
            let mut actual = self
                .actual
                .lock()
                .expect("actual bindings should not be poisoned");
            actual.retain(|existing| existing.request.binding_id != binding.request.binding_id);
            actual.push(binding.clone());
        }
        result
    }

    fn detach(&self, binding_id: uuid::Uuid) -> Result<(), EnforcementError> {
        self.operations
            .lock()
            .expect("operation record should not be poisoned")
            .push(format!("detach:{binding_id}"));
        self.actual
            .lock()
            .expect("actual bindings should not be poisoned")
            .retain(|binding| binding.request.binding_id != binding_id);
        Ok(())
    }
}

fn request(id: &str) -> ApplyPolicy {
    ApplyPolicy {
        binding_id: uuid::Uuid::parse_str(id).expect("fixture UUID should parse"),
        agent_id: "reconciliation-agent".into(),
        session_id: Some("reconciliation-session".into()),
        root_pid: 42,
        process_start_time: 99,
        policy_id: "reconciliation-policy".into(),
        policy_revision: "revision-1".into(),
        policy_dsl: "label AGENT".into(),
        policy_mode: None,
    }
}

fn desired(request: ApplyPolicy) -> Binding {
    Binding {
        request,
        state: BindingState::Degraded,
        message: Some("backend restarted".into()),
        domain_id: None,
    }
}

fn enforced(request: ApplyPolicy, domain_id: u32) -> Binding {
    Binding {
        request,
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(domain_id),
    }
}

fn credential_request(id: &str, ttl_secs: u64) -> ApplyCredentialPolicy {
    ApplyCredentialPolicy {
        binding_id: uuid::Uuid::parse_str(id).expect("fixture UUID should parse"),
        agent_id: "reconciliation-agent".into(),
        session_id: Some("reconciliation-session".into()),
        root_pid: 42,
        process_start_time: 99,
        policy: CredentialExfiltrationPolicy {
            policy_id: "reconciliation-policy".into(),
            revision: 1,
            source_patterns: vec!["/tmp/credential".into()],
            trusted_endpoints: vec!["trusted.example:443".into()],
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: ttl_secs,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        },
    }
}

fn credential_binding(request: &ApplyCredentialPolicy, domain_id: u32) -> Binding {
    enforced(
        ApplyPolicy {
            binding_id: request.binding_id,
            agent_id: request.agent_id.clone(),
            session_id: request.session_id.clone(),
            root_pid: request.root_pid,
            process_start_time: request.process_start_time,
            policy_id: request.policy.policy_id.clone(),
            policy_revision: request.policy.revision.to_string(),
            policy_dsl: format!(
                "mode audit ttl {}\nsource CREDENTIAL = file \"/tmp/credential\"",
                request.policy.taint_ttl_secs
            ),
            policy_mode: Some(request.policy.mode),
        },
        domain_id,
    )
}

#[test]
fn reconnect_replays_durable_credential_intent_through_typed_lease() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let request = credential_request("00000000-0000-0000-0000-000000000071", 300);
    store
        .begin_credential_policy_intent(&request)
        .expect("typed intent should seed before mutation");
    let acknowledged = credential_binding(&request, 11);
    let client = ScriptedClient::with_credential_applies(
        vec![acknowledged.clone()],
        vec![Ok(acknowledged.clone())],
        Vec::new(),
    );

    reconcile_desired_state(&client, &store, uuid::Uuid::new_v4())
        .expect("typed intent should reconcile");

    assert!(client.applied().is_empty());
    assert_eq!(client.credential_applied(), vec![request.clone()]);
    assert_eq!(
        store
            .binding(request.binding_id)
            .expect("binding should load"),
        Some(acknowledged)
    );
    assert_eq!(
        store
            .credential_policy_snapshot(request.binding_id)
            .expect("snapshot should decode")
            .expect("snapshot should persist")
            .policy()
            .expect("snapshot policy should validate")
            .taint_ttl_secs,
        300
    );
}

#[test]
fn reconnect_detaches_stale_runtime_before_replaying_credential_intent() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let credential = credential_request("00000000-0000-0000-0000-000000000073", 300);
    store
        .begin_credential_policy_intent(&credential)
        .expect("typed intent should seed before mutation");
    let stale = enforced(request("00000000-0000-0000-0000-000000000074"), 12);
    let acknowledged = credential_binding(&credential, 13);
    let client = ScriptedClient::with_credential_applies(
        vec![stale.clone()],
        vec![Ok(acknowledged)],
        Vec::new(),
    );

    reconcile_desired_state(&client, &store, uuid::Uuid::new_v4())
        .expect("stale runtime should be cleaned before credential replay");

    assert_eq!(
        client.operations(),
        vec![
            format!("detach:{}", stale.request.binding_id),
            format!("apply:{}", credential.binding_id),
        ]
    );
}

#[test]
fn cleared_runtime_reestablishes_structured_source_without_blocking_readiness() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let credential = credential_request("00000000-0000-0000-0000-000000000081", 300);
    let source = credential_binding(&credential, 7);
    store
        .upsert_credential_binding(&source, &credential.policy)
        .expect("credential source should seed");
    let target = request("00000000-0000-0000-0000-000000000082");
    let transition = PolicyTransition::pending(
        TransitionKey {
            action_id: uuid::Uuid::new_v4(),
            direction: TransitionDirection::Forward,
        },
        ReplacePolicy {
            expected: source,
            source: ReplacementSource::Credential(
                CredentialPolicySnapshot::capture(credential.policy.clone())
                    .expect("fixture policy should validate"),
            ),
            replacement: ReplacementPolicy::Generic(target),
        },
    );
    store
        .begin_transition(&transition)
        .expect("pending transition should seed");
    let recovered = credential_binding(&credential, 11);
    let client = ScriptedClient::with_credential_applies(
        Vec::new(),
        vec![Ok(recovered.clone())],
        vec![Ok(ReplaceOutcome::SourceRetained {
            binding: recovered.clone(),
            code: ReplaceFailureCode::UnsupportedHandoff,
        })],
    );

    reconcile_desired_state(&client, &store, uuid::Uuid::new_v4())
        .expect("proven source retention should not block startup reconciliation");

    assert!(client.applied().is_empty());
    assert_eq!(client.credential_applied(), vec![credential]);
    let persisted = store
        .transition(&transition.key)
        .expect("transition should load")
        .expect("transition should remain");
    assert_eq!(persisted.phase, TransitionPhase::Pending);
    assert_eq!(persisted.request.expected, recovered.clone());
    assert_eq!(persisted.acknowledgement, Some(recovered));
}

#[test]
fn indeterminate_transition_blocks_without_a_privileged_retry() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let source = enforced(request("00000000-0000-0000-0000-000000000091"), 7);
    let target = request("00000000-0000-0000-0000-000000000092");
    store.upsert_binding(&source).expect("source should seed");
    let transition = transition(source.clone(), target);
    store
        .begin_transition(&transition)
        .expect("transition should seed");
    store
        .mark_transition_indeterminate(&transition.key, ReplaceFailureCode::KernelFailure)
        .expect("transition should become indeterminate");
    let client = ScriptedClient::with_replacements(Vec::new(), Vec::new());

    assert!(matches!(
        reconcile_desired_state(&client, &store, uuid::Uuid::new_v4()),
        Err(EnforcementCoordinatorError::TransitionUnavailable)
    ));
    assert!(client.applied().is_empty());
    assert!(client.credential_applied().is_empty());
    assert!(
        client
            .replace_results
            .lock()
            .expect("replace results should lock")
            .is_empty()
    );
}

fn transition(source: Binding, target: ApplyPolicy) -> PolicyTransition {
    PolicyTransition::pending(
        TransitionKey {
            action_id: uuid::Uuid::new_v4(),
            direction: TransitionDirection::Forward,
        },
        ReplacePolicy {
            expected: source,
            source: ReplacementSource::Generic,
            replacement: ReplacementPolicy::Generic(target),
        },
    )
}

#[test]
fn pending_transition_reconciles_before_ordinary_bindings() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let source = enforced(request("00000000-0000-0000-0000-000000000031"), 7);
    let target = enforced(request("00000000-0000-0000-0000-000000000032"), 7);
    store
        .upsert_binding(&source)
        .expect("source binding should seed");
    let transition = transition(source.clone(), target.request.clone());
    store
        .begin_transition(&transition)
        .expect("transition should seed");
    let client = ScriptedClient::with_replacements(
        vec![target.clone()],
        vec![Ok(ReplaceOutcome::Applied(target.clone()))],
    );

    reconcile_desired_state(&client, &store, uuid::Uuid::new_v4())
        .expect("transition should reconcile");

    assert!(client.applied().is_empty());
    assert_eq!(
        store
            .transition(&transition.key)
            .expect("transition should load")
            .expect("transition should exist")
            .phase,
        TransitionPhase::Completed
    );
    assert_eq!(
        store
            .binding(source.request.binding_id)
            .expect("source should load")
            .expect("source should exist")
            .state,
        BindingState::Detached
    );
    assert_eq!(
        store
            .binding(target.request.binding_id)
            .expect("target should load"),
        Some(target)
    );
}

#[test]
fn ambiguous_transition_blocks_ordinary_binding_reconciliation() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let source = enforced(request("00000000-0000-0000-0000-000000000041"), 7);
    let target = request("00000000-0000-0000-0000-000000000042");
    store
        .upsert_binding(&source)
        .expect("source binding should seed");
    let transition = transition(source.clone(), target);
    store
        .begin_transition(&transition)
        .expect("transition should seed");
    let client = ScriptedClient::with_replacements(
        vec![source],
        vec![Ok(ReplaceOutcome::Indeterminate {
            code: ReplaceFailureCode::KernelFailure,
        })],
    );

    let result = reconcile_desired_state(&client, &store, uuid::Uuid::new_v4());

    assert!(matches!(
        result,
        Err(EnforcementCoordinatorError::TransitionUnavailable)
    ));
    assert!(client.applied().is_empty());
    assert_eq!(
        store
            .transition(&transition.key)
            .expect("transition should load")
            .expect("transition should exist")
            .phase,
        TransitionPhase::Indeterminate
    );
}

#[test]
fn reverse_source_retention_issues_five_real_privileged_attempts() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let containment = enforced(request("00000000-0000-0000-0000-000000000051"), 7);
    let audit = enforced(request("00000000-0000-0000-0000-000000000052"), 7);
    let transition = PolicyTransition::pending(
        TransitionKey {
            action_id: uuid::Uuid::new_v4(),
            direction: TransitionDirection::Reverse,
        },
        ReplacePolicy {
            expected: containment.clone(),
            source: ReplacementSource::Generic,
            replacement: ReplacementPolicy::Generic(audit.request),
        },
    );
    store
        .upsert_binding(&containment)
        .expect("containment should seed");
    store
        .begin_transition(&transition)
        .expect("reverse transition should seed");
    let client = ScriptedClient::with_replacements(
        vec![containment.clone()],
        (0..5)
            .map(|attempt| {
                if attempt % 2 == 0 {
                    Ok(ReplaceOutcome::SourceRetained {
                        binding: containment.clone(),
                        code: ReplaceFailureCode::KernelFailure,
                    })
                } else {
                    Ok(ReplaceOutcome::SourceRestored {
                        binding: containment.clone(),
                        code: ReplaceFailureCode::KernelFailure,
                    })
                }
            })
            .collect(),
    );

    for _ in 0..5 {
        assert!(matches!(
            execute_transition(
                &client,
                &store,
                store.transition(&transition.key).unwrap().unwrap(),
                uuid::Uuid::new_v4(),
            ),
            Err(EnforcementCoordinatorError::TransitionUnavailable)
        ));
    }

    let retained = store.transition(&transition.key).unwrap().unwrap();
    assert_eq!(retained.phase, TransitionPhase::Pending);
    assert_eq!(retained.acknowledgement, Some(containment));
    assert!(
        client
            .replace_results
            .lock()
            .expect("replace results should lock")
            .is_empty(),
        "each retry must issue a new privileged replacement"
    );
}

#[test]
fn recovered_ack_from_a_superseded_worker_stays_indeterminate() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let source = enforced(request("00000000-0000-0000-0000-000000000061"), 7);
    let target = enforced(request("00000000-0000-0000-0000-000000000062"), 7);
    let transition = transition(source.clone(), target.request.clone());
    store.upsert_binding(&source).expect("source should seed");
    store
        .begin_transition(&transition)
        .expect("transition should seed");
    let client = ScriptedClient::with_replacements(
        vec![target.clone()],
        vec![Ok(ReplaceOutcome::Applied(target.clone()))],
    );

    let result = execute_transition_fenced(
        &client,
        &store,
        transition.clone(),
        uuid::Uuid::new_v4(),
        &mut |_store, _key, _outcome| Ok(false),
    );

    assert!(matches!(
        result,
        Err(EnforcementCoordinatorError::TransitionUnavailable)
    ));
    assert_eq!(
        store.transition(&transition.key).unwrap().unwrap().phase,
        TransitionPhase::Indeterminate
    );
    assert_eq!(
        store.binding(source.request.binding_id).unwrap(),
        Some(source)
    );
    assert_eq!(store.binding(target.request.binding_id).unwrap(), None);
}

#[test]
fn remote_rejection_fails_only_one_binding_and_allows_readiness() {
    let database = TestDatabase::new();
    let store = EnforcementStore::open(&database.path).expect("test store should open");
    let rejected_request = request("00000000-0000-0000-0000-000000000001");
    let accepted_request = request("00000000-0000-0000-0000-000000000002");
    store
        .upsert_binding(&desired(rejected_request.clone()))
        .expect("rejected desired binding should seed");
    store
        .upsert_binding(&desired(accepted_request.clone()))
        .expect("accepted desired binding should seed");
    let client = ScriptedClient::new(vec![
        Err(EnforcementError::Remote {
            code: "kernel_failure".into(),
            message: "/root/secret.txt\npolicy label CREDENTIAL\u{7}".into(),
        }),
        Ok(enforced(accepted_request.clone(), 7)),
    ]);
    reconcile_desired_state(&client, &store, uuid::Uuid::new_v4())
        .expect("one rejection should not abort the loop");

    assert_eq!(
        client.applied(),
        vec![rejected_request.binding_id, accepted_request.binding_id]
    );
    let rejected = store
        .binding(rejected_request.binding_id)
        .expect("rejected binding should load")
        .expect("rejected binding should remain persisted");
    assert_eq!(rejected.state, BindingState::Failed);
    assert_eq!(
        rejected.message.as_deref(),
        Some("enforcer rejected desired binding: kernel attachment failed")
    );
    assert_eq!(rejected.domain_id, None);
    assert_eq!(
        store
            .binding(accepted_request.binding_id)
            .expect("accepted binding should load"),
        Some(enforced(accepted_request, 7))
    );

    let connection = Connection::open(&database.path).expect("raw database should open");
    let raw: (String, String, Option<String>, Option<i64>) = connection
        .query_row(
            "SELECT desired_json, state, message, domain_id
             FROM enforcement_bindings WHERE binding_id = ?1",
            params![rejected_request.binding_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("raw rejected binding should load");
    assert_eq!(serde_json::from_str::<Binding>(&raw.0).unwrap(), rejected);
    assert_eq!(raw.1, "failed");
    assert_eq!(raw.2, rejected.message);
    assert_eq!(raw.3, None);
}

#[test]
fn remote_operator_message_is_bounded_utf8_safe_and_redacted() {
    let oversized = format!("/root/secret.txt {}\npolicy label SECRET", "界".repeat(600));
    let message = remote_rejection_message("kernel_failure", &oversized);

    assert!(message.chars().count() <= 512);
    assert!(message.is_char_boundary(message.len()));
    assert!(!message.chars().any(char::is_control));
    assert!(!message.contains("/root/secret.txt"));
    assert!(!message.contains("policy label SECRET"));
    assert!(!message.contains('界'));
}

#[test]
fn operator_message_sanitizer_replaces_controls_and_has_empty_fallback() {
    assert_eq!(
        sanitize_operator_message("line one\nline two\u{7}"),
        "line one line two"
    );
    assert_eq!(
        sanitize_operator_message("\n\u{7}\t"),
        "enforcer rejected desired binding without operator-safe detail"
    );
    assert_eq!(
        sanitize_operator_message(&"界".repeat(600)).chars().count(),
        512
    );
}

fn reconcile_apply_error(error: EnforcementError) -> EnforcementCoordinatorError {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let request = request("00000000-0000-0000-0000-000000000010");
    store
        .upsert_binding(&desired(request))
        .expect("desired binding should seed");
    reconcile_desired_state(
        &ScriptedClient::new(vec![Err(error)]),
        &store,
        uuid::Uuid::new_v4(),
    )
    .expect_err("non-remote errors should abort the generation")
}

#[test]
fn io_protocol_and_response_mismatch_remain_generation_fatal() {
    assert!(matches!(
        reconcile_apply_error(EnforcementError::Io(std::io::Error::other(
            "fixture transport failure"
        ))),
        EnforcementCoordinatorError::Client(EnforcementError::Io(_))
    ));
    assert!(matches!(
        reconcile_apply_error(EnforcementError::Protocol(ProtocolError::MissingNewline)),
        EnforcementCoordinatorError::Client(EnforcementError::Protocol(_))
    ));
    assert!(matches!(
        reconcile_apply_error(EnforcementError::ResponseMismatch {
            expected: uuid::Uuid::new_v4(),
            actual: uuid::Uuid::new_v4(),
        }),
        EnforcementCoordinatorError::Client(EnforcementError::ResponseMismatch { .. })
    ));
    assert!(matches!(
        reconcile_apply_error(EnforcementError::Remote {
            code: "required_subscription_unavailable".into(),
            message: "subscription generation changed".into(),
        }),
        EnforcementCoordinatorError::Client(EnforcementError::Remote { code, .. })
            if code == "required_subscription_unavailable"
    ));
}

#[test]
fn acknowledgement_store_failure_remains_generation_fatal() {
    let store = EnforcementStore::open(":memory:").expect("test store should open");
    let request = request("00000000-0000-0000-0000-000000000020");
    store
        .upsert_binding(&desired(request.clone()))
        .expect("desired binding should seed");
    let mut conflicting = request;
    conflicting.policy_revision = "conflicting-revision".into();

    let result = reconcile_desired_state(
        &ScriptedClient::new(vec![Ok(enforced(conflicting, 9))]),
        &store,
        uuid::Uuid::new_v4(),
    );

    assert!(matches!(
        result,
        Err(EnforcementCoordinatorError::Store(
            EnforcementStoreError::BindingConflict(_)
        ))
    ));
}
