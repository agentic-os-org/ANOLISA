//! Public extension contracts exercised from outside the library crate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use semantic_arbiter::composition::{
    compose_outcome, CapabilityRegistry, CompositionError, DirectiveCapability, OutcomeContribution,
};
use semantic_arbiter::config::ModelConfig;
use semantic_arbiter::contract::{
    AnswerValue, Binding, EvidenceFact, Reason, RebuildResult, Scenario, UnitAnswer,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{
    validate_response, EvaluationRequest, Evaluator, EVALUATION_API_VERSION,
};
use semantic_arbiter::executor::fixture::{InMemoryClaimStore, ScriptedExecutor};
use semantic_arbiter::executor::{
    idempotency_key, AuthorizationContext, EffectState, ExecutionError, ExecutorRegistry,
};
use semantic_arbiter::judgment::{JudgmentUnit, JudgmentUnitRegistry, TaskScopeUnit};
use semantic_arbiter::model::{FixtureBackend, ModelReply, NoulProbability, NoulQuestion};
use semantic_arbiter::outcome::{
    CandidateControl, Directive, Obligation, ObligationAction, ReceiptState, SubjectAction,
    SubjectDirective,
};
use serde_json::{json, Value};

struct PreserveUnit;

impl JudgmentUnit for PreserveUnit {
    fn scenario(&self) -> Scenario {
        Scenario::Preservation
    }

    fn revision(&self) -> &'static str {
        "custom-preservation-r1"
    }

    fn question_id(&self) -> &'static str {
        "custom-preservation-v1"
    }

    fn question(&self) -> NoulQuestion {
        NoulQuestion::new("Return preservation support for this fixture.".into())
    }

    fn decide(
        &self,
        baseline: &UnitAnswer,
        probability: &NoulProbability,
        _threshold: u32,
    ) -> UnitAnswer {
        UnitAnswer {
            unit: baseline.unit,
            value: AnswerValue::Preserve,
            confidence_ppm: probability.yes_ppm(),
            reason: Reason::Preserved,
            evidence_refs: baseline.evidence_refs.clone(),
        }
    }
}

struct DowngradeSnapshotUnit;

impl JudgmentUnit for DowngradeSnapshotUnit {
    fn scenario(&self) -> Scenario {
        Scenario::Preservation
    }

    fn revision(&self) -> &'static str {
        "downgrade-snapshot-r1"
    }

    fn question_id(&self) -> &'static str {
        "downgrade-snapshot-v1"
    }

    fn question(&self) -> NoulQuestion {
        NoulQuestion::new("Attempt to downgrade a snapshot requirement.".into())
    }

    fn decide(
        &self,
        baseline: &UnitAnswer,
        _probability: &NoulProbability,
        _threshold: u32,
    ) -> UnitAnswer {
        UnitAnswer {
            unit: baseline.unit,
            value: AnswerValue::Allow,
            confidence_ppm: 1_000_000,
            reason: Reason::Allowed,
            evidence_refs: baseline.evidence_refs.clone(),
        }
    }
}

struct MutableUnit {
    preserve: AtomicBool,
    revision: &'static str,
}

impl MutableUnit {
    fn new(revision: &'static str) -> Self {
        Self {
            preserve: AtomicBool::new(false),
            revision,
        }
    }
}

impl JudgmentUnit for MutableUnit {
    fn scenario(&self) -> Scenario {
        Scenario::Preservation
    }

    fn revision(&self) -> &'static str {
        self.revision
    }

    fn question_id(&self) -> &'static str {
        "mutable-preservation-v1"
    }

    fn question(&self) -> NoulQuestion {
        NoulQuestion::new("Return mutable preservation support for this fixture.".into())
    }

    fn decide(
        &self,
        baseline: &UnitAnswer,
        probability: &NoulProbability,
        _threshold: u32,
    ) -> UnitAnswer {
        if self.preserve.load(Ordering::SeqCst) {
            UnitAnswer {
                unit: baseline.unit,
                value: AnswerValue::Preserve,
                confidence_ppm: probability.yes_ppm(),
                reason: Reason::Preserved,
                evidence_refs: baseline.evidence_refs.clone(),
            }
        } else {
            baseline.clone()
        }
    }
}

struct InvalidUnit;

impl JudgmentUnit for InvalidUnit {
    fn scenario(&self) -> Scenario {
        Scenario::Preservation
    }

    fn revision(&self) -> &'static str {
        "invalid-preservation-r1"
    }

    fn question_id(&self) -> &'static str {
        "invalid-preservation-v1"
    }

    fn question(&self) -> NoulQuestion {
        NoulQuestion::new("Return an invalid fixture answer.".into())
    }

    fn decide(
        &self,
        baseline: &UnitAnswer,
        _probability: &NoulProbability,
        _threshold: u32,
    ) -> UnitAnswer {
        UnitAnswer {
            unit: baseline.unit,
            value: AnswerValue::OutsideScope,
            confidence_ppm: 1_000_000,
            reason: Reason::OutsideScope,
            evidence_refs: baseline.evidence_refs.clone(),
        }
    }
}

fn binding() -> Binding {
    Binding {
        session_id: "session".into(),
        instance_id: "instance".into(),
        workspace_id: "workspace".into(),
        task_version: "task-v1".into(),
        config_revision: "policy-v1".into(),
        candidate_id: "candidate".into(),
        candidate_revision: "candidate-v1".into(),
    }
}

fn config() -> ModelConfig {
    ModelConfig::decode(
        &serde_json::to_vec(&json!({
            "config_revision": "policy-v1",
            "backend": {
                "kind": "bailian",
                "endpoint_url": "https://model.invalid/extension-contract",
                "model": "fixture-model",
                "key_env": "SEMANTIC_ARBITER_UNUSED_EXTENSION_KEY"
            },
            "timeout_ms": 10000,
            "max_response_bytes": 8192,
            "thresholds": {
                "preservation_ppm": 900000,
                "task_scope_ppm": 900000
            },
            "transport": "https"
        }))
        .unwrap(),
    )
    .unwrap()
}

fn request() -> EvaluationRequest {
    let mut value: Value =
        serde_json::from_slice(include_bytes!("../examples/preservation-allow.json")).unwrap();
    value.as_object_mut().unwrap().remove("check_at_ms");
    value["api_version"] = json!(EVALUATION_API_VERSION);
    value["mode"] = json!("model_evaluation");
    value["scenarios"] = json!(["preservation"]);
    let now = Ingress::capture().unwrap().now_ms();
    value["budget"]["opened_at_ms"] = json!(now.saturating_sub(1));
    value["budget"]["deadline_ms"] = json!(now + 10_000);
    value["evidence"][0]["captured_at_ms"] = json!(now);
    value["evidence"][0]["valid_until_ms"] = json!(now + 20_000);
    EvaluationRequest::decode(&serde_json::to_vec(&value).unwrap()).unwrap()
}

fn registry(unit: Arc<dyn JudgmentUnit>) -> JudgmentUnitRegistry {
    JudgmentUnitRegistry::new(unit, Arc::new(TaskScopeUnit)).unwrap()
}

#[tokio::test]
async fn registered_unit_replaces_only_its_closed_scenario() {
    let reply = ModelReply {
        answers: BTreeMap::from([(
            "custom-preservation-v1".into(),
            NoulProbability::new(1.0).unwrap(),
        )]),
    };
    let run = Evaluator::with_units(
        config(),
        FixtureBackend::new(Ok(reply)),
        registry(Arc::new(PreserveUnit)),
    )
    .unwrap()
    .evaluate(&request())
    .await
    .unwrap();

    assert_eq!(run.response.answers[0].value, AnswerValue::Preserve);
    assert_eq!(
        run.response.outcome.control.candidate,
        CandidateControl::Block
    );
    assert_eq!(
        run.response.judgments[0].question_id,
        "custom-preservation-v1"
    );
    assert_eq!(
        run.response.judgments[0].unit_revision,
        "custom-preservation-r1"
    );
}

#[tokio::test]
async fn response_validation_freezes_unit_decisions_and_binds_revisions() {
    let request = request();
    let unit = Arc::new(MutableUnit::new("mutable-preservation-r1"));
    let reply = || ModelReply {
        answers: BTreeMap::from([(
            "mutable-preservation-v1".into(),
            NoulProbability::new(1.0).unwrap(),
        )]),
    };
    let first = Evaluator::with_units(
        config(),
        FixtureBackend::new(Ok(reply())),
        registry(unit.clone()),
    )
    .unwrap()
    .evaluate(&request)
    .await
    .unwrap();

    assert_eq!(first.response.answers[0].value, AnswerValue::Allow);
    unit.preserve.store(true, Ordering::SeqCst);
    validate_response(&first.response, &request, &config(), first.record()).unwrap();

    let second = Evaluator::with_units(
        config(),
        FixtureBackend::new(Ok(reply())),
        registry(Arc::new(MutableUnit::new("mutable-preservation-r2"))),
    )
    .unwrap()
    .evaluate(&request)
    .await
    .unwrap();
    assert_ne!(first.response.config_digest, second.response.config_digest);
}

#[tokio::test]
async fn invalid_custom_answer_is_rejected_centrally() {
    let reply = ModelReply {
        answers: BTreeMap::from([(
            "invalid-preservation-v1".into(),
            NoulProbability::new(1.0).unwrap(),
        )]),
    };
    let result = Evaluator::with_units(
        config(),
        FixtureBackend::new(Ok(reply)),
        registry(Arc::new(InvalidUnit)),
    )
    .unwrap()
    .evaluate(&request())
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn custom_unit_cannot_remove_deterministic_snapshot_protection() {
    let mut request = request();
    request.evidence[0].fact = EvidenceFact::Rebuild {
        resource_id: request.candidate.resource_id.clone(),
        result: RebuildResult::Unique,
    };
    let reply = ModelReply {
        answers: BTreeMap::from([(
            "downgrade-snapshot-v1".into(),
            NoulProbability::new(0.0).unwrap(),
        )]),
    };
    let result = Evaluator::with_units(
        config(),
        FixtureBackend::new(Ok(reply)),
        registry(Arc::new(DowngradeSnapshotUnit)),
    )
    .unwrap()
    .evaluate(&request)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn deterministic_hard_block_never_calls_custom_decision() {
    let mut request = request();
    request
        .task
        .as_mut()
        .unwrap()
        .preserve_resource_ids
        .push(request.candidate.resource_id.clone());
    let run = Evaluator::with_units(
        config(),
        FixtureBackend::new(Ok(ModelReply {
            answers: BTreeMap::new(),
        })),
        registry(Arc::new(InvalidUnit)),
    )
    .unwrap()
    .evaluate(&request)
    .await
    .unwrap();

    assert_eq!(run.response.answers[0].value, AnswerValue::Preserve);
    assert_eq!(
        run.response.outcome.control.candidate,
        CandidateControl::Block
    );
    assert!(!run.response.model_called);
}

#[test]
fn composition_is_strict_deterministic_and_deduplicated() {
    let capabilities = CapabilityRegistry::new([
        DirectiveCapability::Audit,
        DirectiveCapability::Snapshot,
        DirectiveCapability::Freeze,
    ]);
    let freeze = SubjectDirective {
        id: "freeze".into(),
        instance_id: "instance".into(),
        action: SubjectAction::Freeze,
        scope_ids: vec!["workspace".into()],
    };
    let snapshot = Obligation {
        id: "snapshot".into(),
        critical: true,
        action: ObligationAction::Snapshot {
            workspace_id: "workspace".into(),
        },
    };
    let outcome = compose_outcome(
        &binding(),
        &capabilities,
        [
            OutcomeContribution {
                candidate: CandidateControl::Allow,
                subjects: vec![freeze.clone()],
                obligations: vec![snapshot.clone()],
                evidence_refs: vec!["b".into(), "a".into()],
            },
            OutcomeContribution {
                candidate: CandidateControl::Block,
                subjects: vec![freeze],
                obligations: vec![snapshot],
                evidence_refs: vec!["a".into()],
            },
        ],
    )
    .unwrap();

    assert_eq!(outcome.control.candidate, CandidateControl::Block);
    assert_eq!(outcome.control.subject.len(), 1);
    assert_eq!(outcome.evidence_refs, ["a", "b"]);
    assert!(matches!(
        outcome.obligations[0].action,
        ObligationAction::Snapshot { .. }
    ));
    assert!(matches!(
        outcome.obligations[1].action,
        ObligationAction::Audit {}
    ));
}

#[test]
fn empty_composition_needs_no_executor_capability() {
    let outcome = compose_outcome(
        &binding(),
        &CapabilityRegistry::default(),
        std::iter::empty(),
    )
    .unwrap();

    assert_eq!(outcome.control.candidate, CandidateControl::Nop);
    assert!(outcome.control.subject.is_empty());
    assert!(outcome.obligations.is_empty());
    assert!(outcome.evidence_refs.is_empty());
}

#[test]
fn composition_rejects_missing_capability_and_conflicting_ids() {
    let no_snapshot = CapabilityRegistry::new([DirectiveCapability::Audit]);
    let snapshot = Obligation {
        id: "shared".into(),
        critical: true,
        action: ObligationAction::Snapshot {
            workspace_id: "workspace".into(),
        },
    };
    assert_eq!(
        compose_outcome(
            &binding(),
            &no_snapshot,
            [OutcomeContribution {
                candidate: CandidateControl::Allow,
                subjects: vec![],
                obligations: vec![snapshot.clone()],
                evidence_refs: vec![],
            }],
        ),
        Err(CompositionError::CapabilityUnavailable)
    );

    let capabilities =
        CapabilityRegistry::new([DirectiveCapability::Audit, DirectiveCapability::Snapshot]);
    let conflicting = Obligation {
        id: "shared".into(),
        critical: true,
        action: ObligationAction::Audit {},
    };
    assert_eq!(
        compose_outcome(
            &binding(),
            &capabilities,
            [OutcomeContribution {
                candidate: CandidateControl::Allow,
                subjects: vec![],
                obligations: vec![snapshot, conflicting],
                evidence_refs: vec![],
            }],
        ),
        Err(CompositionError::DirectiveConflict)
    );
}

fn authorization(capabilities: CapabilityRegistry) -> AuthorizationContext {
    AuthorizationContext {
        api_version: "semantic-arbiter/v1alpha1".into(),
        request_id: "request".into(),
        binding: binding(),
        expires_at_ms: 2_000,
        capabilities,
        allowed_ids: BTreeSet::from(["workspace".into()]),
    }
}

fn audit() -> Directive {
    Directive::Obligation(Obligation {
        id: "audit".into(),
        critical: true,
        action: ObligationAction::Audit {},
    })
}

#[tokio::test]
async fn executor_claims_once_and_queries_unknown_without_replay() {
    let claims = Arc::new(InMemoryClaimStore::default());
    let executor = Arc::new(ScriptedExecutor::new(
        DirectiveCapability::Audit,
        EffectState::Unconfirmed,
    ));
    let mut registry = ExecutorRegistry::new(claims);
    registry.register(executor.clone()).unwrap();
    let authorization = authorization(CapabilityRegistry::new([DirectiveCapability::Audit]));

    let first = registry
        .execute(&authorization, &audit(), 1_000)
        .await
        .unwrap();
    let second = registry
        .execute(&authorization, &audit(), 1_001)
        .await
        .unwrap();

    assert!(first.dispatched);
    assert!(!second.dispatched);
    assert_eq!(first.receipt.state, ReceiptState::Unknown);
    assert_eq!(second.receipt.state, ReceiptState::Unknown);
    assert_eq!(executor.dispatch_count(), 1);
    assert_eq!(executor.query_count(), 2);

    executor.set_effect(EffectState::Confirmed).unwrap();
    let third = registry
        .execute(&authorization, &audit(), 1_002)
        .await
        .unwrap();
    assert!(!third.dispatched);
    assert_eq!(third.receipt.state, ReceiptState::Applied);
    assert_eq!(executor.dispatch_count(), 1);
}

#[tokio::test]
async fn executor_denies_unapproved_capability_before_claim() {
    let claims = Arc::new(InMemoryClaimStore::default());
    let executor = Arc::new(ScriptedExecutor::new(
        DirectiveCapability::Audit,
        EffectState::Confirmed,
    ));
    let mut registry = ExecutorRegistry::new(claims);
    registry.register(executor.clone()).unwrap();

    assert_eq!(
        registry
            .execute(
                &authorization(CapabilityRegistry::default()),
                &audit(),
                1_000,
            )
            .await,
        Err(ExecutionError::AdmissionDenied)
    );
    assert_eq!(executor.dispatch_count(), 0);
    assert_eq!(executor.query_count(), 0);
}

#[tokio::test]
async fn executor_rejects_unsupported_versions_and_empty_subject_scopes() {
    let claims = Arc::new(InMemoryClaimStore::default());
    let executor = Arc::new(ScriptedExecutor::new(
        DirectiveCapability::Audit,
        EffectState::Confirmed,
    ));
    let mut registry = ExecutorRegistry::new(claims);
    registry.register(executor.clone()).unwrap();

    let mut unsupported = authorization(CapabilityRegistry::new([DirectiveCapability::Audit]));
    unsupported.api_version = "semantic-arbiter/v9".into();
    assert_eq!(
        registry.execute(&unsupported, &audit(), 1_000).await,
        Err(ExecutionError::AdmissionDenied)
    );

    let empty_scope = Directive::Subject(SubjectDirective {
        id: "freeze".into(),
        instance_id: "instance".into(),
        action: SubjectAction::Freeze,
        scope_ids: vec![String::new()],
    });
    let mut empty_authorization = authorization(CapabilityRegistry::new([
        DirectiveCapability::Audit,
        DirectiveCapability::Freeze,
    ]));
    empty_authorization.allowed_ids.insert(String::new());
    assert_eq!(
        registry
            .execute(&empty_authorization, &empty_scope, 1_000)
            .await,
        Err(ExecutionError::AdmissionDenied)
    );
    assert_eq!(executor.dispatch_count(), 0);
    assert_eq!(executor.query_count(), 0);
}

#[test]
fn executor_idempotency_is_bound_to_protocol_version() {
    let directive = audit();
    let v1 = idempotency_key(
        "semantic-arbiter/v1alpha1",
        "request",
        &binding(),
        &directive,
    )
    .unwrap();
    let v2 = idempotency_key(EVALUATION_API_VERSION, "request", &binding(), &directive).unwrap();

    assert_ne!(v1, v2);
}
