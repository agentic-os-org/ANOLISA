//! Deterministic contract tests using the parent injectable ProbeBackend, not real model validation.

use super::*;
use semantic_arbiter::contract::{candidate_revision, NativeTool};
use semantic_arbiter::local_policy::{
    ActionKind, ActionSignal, ContentDelta, FileObservation, FileState, LocalContext,
};
use semantic_arbiter::protocol::ContractError;

fn snapshot_request() -> EvaluationRequest {
    let mut request = request();
    request.scenarios = vec![Scenario::Snapshot];
    request.evidence.clear();
    request.local_context = Some(LocalContext {
        candidate_revision: request.binding.candidate_revision.clone(),
        captured_at_ms: request.budget.opened_at_ms,
        valid_until_ms: request.budget.deadline_ms,
        files: vec![FileObservation {
            resource_id: request.candidate.changes[0].resource_id.clone(),
            state: FileState::Regular,
            delta: ContentDelta::Changed,
            existed_at_start: Some(true),
            sha256: Some(format!("sha256:{}", "a".repeat(64))),
        }],
        signals: vec![],
        gaps: vec![],
    });
    request
}

fn has_snapshot(run: &EvaluationRun) -> bool {
    run.response.outcome.obligations.iter().any(|obligation| {
        obligation.critical
            && matches!(&obligation.action, ObligationAction::Snapshot { workspace_id }
                if workspace_id == &run.response.binding.workspace_id)
    })
}

#[tokio::test]
async fn preexisting_hashed_regular_bytes_get_local_snapshot_even_without_rebuild_evidence() {
    for delta in [ContentDelta::Changed, ContentDelta::Unknown] {
        let mut request = snapshot_request();
        request.local_context.as_mut().unwrap().files[0].delta = delta;
        // A deliberately contradictory fixture answer must never be requested.
        let (run, observations) = evaluate(&request, &[(SNAPSHOT, 0.0)]).await;
        assert_answers(&run, &[AnswerValue::NeedsSnapshot], CandidateControl::Allow);
        assert_eq!(run.response.answers[0].reason, Reason::NeedsSnapshot);
        assert_eq!(run.response.answers[0].confidence_ppm, 1_000_000);
        assert_trace(
            &run,
            0,
            DecisionRoute::LocalPolicy,
            Some("protect_preexisting_bytes"),
            Reason::MissingEvidence,
            false,
        );
        assert!(run.response.judgments[0].probability.is_none());
        assert!(run.response.judgments[0].error.is_none());
        assert!(run.response.answers[0].evidence_refs.is_empty());
        assert!(!run.response.model_called);
        assert!(observations.calls().is_empty());
        assert!(has_snapshot(&run));
    }
}

#[tokio::test]
async fn exact_absence_or_byte_identical_edit_needs_no_snapshot_or_model() {
    for absent in [false, true] {
        let mut request = snapshot_request();
        let file = &mut request.local_context.as_mut().unwrap().files[0];
        if absent {
            file.state = FileState::Absent;
            file.delta = ContentDelta::Unknown;
            file.sha256 = None;
        } else {
            file.delta = ContentDelta::Unchanged;
        }
        let (run, observations) = evaluate(&request, &[(SNAPSHOT, 1.0)]).await;
        assert_answers(&run, &[AnswerValue::Allow], CandidateControl::Allow);
        assert_eq!(run.response.answers[0].reason, Reason::Allowed);
        assert_eq!(run.response.answers[0].confidence_ppm, 1_000_000);
        assert_trace(
            &run,
            0,
            DecisionRoute::LocalPolicy,
            Some("no_current_bytes_lost"),
            Reason::MissingEvidence,
            false,
        );
        assert!(!has_snapshot(&run));
        assert!(!run.response.model_called);
        assert!(observations.calls().is_empty());
    }
}

#[tokio::test]
async fn byte_identical_operation_does_not_destroy_unique_bytes() {
    let mut input = snapshot_request();
    input.evidence = request().evidence;
    input.evidence[0].fact = EvidenceFact::Rebuild {
        resource_id: input.candidate.changes[0].resource_id.clone(),
        result: RebuildResult::Unique,
    };
    input.local_context.as_mut().unwrap().files[0].delta = ContentDelta::Unchanged;
    let (run, observations) = evaluate(&input, &[]).await;
    assert_answers(&run, &[AnswerValue::Allow], CandidateControl::Allow);
    assert_trace(
        &run,
        0,
        DecisionRoute::LocalPolicy,
        Some("no_current_bytes_lost"),
        Reason::Allowed,
        false,
    );
    assert!(!has_snapshot(&run));
    assert!(observations.calls().is_empty());
}

#[tokio::test]
async fn incomplete_new_or_unknown_observations_delegate_instead_of_inventing_protection() {
    for case in [
        "no_context",
        "no_files",
        "partial",
        "new",
        "inventory_unknown",
        "unhashed",
        "unknown",
        "directory",
        "symlink",
        "opaque",
    ] {
        for (p, value, control) in [
            (0.0, AnswerValue::Allow, CandidateControl::Allow),
            (1.0, AnswerValue::NeedsSnapshot, CandidateControl::Allow),
            (0.5, AnswerValue::Unknown, CandidateControl::Nop),
        ] {
            let mut request = snapshot_request();
            let context = request.local_context.as_mut().unwrap();
            match case {
                "no_context" => request.local_context = None,
                "no_files" => context.files.clear(),
                "partial" => {
                    let mut change = request.candidate.changes[0].clone();
                    change.resource_id = "second-resource".into();
                    request.candidate.changes.push(change);
                    request.task.as_mut().unwrap().allowed_resource_ids.clear();
                    context.files[0].delta = ContentDelta::Unchanged;
                }
                "new" => context.files[0].existed_at_start = Some(false),
                "inventory_unknown" => context.files[0].existed_at_start = None,
                "unhashed" => {
                    context.files[0].sha256 = None;
                    context.files[0].delta = ContentDelta::Unknown;
                }
                "opaque" => {
                    context.files[0].delta = ContentDelta::Unchanged;
                    context.signals.push(ActionSignal {
                        kind: ActionKind::OpaqueExecution,
                        resource_id: None,
                    });
                }
                _ => {
                    context.files[0].state = match case {
                        "directory" => FileState::Directory,
                        "symlink" => FileState::Symlink,
                        _ => FileState::Unknown,
                    };
                    context.files[0].delta = ContentDelta::Unknown;
                    context.files[0].sha256 = None;
                }
            }
            if let Some(context) = &mut request.local_context {
                context.gaps.push("fixture-coverage-incomplete".into());
            }
            let (run, observations) = evaluate(&request, &[(SNAPSHOT, p)]).await;
            assert_answers(&run, &[value], control);
            assert_trace(
                &run,
                0,
                DecisionRoute::ModelFallback,
                None,
                Reason::MissingEvidence,
                true,
            );
            assert_eq!(
                has_snapshot(&run),
                value == AnswerValue::NeedsSnapshot,
                "{case}"
            );
            assert!(run.response.model_called, "{case}");
            let calls = observations.calls();
            assert_eq!(calls.len(), 1, "{case}");
            assert_eq!(
                calls[0]
                    .0
                    .questions
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                [SNAPSHOT]
            );
            assert_eq!(
                calls[0].0.state["local_context"],
                json!(request.local_context)
            );
            assert_eq!(
                calls[0].0.state["qualification"][0]["reason"],
                "missing_evidence"
            );
        }
    }
}

#[tokio::test]
async fn missing_gap_and_unknown_evidence_fallback_retains_original_qualification() {
    for scenario in [Scenario::Preservation, Scenario::Snapshot] {
        for case in ["missing", "gap", "unknown"] {
            for p in [0.0, 0.5, 1.0] {
                let mut request = request();
                let baseline = inhibit_preservation(&mut request, case);
                request.scenarios = vec![scenario];
                for gap in &mut request.gaps {
                    gap.scenario = scenario;
                }
                let id = if scenario == Scenario::Snapshot {
                    SNAPSHOT
                } else {
                    PRESERVATION
                };
                let (run, observations) = evaluate(&request, &[(id, p)]).await;
                let expected = match (scenario, p) {
                    (_, 0.0) => AnswerValue::Allow,
                    (_, 0.5) => AnswerValue::Unknown,
                    (Scenario::Snapshot, _) => AnswerValue::NeedsSnapshot,
                    _ => AnswerValue::Preserve,
                };
                let control = match expected {
                    AnswerValue::Preserve => CandidateControl::Block,
                    AnswerValue::Unknown => CandidateControl::Nop,
                    _ => CandidateControl::Allow,
                };
                assert_answers(&run, &[expected], control);
                assert_trace(&run, 0, DecisionRoute::ModelFallback, None, baseline, true);
                let calls = observations.calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(
                    calls[0]
                        .0
                        .questions
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    [id]
                );
                assert_eq!(calls[0].0.state["evidence_gaps"], json!(request.gaps));
                assert_eq!(
                    calls[0].0.state["qualification"][0]["reason"],
                    json!(baseline)
                );
                assert_eq!(calls[0].0.state["qualification"][0]["value"], "unknown");
                let selected = request.evidence.clone();
                assert_eq!(calls[0].0.state["evidence"], json!(selected));
                assert_eq!(
                    run.response.answers[0].evidence_refs,
                    selected
                        .iter()
                        .map(|item| item.id.clone())
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

#[tokio::test]
async fn task_scope_is_semantic_even_when_local_signals_and_snapshot_are_decisive() {
    for (p, scope, control) in [
        (1.0, AnswerValue::Allow, CandidateControl::Allow),
        (0.0, AnswerValue::OutsideScope, CandidateControl::Block),
    ] {
        let mut request = snapshot_request();
        request.scenarios = vec![Scenario::Snapshot, Scenario::TaskScope];
        request.task.as_mut().unwrap().text =
            "Update the test and its dependencies as requested.".into();
        let context = request.local_context.as_mut().unwrap();
        context.signals = [
            ActionKind::TestChange,
            ActionKind::DependencyChange,
            ActionKind::CheckBypass,
            ActionKind::GitPublish,
        ]
        .into_iter()
        .map(|kind| ActionSignal {
            kind,
            resource_id: Some(context.files[0].resource_id.clone()),
        })
        .collect();
        let (run, observations) = evaluate(&request, &[(TASK_SCOPE, p)]).await;
        assert_answers(&run, &[scope, AnswerValue::NeedsSnapshot], control);
        assert_trace(
            &run,
            0,
            DecisionRoute::ModelFallback,
            None,
            Reason::Allowed,
            true,
        );
        assert_trace(
            &run,
            1,
            DecisionRoute::LocalPolicy,
            Some("protect_preexisting_bytes"),
            Reason::MissingEvidence,
            false,
        );
        let calls = observations.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]
                .0
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [TASK_SCOPE]
        );
        assert_eq!(
            calls[0].0.state["local_context"],
            json!(request.local_context)
        );
        assert_eq!(calls[0].0.state["authoritative_task"], json!(request.task));
        assert_eq!(has_snapshot(&run), control == CandidateControl::Allow);
    }
}

#[tokio::test]
async fn malformed_observation_binding_hash_and_resources_are_rejected_before_inference() {
    let original = snapshot_request();
    let wire = serde_json::to_value(&original).unwrap();
    for (path, value, error) in [
        (
            "/local_context/candidate_revision",
            json!("other-revision"),
            ContractError::BindingMismatch,
        ),
        (
            "/local_context/files/0/resource_id",
            json!("unrelated-resource"),
            ContractError::InvalidInput,
        ),
        (
            "/local_context/files/0/sha256",
            json!("sha256:bad"),
            ContractError::InvalidInput,
        ),
        (
            "/local_context/files/0/sha256",
            json!(format!("sha256:{}", "g".repeat(64))),
            ContractError::InvalidInput,
        ),
        (
            "/local_context/files/0/sha256",
            Value::Null,
            ContractError::InvalidInput,
        ),
        (
            "/local_context/files/0/state",
            json!("absent"),
            ContractError::InvalidInput,
        ),
        (
            "/local_context/files",
            json!([
                wire["local_context"]["files"][0],
                wire["local_context"]["files"][0]
            ]),
            ContractError::InvalidInput,
        ),
        (
            "/local_context/signals",
            json!([{"kind": "delete", "resource_id": "unrelated-resource"}]),
            ContractError::InvalidInput,
        ),
        (
            "/local_context/valid_until_ms",
            json!(original.budget.opened_at_ms),
            ContractError::InvalidInput,
        ),
    ] {
        let mut invalid = wire.clone();
        *invalid.pointer_mut(path).unwrap() = value;
        assert_eq!(
            EvaluationRequest::decode(&serde_json::to_vec(&invalid).unwrap()).unwrap_err(),
            error,
            "{path}"
        );
        let typed = serde_json::from_value(invalid).unwrap();
        let (backend, observations) = ProbeBackend::new(Ok(reply(&[(SNAPSHOT, 0.0)])));
        assert_eq!(
            Evaluator::new(config(), backend)
                .unwrap()
                .evaluate(&typed)
                .await
                .unwrap_err(),
            error,
            "{path}"
        );
        assert!(observations.calls().is_empty());
    }
}

#[tokio::test]
async fn native_candidate_content_changes_cannot_reuse_local_observation_binding() {
    let mut request = snapshot_request();
    request.candidate.native_tool = Some(NativeTool {
        name: "write_file".into(),
        input: json!({"path": "result.txt", "content": "new"}),
    });
    request.binding.candidate_revision = candidate_revision(&request.candidate).unwrap();
    request.local_context.as_mut().unwrap().candidate_revision =
        request.binding.candidate_revision.clone();
    request.candidate.native_tool.as_mut().unwrap().input["content"] = json!("substituted");
    let (backend, observations) = ProbeBackend::new(Ok(reply(&[(SNAPSHOT, 0.0)])));
    assert_eq!(
        Evaluator::new(config(), backend)
            .unwrap()
            .evaluate(&request)
            .await
            .unwrap_err(),
        ContractError::BindingMismatch
    );
    assert!(observations.calls().is_empty());
}

#[tokio::test]
async fn local_hints_never_override_stale_untrusted_or_conflicting_evidence() {
    for case in ["stale", "future", "reported", "echo", "conflicting"] {
        for no_op in [false, true] {
            let mut input = snapshot_request();
            input.evidence = request().evidence;
            let reason = inhibit_preservation(&mut input, case);
            if no_op {
                input.local_context.as_mut().unwrap().files[0].delta = ContentDelta::Unchanged;
            }
            let (run, observations) = evaluate(&input, &[(SNAPSHOT, 0.0)]).await;
            assert_answers(&run, &[AnswerValue::Unknown], CandidateControl::Block);
            assert_eq!(run.response.answers[0].reason, reason);
            assert_trace(&run, 0, DecisionRoute::HardGate, None, reason, false);
            assert!(run.response.judgments[0].probability.is_none());
            assert!(!run.response.model_called);
            assert!(observations.calls().is_empty());
            assert!(!has_snapshot(&run));
        }
    }
}

#[tokio::test]
async fn evidence_integrity_failures_cannot_be_hidden_by_a_gap_or_missing_first_resource() {
    for case in ["stale", "reported", "conflicting"] {
        for gap in [false, true] {
            let mut request = request();
            request.scenarios = vec![Scenario::Snapshot];
            let reason = inhibit_preservation(&mut request, case);
            if gap {
                request.gaps.push(EvidenceGap {
                    source_id: "incomplete-collector".into(),
                    scenario: Scenario::Snapshot,
                });
            } else {
                let mut missing = request.candidate.changes[0].clone();
                missing.resource_id = "missing-first-resource".into();
                request.candidate.changes.insert(0, missing);
                request.task.as_mut().unwrap().allowed_resource_ids.clear();
            }
            let (run, observations) = evaluate(&request, &[(SNAPSHOT, 0.0)]).await;
            assert_eq!(run.response.answers[0].reason, reason, "{case}/gap={gap}");
            assert_answers(&run, &[AnswerValue::Unknown], CandidateControl::Block);
            assert_trace(&run, 0, DecisionRoute::HardGate, None, reason, false);
            assert!(observations.calls().is_empty());
        }
    }
}

#[tokio::test]
async fn already_expired_local_observations_are_omitted_from_fallback() {
    let mut request = snapshot_request();
    request.local_context.as_mut().unwrap().valid_until_ms = request.budget.opened_at_ms + 1;
    let (run, observations) = evaluate(&request, &[(SNAPSHOT, 0.5)]).await;
    assert_answers(&run, &[AnswerValue::Unknown], CandidateControl::Nop);
    assert_trace(
        &run,
        0,
        DecisionRoute::ModelFallback,
        None,
        Reason::MissingEvidence,
        true,
    );
    assert_eq!(observations.calls().len(), 1);
    assert_eq!(
        observations.calls()[0].0.state["local_context"],
        Value::Null
    );
    assert!(!has_snapshot(&run));
}

#[tokio::test]
async fn local_expiry_during_other_unit_inference_cannot_reuse_a_stale_local_answer() {
    for no_op in [false, true] {
        let mut request = snapshot_request();
        request.scenarios.push(Scenario::TaskScope);
        let ingress = Ingress::capture().unwrap();
        let context = request.local_context.as_mut().unwrap();
        context.valid_until_ms = ingress.now_ms() + 1000;
        if no_op {
            context.files[0].delta = ContentDelta::Unchanged;
        }
        let expires = context.valid_until_ms;
        let (mut backend, observations) = ProbeBackend::new(Ok(reply(&[(TASK_SCOPE, 1.0)])));
        backend.behavior = Behavior::SleepUntil(ingress.preparation_deadline(1100));
        let run = Evaluator::new(config(), backend)
            .unwrap()
            .evaluate_from_ingress(&request, ingress)
            .await
            .unwrap();
        assert!(run.response.evaluated_at_ms >= expires);
        assert_answers(
            &run,
            &[AnswerValue::Unknown, AnswerValue::Unknown],
            CandidateControl::Nop,
        );
        assert_trace(
            &run,
            0,
            DecisionRoute::ObservationExpired,
            None,
            Reason::Allowed,
            false,
        );
        assert_trace(
            &run,
            1,
            DecisionRoute::ObservationExpired,
            None,
            Reason::MissingEvidence,
            false,
        );
        assert_eq!(
            run.response.judgments[0]
                .probability
                .as_ref()
                .unwrap()
                .raw(),
            1.0
        );
        assert!(run.response.judgments[1].probability.is_none());
        assert!(run.response.judgments[1].error.is_none());
        assert_eq!(run.response.answers[1].reason, Reason::Undetermined);
        assert!(!has_snapshot(&run));
        let calls = observations.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]
                .0
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [TASK_SCOPE]
        );
        assert_eq!(
            calls[0].0.state["local_context"],
            json!(request.local_context)
        );
        validate_response(&run.response, &request, &config(), run.record()).unwrap();
    }
}

/// A fully attributed read-only call: no changes, one completeness signal.
fn read_only_request(scenarios: Vec<Scenario>) -> EvaluationRequest {
    let mut request = request();
    request.scenarios = scenarios;
    request.evidence.clear();
    request.candidate.changes.clear();
    request.candidate.native_tool = Some(NativeTool {
        name: "Bash".into(),
        input: json!({"command": "git status --short"}),
    });
    request.binding.candidate_revision = candidate_revision(&request.candidate).unwrap();
    request.local_context = Some(LocalContext {
        candidate_revision: request.binding.candidate_revision.clone(),
        captured_at_ms: request.budget.opened_at_ms,
        valid_until_ms: request.budget.deadline_ms,
        files: vec![],
        signals: vec![ActionSignal {
            kind: ActionKind::ReadOnly,
            resource_id: None,
        }],
        gaps: vec![],
    });
    request
}

#[tokio::test]
async fn read_only_calls_settle_protection_locally_but_scope_stays_semantic() {
    // A complete non-sensitive read projection settles the effect questions
    // (no mutation, no loss) but never the goal question: an unrelated read is
    // out of scope even without a write effect, so task scope asks the model.
    let request = read_only_request(vec![
        Scenario::Preservation,
        Scenario::TaskScope,
        Scenario::Snapshot,
    ]);
    let (run, observations) = evaluate(&request, &[(TASK_SCOPE, 1.0)]).await;
    assert_answers(
        &run,
        &[AnswerValue::Allow, AnswerValue::Allow, AnswerValue::Allow],
        CandidateControl::Allow,
    );
    assert_trace(
        &run,
        0,
        DecisionRoute::LocalPolicy,
        Some("preserve_no_mutation"),
        Reason::Allowed,
        false,
    );
    assert_trace(
        &run,
        1,
        DecisionRoute::ModelFallback,
        None,
        Reason::Allowed,
        true,
    );
    assert_trace(
        &run,
        2,
        DecisionRoute::LocalPolicy,
        Some("no_loss_read_only"),
        Reason::Allowed,
        false,
    );
    assert_eq!(run.response.answers[0].confidence_ppm, 1_000_000);
    assert_eq!(run.response.answers[2].confidence_ppm, 1_000_000);
    assert!(run.response.model_called);
    let calls = observations.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0]
            .0
            .questions
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [TASK_SCOPE]
    );
    assert!(!has_snapshot(&run));
}

#[tokio::test]
async fn sensitive_reads_keep_task_scope_semantic_while_protection_stays_local() {
    for (p, scope, control) in [
        (1.0, AnswerValue::Allow, CandidateControl::Allow),
        (0.0, AnswerValue::OutsideScope, CandidateControl::Block),
    ] {
        let mut request = read_only_request(vec![
            Scenario::Preservation,
            Scenario::TaskScope,
            Scenario::Snapshot,
        ]);
        request
            .local_context
            .as_mut()
            .unwrap()
            .signals
            .push(ActionSignal {
                kind: ActionKind::SensitiveRead,
                resource_id: None,
            });
        let (run, observations) = evaluate(&request, &[(TASK_SCOPE, p)]).await;
        assert_answers(
            &run,
            &[AnswerValue::Allow, scope, AnswerValue::Allow],
            control,
        );
        assert_trace(
            &run,
            0,
            DecisionRoute::LocalPolicy,
            Some("preserve_no_mutation"),
            Reason::Allowed,
            false,
        );
        assert_trace(
            &run,
            1,
            DecisionRoute::ModelFallback,
            None,
            Reason::Allowed,
            true,
        );
        assert_trace(
            &run,
            2,
            DecisionRoute::LocalPolicy,
            Some("no_loss_read_only"),
            Reason::Allowed,
            false,
        );
        let calls = observations.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]
                .0
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [TASK_SCOPE]
        );
    }
}

#[tokio::test]
async fn an_opaque_remainder_defeats_every_read_only_policy() {
    let mut request = read_only_request(vec![
        Scenario::Preservation,
        Scenario::TaskScope,
        Scenario::Snapshot,
    ]);
    request
        .local_context
        .as_mut()
        .unwrap()
        .signals
        .push(ActionSignal {
            kind: ActionKind::OpaqueExecution,
            resource_id: None,
        });
    let (run, observations) = evaluate(
        &request,
        &[(PRESERVATION, 0.0), (TASK_SCOPE, 1.0), (SNAPSHOT, 0.0)],
    )
    .await;
    assert_answers(
        &run,
        &[AnswerValue::Allow, AnswerValue::Allow, AnswerValue::Allow],
        CandidateControl::Allow,
    );
    for index in 0..3 {
        assert_trace(
            &run,
            index,
            DecisionRoute::ModelFallback,
            None,
            Reason::Allowed,
            true,
        );
    }
    assert!(run.response.model_called);
    assert_eq!(observations.calls().len(), 1);
}

#[tokio::test]
async fn read_only_signals_without_an_empty_candidate_stay_semantic() {
    let mut request = snapshot_request();
    request.scenarios = vec![Scenario::TaskScope];
    request.local_context.as_mut().unwrap().signals = vec![ActionSignal {
        kind: ActionKind::ReadOnly,
        resource_id: None,
    }];
    let (run, observations) = evaluate(&request, &[(TASK_SCOPE, 1.0)]).await;
    assert_answers(&run, &[AnswerValue::Allow], CandidateControl::Allow);
    assert_trace(
        &run,
        0,
        DecisionRoute::ModelFallback,
        None,
        Reason::Allowed,
        true,
    );
    assert_eq!(observations.calls().len(), 1);
}

#[tokio::test]
async fn creating_absent_resources_preserves_existing_bytes_without_a_model() {
    let mut request = snapshot_request();
    request.scenarios = vec![Scenario::Preservation, Scenario::Snapshot];
    let file = &mut request.local_context.as_mut().unwrap().files[0];
    file.state = FileState::Absent;
    file.delta = ContentDelta::Unknown;
    file.sha256 = None;
    file.existed_at_start = None;
    let (run, observations) = evaluate(&request, &[]).await;
    assert_answers(
        &run,
        &[AnswerValue::Allow, AnswerValue::Allow],
        CandidateControl::Allow,
    );
    assert_trace(
        &run,
        0,
        DecisionRoute::LocalPolicy,
        Some("preserve_absent_target"),
        Reason::MissingEvidence,
        false,
    );
    assert_trace(
        &run,
        1,
        DecisionRoute::LocalPolicy,
        Some("no_current_bytes_lost"),
        Reason::MissingEvidence,
        false,
    );
    assert!(!run.response.model_called);
    assert!(observations.calls().is_empty());
    assert!(!has_snapshot(&run));
}

#[tokio::test]
async fn working_state_destruction_floors_snapshot_when_reach_is_unattributed() {
    let mut request = snapshot_request();
    let context = request.local_context.as_mut().unwrap();
    context.files.clear();
    context.gaps.push("observation_targets_unknown".into());
    context.signals = vec![ActionSignal {
        kind: ActionKind::WorkingStateDestruction,
        resource_id: None,
    }];
    let (run, observations) = evaluate(&request, &[]).await;
    assert_answers(&run, &[AnswerValue::NeedsSnapshot], CandidateControl::Allow);
    assert_trace(
        &run,
        0,
        DecisionRoute::LocalPolicy,
        Some("protect_working_state"),
        Reason::MissingEvidence,
        false,
    );
    assert!(has_snapshot(&run));
    assert!(!run.response.model_called);
    assert!(observations.calls().is_empty());
}

#[tokio::test]
async fn working_state_destruction_over_identical_targets_loses_nothing_locally() {
    let mut input = snapshot_request();
    input.evidence = request().evidence;
    let context = input.local_context.as_mut().unwrap();
    context.files[0].delta = ContentDelta::Unknown;
    context.signals = vec![ActionSignal {
        kind: ActionKind::WorkingStateDestruction,
        resource_id: Some(context.files[0].resource_id.clone()),
    }];
    let (run, observations) = evaluate(&input, &[(SNAPSHOT, 0.0)]).await;
    assert_answers(&run, &[AnswerValue::Allow], CandidateControl::Allow);
    assert_trace(
        &run,
        0,
        DecisionRoute::ModelFallback,
        None,
        Reason::Allowed,
        true,
    );
    assert!(!has_snapshot(&run));
    assert_eq!(observations.calls().len(), 1);
}

#[tokio::test]
async fn expired_observations_never_settle_read_only_or_destruction_policies() {
    for destruction in [false, true] {
        let mut request = if destruction {
            let mut request = snapshot_request();
            let context = request.local_context.as_mut().unwrap();
            context.files.clear();
            context.signals = vec![ActionSignal {
                kind: ActionKind::WorkingStateDestruction,
                resource_id: None,
            }];
            request
        } else {
            read_only_request(vec![Scenario::TaskScope])
        };
        request.local_context.as_mut().unwrap().valid_until_ms = request.budget.opened_at_ms + 1;
        let id = if destruction { SNAPSHOT } else { TASK_SCOPE };
        let baseline = if destruction {
            Reason::MissingEvidence
        } else {
            Reason::Allowed
        };
        let (run, observations) = evaluate(&request, &[(id, 0.5)]).await;
        assert_answers(&run, &[AnswerValue::Unknown], CandidateControl::Nop);
        assert_trace(&run, 0, DecisionRoute::ModelFallback, None, baseline, true);
        assert_eq!(observations.calls().len(), 1);
        assert_eq!(
            observations.calls()[0].0.state["local_context"],
            Value::Null
        );
    }
}

#[tokio::test]
async fn valid_local_observations_and_route_metadata_are_bound_to_the_retained_record() {
    let request = snapshot_request();
    let (run, _) = evaluate(&request, &[]).await;
    let wire = serde_json::to_value(&request).unwrap();
    for (path, value) in [
        (
            "/local_context/files/0/sha256",
            json!(format!("sha256:{}", "b".repeat(64))),
        ),
        ("/local_context/files/0/existed_at_start", json!(false)),
        ("/local_context/files/0/delta", json!("unchanged")),
        (
            "/local_context/valid_until_ms",
            json!(request.budget.deadline_ms - 1),
        ),
        ("/local_context/gaps", json!(["new-gap"])),
    ] {
        let mut changed = wire.clone();
        *changed.pointer_mut(path).unwrap() = value;
        let changed = EvaluationRequest::decode(&serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_ne!(
            input_digest(&request).unwrap(),
            input_digest(&changed).unwrap()
        );
        assert_eq!(
            validate_response(&run.response, &changed, &config(), run.record()),
            Err(ContractError::BindingMismatch)
        );
    }
    let wire = serde_json::to_value(&run.response).unwrap();
    for (path, value) in [
        ("/judgments/0/route", json!("model_fallback")),
        ("/judgments/0/policy_id", json!("no_current_bytes_lost")),
        ("/judgments/0/baseline_reason", json!("allowed")),
    ] {
        let mut forged = wire.clone();
        *forged.pointer_mut(path).unwrap() = value;
        assert_eq!(
            decode_response(
                &serde_json::to_vec(&forged).unwrap(),
                &request,
                &config(),
                run.record()
            ),
            Err(ContractError::InvalidResponse)
        );
    }
}
