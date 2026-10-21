//! Model evaluation contracts exercised through explicit, observable in-process fixtures.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use semantic_arbiter::config::ModelConfig;
use semantic_arbiter::contract::{
    AnswerValue, EvidenceFact, EvidenceGap, Provenance, Reason, RebuildResult, Scenario,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{
    decode_response, input_digest, validate_response, EvaluationRequest, EvaluationRun, Evaluator,
    EVALUATION_API_VERSION,
};
use semantic_arbiter::model::{
    BackendIdentity, BackendKind, BackendMode, FixtureBackend, ModelBackend, ModelError,
    ModelReply, ModelRequest, NoulProbability, NORMALIZATION_VERSION,
};
use semantic_arbiter::outcome::{CandidateControl, ObligationAction};
use semantic_arbiter::protocol;
use serde_json::{json, Value};
use tokio::time::Instant;

const PRESERVATION: &str = "preservation-v1";
const TASK_SCOPE: &str = "task-scope-v1";

fn config() -> ModelConfig {
    ModelConfig::decode(
        &serde_json::to_vec(&json!({
            "config_revision": "policy-v1",
            "backend": {
                "kind": "bailian", "endpoint_url": "https://model.invalid/exact-test-path",
                "model": "contract-test-model", "key_env": "SEMANTIC_ARBITER_CONTRACT_UNUSED_CREDENTIAL_13A9"
            },
            "timeout_ms": 10000, "max_response_bytes": 8192,
            "thresholds": {"preservation_ppm": 900000, "task_scope_ppm": 900000}, "transport": "https"
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
    value["scenarios"] = json!(["preservation", "task_scope"]);
    let now = Ingress::capture().unwrap().now_ms();
    value["budget"]["opened_at_ms"] = json!(now - 1000);
    value["budget"]["deadline_ms"] = json!(now + 10000);
    value["evidence"][0]["captured_at_ms"] = json!(now - 500);
    value["evidence"][0]["valid_until_ms"] = json!(now + 20000);
    EvaluationRequest::decode(&serde_json::to_vec(&value).unwrap()).unwrap()
}

fn reply(probabilities: &[(&str, f64)]) -> ModelReply {
    ModelReply {
        answers: probabilities
            .iter()
            .map(|(id, probability)| {
                (
                    (*id).to_owned(),
                    NoulProbability::new(*probability).unwrap(),
                )
            })
            .collect(),
    }
}

#[derive(Clone, Copy)]
enum Behavior {
    Immediate,
    SleepUntil(Instant),
    Pending,
    ReturnAfterDeadline,
}

#[derive(Clone, Default)]
struct Observations {
    calls: Arc<Mutex<Vec<(ModelRequest, Instant)>>>,
    completed: Arc<AtomicBool>,
}

impl Observations {
    fn calls(&self) -> Vec<(ModelRequest, Instant)> {
        self.calls.lock().unwrap().clone()
    }
}

struct ProbeBackend {
    identity: BackendIdentity,
    result: Result<ModelReply, ModelError>,
    behavior: Behavior,
    observations: Observations,
}

impl ProbeBackend {
    fn new(result: Result<ModelReply, ModelError>) -> (Self, Observations) {
        let observations = Observations::default();
        (
            Self {
                identity: BackendIdentity {
                    kind: BackendKind::Fixture,
                    mode: BackendMode::Fixture,
                    model: "observable-fixture".into(),
                },
                result,
                behavior: Behavior::Immediate,
                observations: observations.clone(),
            },
            observations,
        )
    }
}

impl ModelBackend for ProbeBackend {
    fn identity(&self) -> BackendIdentity {
        self.identity.clone()
    }

    fn infer<'a>(
        &'a self,
        request: &'a ModelRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<ModelReply, ModelError>> + Send + 'a>> {
        self.observations
            .calls
            .lock()
            .unwrap()
            .push((request.clone(), deadline));
        Box::pin(async move {
            match self.behavior {
                Behavior::Immediate => {}
                Behavior::SleepUntil(at) => tokio::time::sleep_until(at).await,
                Behavior::Pending => std::future::pending::<()>().await,
                Behavior::ReturnAfterDeadline => {
                    // A non-cooperative backend may finish without yielding to timeout_at.
                    // The evaluator must still discard a favorable but late observation.
                    std::thread::sleep(
                        (deadline + Duration::from_millis(30))
                            .saturating_duration_since(Instant::now()),
                    );
                }
            }
            self.observations.completed.store(true, Ordering::SeqCst);
            self.result.clone()
        })
    }
}

async fn evaluate(
    request: &EvaluationRequest,
    probabilities: &[(&str, f64)],
) -> (EvaluationRun, Observations) {
    let (backend, observations) = ProbeBackend::new(Ok(reply(probabilities)));
    let config = config();
    let run = Evaluator::new(config.clone(), backend)
        .unwrap()
        .evaluate(request)
        .await
        .unwrap();
    validate_response(&run.response, request, &config, run.record()).unwrap();
    (run, observations)
}

fn assert_answers(run: &EvaluationRun, expected: &[AnswerValue], control: CandidateControl) {
    assert_eq!(
        run.response
            .answers
            .iter()
            .map(|answer| answer.value)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(run.response.outcome.control.candidate, control);
    assert!(run.response.outcome.control.subject.is_empty());
    assert!(run.response.outcome.obligations.iter().any(|obligation| {
        obligation.critical && matches!(obligation.action, ObligationAction::Audit {})
    }));
}

#[tokio::test]
async fn model_changes_answers_for_identical_structured_input_in_one_combined_call() {
    let request = request();
    let cases = [
        (
            0.0,
            1.0,
            [AnswerValue::Allow, AnswerValue::Allow],
            CandidateControl::Allow,
        ),
        (
            1.0,
            1.0,
            [AnswerValue::Preserve, AnswerValue::Allow],
            CandidateControl::Block,
        ),
        (
            0.0,
            0.0,
            [AnswerValue::Allow, AnswerValue::OutsideScope],
            CandidateControl::Block,
        ),
        (
            0.5,
            0.5,
            [AnswerValue::Unknown, AnswerValue::Unknown],
            CandidateControl::Nop,
        ),
    ];
    let mut digest = None;
    for (preservation, task_scope, expected, control) in cases {
        let (run, observations) = evaluate(
            &request,
            &[(PRESERVATION, preservation), (TASK_SCOPE, task_scope)],
        )
        .await;
        assert_answers(&run, &expected, control);
        assert!(run.response.model_called);
        assert_eq!(observations.calls().len(), 1);
        assert_eq!(
            observations.calls()[0]
                .0
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [PRESERVATION, TASK_SCOPE]
        );
        assert_eq!(run.response.normalization_revision, NORMALIZATION_VERSION);
        for (trace, raw) in run
            .response
            .judgments
            .iter()
            .zip([preservation, task_scope])
        {
            assert_eq!(trace.probability.as_ref().unwrap().raw(), raw);
            assert_eq!(trace.threshold_ppm, 900000);
            assert_eq!(trace.error, None);
            assert!(trace.model_used);
        }
        let expected_digest = digest.get_or_insert_with(|| run.response.input_digest.clone());
        assert_eq!(run.response.input_digest.as_str(), expected_digest.as_str());
    }
}

#[tokio::test]
async fn projection_preserves_task_candidate_and_selected_evidence_provenance_only() {
    let mut request = request();
    request.task.as_mut().unwrap().text =
        "Keep the previous report; replace only the generated copy.".into();
    let selected = request.evidence[0].clone();
    let mut unrelated = selected.clone();
    unrelated.id = "unrelated-local-proof".into();
    unrelated.fact = EvidenceFact::Rebuild {
        resource_id: "unrelated-resource".into(),
        result: RebuildResult::Identical,
    };
    let mut untrusted = selected.clone();
    untrusted.id = "untrusted-report".into();
    untrusted.provenance = Provenance::SourceReported;
    let mut echo = selected.clone();
    echo.id = "model-echo".into();
    echo.provenance = Provenance::ModelEcho;
    request.evidence.extend([unrelated, untrusted, echo]);
    let (run, observations) = evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
    let calls = observations.calls();
    assert_eq!(calls.len(), 1);
    let projected = &calls[0].0;
    assert_eq!(
        projected.state,
        json!({
            "authoritative_task": request.task,
            "candidate": request.candidate,
            "evidence": [selected]
        })
    );
    for (id, question) in &projected.questions {
        let question = serde_json::to_value(question).unwrap();
        assert_eq!(question["type"], "noul");
        assert_eq!(question.as_object().unwrap().len(), 2);
        let text = question["instructions"].as_str().unwrap();
        assert!(text.contains("authoritative_task.text"));
        assert!(text.contains("candidate.operation"));
        assert!(!text.contains(request.task.as_ref().unwrap().text.as_str()));
        assert!(id == PRESERVATION || id == TASK_SCOPE);
    }
    assert_eq!(run.response.answers[0].evidence_refs, ["rebuild-1"]);
    assert!(run.response.answers[1].evidence_refs.is_empty());
    assert_eq!(run.response.outcome.evidence_refs, ["rebuild-1"]);
}

#[tokio::test]
async fn hard_task_constraints_skip_inference_even_with_favorable_fixture_answers() {
    for (case, expected, reason) in [
        ("preserve", AnswerValue::Preserve, Reason::Preserved),
        ("outside", AnswerValue::OutsideScope, Reason::OutsideScope),
        ("missing", AnswerValue::Unknown, Reason::MissingTask),
    ] {
        let mut request = request();
        match case {
            "preserve" => request
                .task
                .as_mut()
                .unwrap()
                .preserve_resource_ids
                .push(request.candidate.resource_id.clone()),
            "outside" => request.task.as_mut().unwrap().allowed_resource_ids.clear(),
            "missing" => request.task = None,
            _ => unreachable!(),
        }
        let (run, observations) =
            evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
        assert_answers(&run, &[expected, expected], CandidateControl::Block);
        assert!(!run.response.model_called, "{case}");
        assert!(observations.calls().is_empty(), "{case}");
        assert!(run
            .response
            .answers
            .iter()
            .all(|answer| answer.reason == reason));
        assert!(run.response.judgments.iter().all(|trace| !trace.model_used
            && trace.probability.is_none()
            && trace.error.is_none()));
    }
}

fn inhibit_preservation(request: &mut EvaluationRequest, case: &str) -> Reason {
    match case {
        "gap" => {
            request.gaps.push(EvidenceGap {
                source_id: "synthetic-verifier".into(),
                scenario: Scenario::Preservation,
            });
            Reason::EvidenceGap
        }
        "missing" => {
            request.evidence.clear();
            Reason::MissingEvidence
        }
        "stale" => {
            request.evidence[0].valid_until_ms = request.budget.opened_at_ms;
            request.evidence[0].captured_at_ms = request.budget.opened_at_ms - 1;
            Reason::StaleEvidence
        }
        "future" => {
            request.evidence[0].captured_at_ms = request.budget.deadline_ms;
            Reason::StaleEvidence
        }
        "conflicting" => {
            let mut conflict = request.evidence[0].clone();
            conflict.id = "conflicting-proof".into();
            conflict.fact = EvidenceFact::Rebuild {
                resource_id: request.candidate.resource_id.clone(),
                result: RebuildResult::Unique,
            };
            request.evidence.push(conflict);
            Reason::ConflictingEvidence
        }
        "reported" => {
            request.evidence[0].provenance = Provenance::SourceReported;
            Reason::UntrustedEvidence
        }
        "echo" => {
            request.evidence[0].provenance = Provenance::ModelEcho;
            Reason::UntrustedEvidence
        }
        "unknown" => {
            request.evidence[0].fact = EvidenceFact::Rebuild {
                resource_id: request.candidate.resource_id.clone(),
                result: RebuildResult::Unknown,
            };
            Reason::Undetermined
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn evidence_failures_are_not_overridden_and_only_eligible_units_enter_the_batch() {
    for case in [
        "gap",
        "missing",
        "stale",
        "future",
        "conflicting",
        "reported",
        "echo",
        "unknown",
    ] {
        for combined in [false, true] {
            let mut request = request();
            let expected_reason = inhibit_preservation(&mut request, case);
            if !combined {
                request.scenarios = vec![Scenario::Preservation];
            }
            let (run, observations) = evaluate(&request, &[(TASK_SCOPE, 1.0)]).await;
            assert_eq!(
                run.response.answers[0].value,
                AnswerValue::Unknown,
                "{case}"
            );
            assert_eq!(run.response.answers[0].reason, expected_reason, "{case}");
            assert_eq!(
                run.response.outcome.control.candidate,
                CandidateControl::Block
            );
            assert!(!run.response.judgments[0].model_used);
            assert!(run.response.judgments[0].probability.is_none());
            assert_eq!(run.response.model_called, combined);
            let calls = observations.calls();
            assert_eq!(calls.len(), usize::from(combined), "{case}");
            if combined {
                assert_eq!(
                    calls[0]
                        .0
                        .questions
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    [TASK_SCOPE]
                );
                assert_eq!(calls[0].0.state["evidence"], json!([]));
                assert_eq!(run.response.answers[1].value, AnswerValue::Allow);
            }
        }
    }
}

#[tokio::test]
async fn task_scope_gap_does_not_suppress_eligible_preservation_or_allow_scope_to_be_invented() {
    let mut request = request();
    request.gaps.push(EvidenceGap {
        source_id: "scope-observer".into(),
        scenario: Scenario::TaskScope,
    });
    let (run, observations) = evaluate(&request, &[(PRESERVATION, 0.0)]).await;
    assert_answers(
        &run,
        &[AnswerValue::Allow, AnswerValue::Unknown],
        CandidateControl::Block,
    );
    assert_eq!(run.response.answers[1].reason, Reason::EvidenceGap);
    assert_eq!(
        observations.calls()[0]
            .0
            .questions
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [PRESERVATION]
    );
    assert_eq!(
        observations.calls()[0].0.state["evidence"],
        serde_json::to_value(&request.evidence).unwrap()
    );
}

#[tokio::test]
async fn exhausted_evidence_budgets_skip_the_entire_batch() {
    for byte_limit in [false, true] {
        let mut request = request();
        if byte_limit {
            request.budget.max_evidence_bytes = 1;
        } else {
            request.budget.max_evidence_items = 0;
        }
        let (run, observations) =
            evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
        assert_answers(
            &run,
            &[AnswerValue::Unknown, AnswerValue::Unknown],
            CandidateControl::Block,
        );
        assert!(run
            .response
            .answers
            .iter()
            .all(|answer| answer.reason == Reason::EvidenceBudgetExceeded));
        assert!(!run.response.model_called);
        assert!(observations.calls().is_empty());
    }
}

#[tokio::test]
async fn selected_scenarios_define_the_exact_question_set_and_canonical_answer_order() {
    for scenarios in [
        vec![Scenario::Preservation],
        vec![Scenario::TaskScope],
        vec![Scenario::TaskScope, Scenario::Preservation],
    ] {
        let mut request = request();
        request.scenarios = scenarios.clone();
        let probabilities: Vec<_> = [
            (Scenario::Preservation, PRESERVATION, 0.0),
            (Scenario::TaskScope, TASK_SCOPE, 1.0),
        ]
        .into_iter()
        .filter(|(scenario, _, _)| scenarios.contains(scenario))
        .map(|(_, id, p)| (id, p))
        .collect();
        let (run, observations) = evaluate(&request, &probabilities).await;
        let ids: Vec<_> = probabilities.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            observations.calls()[0]
                .0
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            run.response
                .judgments
                .iter()
                .map(|trace| trace.question_id.as_str())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(run.response.answers.len(), scenarios.len());
    }
}

#[tokio::test]
async fn missing_extra_and_wrong_question_answers_fail_the_whole_batch_without_retry() {
    for probabilities in [
        vec![],
        vec![(PRESERVATION, 0.0)],
        vec![(TASK_SCOPE, 1.0)],
        vec![
            (PRESERVATION, 0.0),
            (TASK_SCOPE, 1.0),
            ("invented-question", 1.0),
        ],
        vec![("preservation", 0.0), ("task_scope", 1.0)],
    ] {
        let request = request();
        let (run, observations) = evaluate(&request, &probabilities).await;
        assert_answers(
            &run,
            &[AnswerValue::Unknown, AnswerValue::Unknown],
            CandidateControl::Nop,
        );
        assert_eq!(observations.calls().len(), 1);
        assert!(run
            .response
            .judgments
            .iter()
            .all(|trace| trace.error == Some(ModelError::InvalidResponse)
                && trace.probability.is_none()
                && !trace.model_used));
    }
}

#[tokio::test]
async fn configured_thresholds_are_inclusive_independent_and_conservatively_normalized() {
    // Binary-exact boundaries avoid relying on decimal floating-point rounding.
    let cases = [
        (
            0.749999,
            0.875,
            AnswerValue::Unknown,
            AnswerValue::Allow,
            CandidateControl::Allow,
        ),
        (
            0.75,
            0.875,
            AnswerValue::Preserve,
            AnswerValue::Allow,
            CandidateControl::Block,
        ),
        (
            0.25,
            0.125,
            AnswerValue::Allow,
            AnswerValue::OutsideScope,
            CandidateControl::Block,
        ),
        (
            0.250001,
            0.125001,
            AnswerValue::Unknown,
            AnswerValue::Unknown,
            CandidateControl::Nop,
        ),
        (
            0.0,
            0.874999,
            AnswerValue::Allow,
            AnswerValue::Unknown,
            CandidateControl::Allow,
        ),
    ];
    for (preservation, task_scope, expected_preservation, expected_task_scope, expected_control) in
        cases
    {
        let request = request();
        let mut config = config();
        config.thresholds.preservation_ppm = 750000;
        config.thresholds.task_scope_ppm = 875000;
        let evaluator = Evaluator::new(
            config.clone(),
            FixtureBackend::new(Ok(reply(&[
                (PRESERVATION, preservation),
                (TASK_SCOPE, task_scope),
            ]))),
        )
        .unwrap();
        let run = evaluator.evaluate(&request).await.unwrap();
        assert_eq!(run.response.answers[0].value, expected_preservation);
        assert_eq!(run.response.answers[1].value, expected_task_scope);
        assert_eq!(run.response.outcome.control.candidate, expected_control);
        assert_ne!(
            run.response.outcome.control.candidate,
            CandidateControl::Ask
        );
        assert_eq!(run.response.judgments[0].threshold_ppm, 750000);
        assert_eq!(run.response.judgments[1].threshold_ppm, 875000);
        for (answer, raw) in run.response.answers.iter().zip([preservation, task_scope]) {
            let p = NoulProbability::new(raw).unwrap();
            let expected_confidence = match answer.value {
                AnswerValue::Unknown => 0,
                AnswerValue::Preserve => p.yes_ppm(),
                AnswerValue::OutsideScope => p.no_ppm(),
                AnswerValue::Allow if answer.unit == Scenario::Preservation => p.no_ppm(),
                AnswerValue::Allow => p.yes_ppm(),
                AnswerValue::NeedsSnapshot => unreachable!(),
            };
            assert_eq!(answer.confidence_ppm, expected_confidence);
        }
        validate_response(&run.response, &request, &config, run.record()).unwrap();
    }
}

#[tokio::test]
async fn semantic_permission_keeps_the_local_snapshot_obligation_and_workspace_binding() {
    let mut request = request();
    request.evidence[0].fact = EvidenceFact::Rebuild {
        resource_id: request.candidate.resource_id.clone(),
        result: RebuildResult::Unique,
    };
    let (run, _) = evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
    assert_answers(
        &run,
        &[AnswerValue::NeedsSnapshot, AnswerValue::Allow],
        CandidateControl::Allow,
    );
    assert_eq!(run.response.answers[0].reason, Reason::NeedsSnapshot);
    assert_eq!(
        serde_json::to_value(&run.response.outcome.obligations).unwrap(),
        json!([
            {"id": "snapshot", "critical": true, "action": {"kind": "snapshot", "parameters": {"workspace_id": request.binding.workspace_id}}},
            {"id": "audit", "critical": true, "action": {"kind": "audit", "parameters": {}}}
        ])
    );
    for (probabilities, expected_control, expected_preservation) in [
        (
            [(PRESERVATION, 1.0), (TASK_SCOPE, 1.0)],
            CandidateControl::Block,
            AnswerValue::Preserve,
        ),
        (
            [(PRESERVATION, 0.0), (TASK_SCOPE, 0.0)],
            CandidateControl::Block,
            AnswerValue::NeedsSnapshot,
        ),
        (
            [(PRESERVATION, 0.5), (TASK_SCOPE, 1.0)],
            CandidateControl::Allow,
            AnswerValue::Unknown,
        ),
    ] {
        let (without_snapshot, _) = evaluate(&request, &probabilities).await;
        assert_eq!(
            without_snapshot.response.outcome.control.candidate,
            expected_control
        );
        assert_eq!(
            without_snapshot.response.answers[0].value,
            expected_preservation
        );
        assert!(!without_snapshot
            .response
            .outcome
            .obligations
            .iter()
            .any(|item| matches!(item.action, ObligationAction::Snapshot { .. })));
    }
}

#[tokio::test]
async fn every_backend_failure_becomes_unknown_nop_and_a_sanitized_trace_without_retry() {
    for error in [
        ModelError::Timeout,
        ModelError::Authentication,
        ModelError::RateLimited,
        ModelError::Transport,
        ModelError::Http,
        ModelError::InvalidResponse,
        ModelError::ResponseTooLarge,
        ModelError::MissingCredentials,
    ] {
        let request = request();
        let config = config();
        let (backend, observations) = ProbeBackend::new(Err(error));
        let run = Evaluator::new(config.clone(), backend)
            .unwrap()
            .evaluate(&request)
            .await
            .unwrap();
        assert_answers(
            &run,
            &[AnswerValue::Unknown, AnswerValue::Unknown],
            CandidateControl::Nop,
        );
        assert_eq!(observations.calls().len(), 1);
        assert!(run.response.model_called);
        assert!(run
            .response
            .answers
            .iter()
            .all(|answer| answer.reason == Reason::Undetermined && answer.confidence_ppm == 0));
        assert!(run
            .response
            .judgments
            .iter()
            .all(|trace| trace.error == Some(error)
                && trace.probability.is_none()
                && !trace.model_used));
        validate_response(&run.response, &request, &config, run.record()).unwrap();
    }
}

#[tokio::test]
async fn fixture_success_never_masquerades_as_remote_and_mismatched_adapter_identity_is_rejected() {
    let request = request();
    let config = config();
    let run = Evaluator::new(
        config.clone(),
        FixtureBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]))),
    )
    .unwrap()
    .evaluate(&request)
    .await
    .unwrap();
    assert_eq!(run.response.backend.kind, BackendKind::Fixture);
    assert_eq!(run.response.backend.mode, BackendMode::Fixture);
    assert_eq!(run.response.backend.model, "fixture");
    assert_eq!(
        run.response.outcome.control.candidate,
        CandidateControl::Allow
    );
    let mut forged = run.response.clone();
    forged.backend = config.identity();
    assert!(validate_response(&forged, &request, &config, run.record()).is_err());
    for identity in [
        BackendIdentity {
            kind: BackendKind::Fixture,
            mode: BackendMode::Remote,
            model: "fixture".into(),
        },
        BackendIdentity {
            kind: BackendKind::Bailian,
            mode: BackendMode::Fixture,
            model: "contract-test-model".into(),
        },
        BackendIdentity {
            kind: BackendKind::Typesafe,
            mode: BackendMode::Remote,
            model: "contract-test-model".into(),
        },
        BackendIdentity {
            kind: BackendKind::Bailian,
            mode: BackendMode::Remote,
            model: "other-model".into(),
        },
    ] {
        let (mut backend, observations) =
            ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
        backend.identity = identity;
        assert!(Evaluator::new(config.clone(), backend).is_err());
        assert!(observations.calls().is_empty());
    }
}

#[tokio::test]
async fn all_public_trace_and_outcome_fields_are_bound_to_the_retained_record() {
    let request = request();
    let config = config();
    let (mut run, observations) =
        evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
    let original = run.response.clone();
    let record = run.record().clone();
    let wire = serde_json::to_value(&original).unwrap();
    let mutations = [
        ("/api_version", json!("semantic-arbiter/v1alpha1")),
        ("/request_id", json!("another-request")),
        ("/mode", json!("deterministic_check")),
        ("/binding/candidate_revision", json!("candidate-v2")),
        ("/input_digest", json!("sha256:forged")),
        ("/config_digest", json!("sha256:forged")),
        ("/backend/kind", json!("bailian")),
        ("/backend/mode", json!("remote")),
        ("/backend/model", json!("another-model")),
        ("/normalization_revision", json!("unverified-normalization")),
        ("/evaluated_at_ms", json!(original.evaluated_at_ms + 1)),
        ("/elapsed_ms", json!(original.elapsed_ms + 1)),
        ("/model_called", json!(false)),
        ("/judgments/0/unit", json!("task_scope")),
        ("/judgments/0/unit_revision", json!("future-revision")),
        ("/judgments/0/question_id", json!(TASK_SCOPE)),
        ("/judgments/0/threshold_ppm", json!(500001)),
        (
            "/judgments/0/probability",
            serde_json::to_value(NoulProbability::new(1.0).unwrap()).unwrap(),
        ),
        ("/judgments/0/probability/raw", json!(0.25)),
        ("/judgments/0/probability/yes_ppm", json!(1000000)),
        ("/judgments/0/probability/no_ppm", json!(0)),
        ("/judgments/0/error", json!("authentication")),
        ("/judgments/0/model_used", json!(false)),
        ("/judgments/0/evidence_refs", json!([])),
        ("/judgments/1/evidence_refs", json!(["rebuild-1"])),
        ("/answers/0/value", json!("preserve")),
        ("/answers/0/reason", json!("preserved")),
        ("/answers/0/confidence_ppm", json!(999999)),
        ("/answers/0/evidence_refs", json!(["invented-proof"])),
        ("/answers", json!([])),
        ("/judgments", json!([])),
        ("/outcome/control/candidate", json!("block")),
        ("/outcome/obligations", json!([])),
        ("/outcome/evidence_refs", json!([])),
    ];
    for (path, value) in mutations {
        let mut forged = wire.clone();
        *forged.pointer_mut(path).unwrap() = value;
        assert!(
            decode_response(
                &serde_json::to_vec(&forged).unwrap(),
                &request,
                &config,
                &record
            )
            .is_err(),
            "accepted mutation at {path}"
        );
        if let Ok(typed) = serde_json::from_value(forged) {
            assert!(
                validate_response(&typed, &request, &config, &record).is_err(),
                "accepted typed mutation at {path}"
            );
        }
    }
    run.response.judgments[0].probability = Some(NoulProbability::new(1.0).unwrap());
    run.response.answers[0].value = AnswerValue::Preserve;
    assert!(validate_response(&run.response, &request, &config, run.record()).is_err());
    validate_response(&original, &request, &config, run.record()).unwrap();
    assert_eq!(
        decode_response(
            &serde_json::to_vec(&original).unwrap(),
            &request,
            &config,
            &record
        )
        .unwrap(),
        original
    );
    assert_eq!(
        observations.calls().len(),
        1,
        "verification must not invoke the model"
    );
}

#[tokio::test]
async fn original_input_configuration_and_actual_observation_cannot_be_substituted() {
    let request = request();
    let config = config();
    let (run, observations) = evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
    let wire = serde_json::to_value(&request).unwrap();
    for (path, value) in [
        (
            "/task/text",
            json!("A different authoritative task with identical resource lists."),
        ),
        ("/task/origin", json!("native_user")),
        ("/candidate/operation", json!("delete")),
        ("/budget/deadline_ms", json!(request.budget.deadline_ms + 1)),
        ("/evidence/0/source_revision", json!("source-v2")),
        (
            "/evidence/0/valid_until_ms",
            json!(request.evidence[0].valid_until_ms + 1),
        ),
        ("/evidence/0/provenance", json!("source_reported")),
        (
            "/gaps",
            json!([{"source_id": "new-gap", "scenario": "task_scope"}]),
        ),
    ] {
        let mut changed = wire.clone();
        *changed.pointer_mut(path).unwrap() = value;
        let changed: EvaluationRequest = serde_json::from_value(changed).unwrap();
        assert_ne!(
            input_digest(&request).unwrap(),
            input_digest(&changed).unwrap(),
            "{path}"
        );
        assert!(
            validate_response(&run.response, &changed, &config, run.record()).is_err(),
            "{path}"
        );
    }
    let config_wire = serde_json::to_value(&config).unwrap();
    for (path, value) in [
        ("/config_revision", json!("policy-v2")),
        ("/backend/model", json!("changed-model")),
        (
            "/backend/endpoint_url",
            json!("https://model.invalid/other-path"),
        ),
        (
            "/backend/key_env",
            json!("SEMANTIC_ARBITER_CONTRACT_UNUSED_OTHER_13A9"),
        ),
        ("/timeout_ms", json!(9999)),
        ("/max_response_bytes", json!(8191)),
        ("/thresholds/preservation_ppm", json!(900001)),
        ("/thresholds/task_scope_ppm", json!(900001)),
    ] {
        let mut changed = config_wire.clone();
        *changed.pointer_mut(path).unwrap() = value;
        let changed: ModelConfig = serde_json::from_value(changed).unwrap();
        assert!(
            validate_response(&run.response, &request, &changed, run.record()).is_err(),
            "{path}"
        );
    }
    let (other_run, _) = evaluate(&request, &[(PRESERVATION, 1.0), (TASK_SCOPE, 1.0)]).await;
    assert!(validate_response(&run.response, &request, &config, other_run.record()).is_err());
    assert_eq!(observations.calls().len(), 1);
}

#[tokio::test]
async fn request_policy_binding_mismatch_is_rejected_before_inference() {
    let request = request();
    let mut config = config();
    config.config_revision = "policy-v2".into();
    let (backend, observations) =
        ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
    assert!(Evaluator::new(config, backend)
        .unwrap()
        .evaluate(&request)
        .await
        .is_err());
    assert!(observations.calls().is_empty());
}

#[tokio::test]
async fn evidence_expiring_during_inference_is_rechecked_before_allowance() {
    let mut request = request();
    let ingress = Ingress::capture().unwrap();
    request.evidence[0].valid_until_ms = ingress.now_ms() + 1000;
    let (mut backend, observations) =
        ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
    backend.behavior = Behavior::SleepUntil(ingress.preparation_deadline(1100));
    let config = config();
    let run = Evaluator::new(config.clone(), backend)
        .unwrap()
        .evaluate_from_ingress(&request, ingress)
        .await
        .unwrap();
    assert_eq!(observations.calls().len(), 1);
    assert_answers(
        &run,
        &[AnswerValue::Unknown, AnswerValue::Allow],
        CandidateControl::Block,
    );
    assert_eq!(run.response.answers[0].reason, Reason::StaleEvidence);
    assert!(run.response.evaluated_at_ms >= request.evidence[0].valid_until_ms);
    assert!(run.response.judgments[0].probability.is_some());
    assert!(!run.response.judgments[0].model_used);
    assert!(run.response.judgments[1].model_used);
    validate_response(&run.response, &request, &config, run.record()).unwrap();
}

#[tokio::test]
async fn event_deadline_already_spent_before_ingress_cannot_be_renewed() {
    let mut request = request();
    request.budget.deadline_ms = request.budget.opened_at_ms + 1;
    let (run, observations) = evaluate(&request, &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)]).await;
    assert_answers(
        &run,
        &[AnswerValue::Unknown, AnswerValue::Unknown],
        CandidateControl::Block,
    );
    assert!(run
        .response
        .answers
        .iter()
        .all(|answer| answer.reason == Reason::BudgetExceeded));
    assert!(!run.response.model_called);
    assert!(observations.calls().is_empty());
}

#[tokio::test]
async fn preparation_consumes_config_cap_instead_of_starting_a_fresh_timeout() {
    let request = request();
    let mut config = config();
    config.timeout_ms = 40;
    let ingress = Ingress::capture().unwrap();
    tokio::time::sleep_until(ingress.preparation_deadline(70)).await;
    let (backend, observations) =
        ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
    let run = Evaluator::new(config.clone(), backend)
        .unwrap()
        .evaluate_from_ingress(&request, ingress)
        .await
        .unwrap();
    assert!(observations.calls().is_empty());
    assert!(!run.response.model_called);
    assert!(run.response.elapsed_ms >= 40);
    assert!(run
        .response
        .answers
        .iter()
        .all(|answer| answer.reason == Reason::BudgetExceeded));
    assert_eq!(
        run.response.outcome.control.candidate,
        CandidateControl::Nop
    );
    validate_response(&run.response, &request, &config, run.record()).unwrap();
}

#[tokio::test]
async fn backend_receives_the_original_minimum_deadline_including_preparation_time() {
    for config_is_shorter in [false, true] {
        let mut request = request();
        let mut config = config();
        let ingress = Ingress::capture().unwrap();
        let elapsed_before = ingress.elapsed_ms();
        request.budget.deadline_ms = ingress.now_ms() + 5000;
        let elapsed_after = ingress.elapsed_ms();
        config.timeout_ms = if config_is_shorter { 3000 } else { 8000 };
        tokio::time::sleep_until(ingress.preparation_deadline(30)).await;
        let (backend, observations) =
            ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
        let run = Evaluator::new(config.clone(), backend)
            .unwrap()
            .evaluate_from_ingress(&request, ingress)
            .await
            .unwrap();
        assert_eq!(observations.calls().len(), 1);
        let observed_deadline = observations.calls()[0].1;
        if config_is_shorter {
            assert_eq!(observed_deadline, ingress.preparation_deadline(3000));
        } else {
            assert!(observed_deadline >= ingress.preparation_deadline(5000 + elapsed_before));
            assert!(observed_deadline <= ingress.preparation_deadline(5000 + elapsed_after));
        }
        assert!(run.response.elapsed_ms >= 30);
        assert_eq!(
            run.response.outcome.control.candidate,
            CandidateControl::Allow
        );
        validate_response(&run.response, &request, &config, run.record()).unwrap();
    }
}

#[tokio::test]
async fn pending_inference_times_out_once_and_never_authorizes() {
    let request = request();
    let mut config = config();
    config.timeout_ms = 80;
    let (mut backend, observations) =
        ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
    backend.behavior = Behavior::Pending;
    let run = Evaluator::new(config.clone(), backend)
        .unwrap()
        .evaluate(&request)
        .await
        .unwrap();
    assert_eq!(observations.calls().len(), 1);
    assert!(!observations.completed.load(Ordering::SeqCst));
    assert_answers(
        &run,
        &[AnswerValue::Unknown, AnswerValue::Unknown],
        CandidateControl::Nop,
    );
    assert!(run
        .response
        .answers
        .iter()
        .all(|answer| answer.reason == Reason::BudgetExceeded));
    assert!(run
        .response
        .judgments
        .iter()
        .all(|trace| trace.error == Some(ModelError::Timeout) && !trace.model_used));
    validate_response(&run.response, &request, &config, run.record()).unwrap();
}

#[tokio::test]
async fn favorable_non_yielding_answer_returned_after_deadline_is_discarded() {
    let request = request();
    let mut config = config();
    config.timeout_ms = 80;
    let (mut backend, observations) =
        ProbeBackend::new(Ok(reply(&[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0)])));
    backend.behavior = Behavior::ReturnAfterDeadline;
    let run = Evaluator::new(config.clone(), backend)
        .unwrap()
        .evaluate(&request)
        .await
        .unwrap();
    assert_eq!(observations.calls().len(), 1);
    assert!(observations.completed.load(Ordering::SeqCst));
    assert_answers(
        &run,
        &[AnswerValue::Unknown, AnswerValue::Unknown],
        CandidateControl::Nop,
    );
    assert!(run.response.elapsed_ms >= 80);
    assert!(run
        .response
        .answers
        .iter()
        .all(|answer| answer.reason == Reason::BudgetExceeded));
    assert!(run.response.judgments.iter().all(|trace| !trace.model_used));
    validate_response(&run.response, &request, &config, run.record()).unwrap();
}

#[test]
fn model_evaluation_cannot_accept_recorded_clock_or_check_protocol_fields() {
    let original = request();
    let value = serde_json::to_value(&original).unwrap();
    assert!(protocol::decode_request(&serde_json::to_vec(&value).unwrap()).is_err());
    for (key, entry) in [
        ("check_at_ms", json!(1500)),
        ("replay_at_ms", json!(1500)),
        ("api_version", json!("semantic-arbiter/v1alpha1")),
        ("mode", json!("deterministic_check")),
        ("backend", json!("fixture")),
    ] {
        let mut invalid = value.clone();
        invalid[key] = entry;
        assert!(
            EvaluationRequest::decode(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "{key}"
        );
    }
    let check_request =
        protocol::decode_request(include_bytes!("../examples/preservation-allow.json")).unwrap();
    let mut check_response = semantic_arbiter::check::check(&check_request).unwrap();
    assert!(protocol::decode_response(
        &serde_json::to_vec(&check_response).unwrap(),
        &check_request
    )
    .is_ok());
    check_response.answers[0].confidence_ppm -= 1;
    assert!(protocol::decode_response(
        &serde_json::to_vec(&check_response).unwrap(),
        &check_request
    )
    .is_err());
}

#[test]
fn duplicate_or_unregistered_scenarios_are_rejected_at_ingress() {
    let mut value = serde_json::to_value(request()).unwrap();
    for scenarios in [
        json!([]),
        json!(["preservation", "preservation"]),
        json!(["unregistered_scenario"]),
    ] {
        value["scenarios"] = scenarios;
        assert!(EvaluationRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let request = request();
    let mut invalid = serde_json::to_value(request).unwrap();
    invalid["task"]["session_id"] = json!("different-session");
    assert!(EvaluationRequest::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
}
