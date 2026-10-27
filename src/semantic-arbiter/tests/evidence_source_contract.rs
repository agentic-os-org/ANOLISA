//! Evidence source registration, collection and frozen-check boundary contracts.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use semantic_arbiter::check::check;
use semantic_arbiter::contract::{
    Binding, Budget, Candidate, EvidenceFact, Operation, Provenance, Reason, RebuildResult,
    Scenario, Task, TaskOrigin,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evidence::{
    EvidenceCollection, EvidenceCollectionError, EvidenceRequest, EvidenceSource,
    EvidenceSourceDescriptor, EvidenceSourceError, EvidenceSourceOutput, EvidenceSourceRegistry,
    EvidenceSourceStatus, SourceEvidence, MAX_SOURCE_AGE_MS, MAX_SOURCE_TIMEOUT_MS,
};
use semantic_arbiter::outcome::CandidateControl;
use semantic_arbiter::protocol::{
    CheckMode, CheckRequest, CHECK_API_VERSION, MAX_EVIDENCE_ITEMS, MAX_MESSAGE_BYTES,
};
use tokio::time::Instant;

#[derive(Clone, Debug)]
struct Observation {
    source_id: String,
    request_id: String,
    scenarios: Vec<Scenario>,
    task_text: Option<String>,
    candidate: Candidate,
    deadline: Instant,
}

#[derive(Clone)]
enum Behavior {
    Return(Result<EvidenceSourceOutput, EvidenceSourceError>),
    Delayed(Duration, EvidenceSourceOutput),
    Pending(Arc<AtomicBool>),
}

struct FixtureSource {
    descriptor: EvidenceSourceDescriptor,
    behavior: Behavior,
    observations: Arc<Mutex<Vec<Observation>>>,
}

impl FixtureSource {
    fn returning(
        descriptor: EvidenceSourceDescriptor,
        output: EvidenceSourceOutput,
        observations: Arc<Mutex<Vec<Observation>>>,
    ) -> Self {
        Self {
            descriptor,
            behavior: Behavior::Return(Ok(output)),
            observations,
        }
    }

    fn delayed(
        descriptor: EvidenceSourceDescriptor,
        delay: Duration,
        output: EvidenceSourceOutput,
        observations: Arc<Mutex<Vec<Observation>>>,
    ) -> Self {
        Self {
            descriptor,
            behavior: Behavior::Delayed(delay, output),
            observations,
        }
    }

    fn unavailable(
        descriptor: EvidenceSourceDescriptor,
        observations: Arc<Mutex<Vec<Observation>>>,
    ) -> Self {
        Self {
            descriptor,
            behavior: Behavior::Return(Err(EvidenceSourceError::Unavailable)),
            observations,
        }
    }
}

impl EvidenceSource for FixtureSource {
    fn descriptor(&self) -> EvidenceSourceDescriptor {
        self.descriptor.clone()
    }

    fn collect<'a>(
        &'a self,
        request: EvidenceRequest<'a>,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<EvidenceSourceOutput, EvidenceSourceError>> + Send + 'a>>
    {
        self.observations.lock().unwrap().push(Observation {
            source_id: self.descriptor.source_id.clone(),
            request_id: request.request_id.to_owned(),
            scenarios: request.scenarios.to_vec(),
            task_text: request.task.map(|task| task.text.clone()),
            candidate: request.candidate.clone(),
            deadline,
        });
        Box::pin(async move {
            match &self.behavior {
                Behavior::Return(result) => result.clone(),
                Behavior::Delayed(delay, output) => {
                    tokio::time::sleep(*delay).await;
                    Ok(output.clone())
                }
                Behavior::Pending(completed) => {
                    std::future::pending::<()>().await;
                    completed.store(true, Ordering::SeqCst);
                    unreachable!()
                }
            }
        })
    }
}

fn descriptor(source_id: &str, provenance: Provenance) -> EvidenceSourceDescriptor {
    EvidenceSourceDescriptor {
        source_id: source_id.into(),
        source_revision: "revision-1".into(),
        provenance,
        supported_scenarios: vec![Scenario::TaskScope, Scenario::Preservation],
        max_evidence_items: 8,
        max_evidence_bytes: MAX_MESSAGE_BYTES,
        max_age_ms: MAX_SOURCE_AGE_MS,
        timeout_ms: 1_000,
    }
}

fn source_evidence(id: &str, captured_at_ms: u64, valid_until_ms: u64) -> SourceEvidence {
    SourceEvidence {
        id: id.into(),
        captured_at_ms,
        valid_until_ms,
        fact: EvidenceFact::Rebuild {
            resource_id: "resource-1".into(),
            result: RebuildResult::Identical,
        },
    }
}

fn request(now_ms: u64) -> CheckRequest {
    CheckRequest {
        api_version: CHECK_API_VERSION.into(),
        request_id: "request-1".into(),
        mode: CheckMode::DeterministicCheck,
        binding: Binding {
            session_id: "session-1".into(),
            instance_id: "instance-1".into(),
            workspace_id: "workspace-1".into(),
            task_version: "task-1".into(),
            config_revision: "policy-1".into(),
            candidate_id: "candidate-1".into(),
            candidate_revision: "candidate-revision-1".into(),
        },
        budget: Budget {
            opened_at_ms: now_ms.saturating_sub(1_000),
            deadline_ms: now_ms + 5_000,
            max_evidence_items: MAX_EVIDENCE_ITEMS,
            max_evidence_bytes: MAX_MESSAGE_BYTES,
        },
        check_at_ms: now_ms,
        scenarios: vec![Scenario::TaskScope, Scenario::Preservation],
        task: Some(Task {
            session_id: "session-1".into(),
            version: "task-1".into(),
            origin: TaskOrigin::Operator,
            text: "Replace the generated resource".into(),
            allowed_resource_ids: vec!["resource-1".into()],
            preserve_resource_ids: vec![],
        }),
        candidate: Candidate {
            operation: Operation::Replace,
            resource_id: "resource-1".into(),
            native_tool: None,
        },
        evidence: vec![],
        gaps: vec![],
    }
}

fn source_evidence_bytes(collection: &EvidenceCollection, source_id: &str) -> usize {
    let evidence = collection
        .evidence
        .iter()
        .filter(|item| item.source_id == source_id)
        .cloned()
        .collect::<Vec<_>>();
    serde_json::to_vec(&evidence).unwrap().len()
}

async fn collect_one(
    ingress: Ingress,
    now_ms: u64,
    source_byte_limit: usize,
    request_byte_limit: usize,
) -> Result<EvidenceCollection, EvidenceCollectionError> {
    let mut request = request(now_ms);
    request.budget.max_evidence_bytes = request_byte_limit;
    let mut source_descriptor = descriptor("source", Provenance::LocalVerifier);
    source_descriptor.max_evidence_bytes = source_byte_limit;
    let mut registry = EvidenceSourceRegistry::new();
    registry.register(FixtureSource::returning(
        source_descriptor,
        EvidenceSourceOutput {
            evidence: vec![source_evidence("evidence-1", now_ms, now_ms + 2_000)],
            gaps: vec![],
        },
        Arc::new(Mutex::new(Vec::new())),
    ))?;
    registry
        .collect(&["source".into()], EvidenceRequest::from(&request), ingress)
        .await
}

#[tokio::test]
async fn collection_is_explicit_bound_ordered_and_preserves_partial_gaps() {
    let ingress = Ingress::capture().unwrap();
    let mut request = request(ingress.now_ms());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut registry = EvidenceSourceRegistry::new();
    registry
        .register(FixtureSource::returning(
            descriptor("a-reported", Provenance::SourceReported),
            EvidenceSourceOutput {
                evidence: vec![source_evidence(
                    "z-evidence",
                    ingress.now_ms(),
                    ingress.now_ms() + 2_000,
                )],
                gaps: vec![Scenario::TaskScope],
            },
            observations.clone(),
        ))
        .unwrap();
    registry
        .register(FixtureSource::returning(
            descriptor("z-local", Provenance::LocalVerifier),
            EvidenceSourceOutput {
                evidence: vec![
                    source_evidence("b-evidence", ingress.now_ms(), ingress.now_ms() + 2_000),
                    source_evidence("a-evidence", ingress.now_ms(), ingress.now_ms() + 2_000),
                ],
                gaps: vec![],
            },
            observations.clone(),
        ))
        .unwrap();

    let collection = registry
        .collect(
            &["z-local".into(), "a-reported".into()],
            EvidenceRequest::from(&request),
            ingress,
        )
        .await
        .unwrap();

    assert_eq!(
        collection
            .evidence
            .iter()
            .map(|item| (item.source_id.as_str(), item.id.as_str()))
            .collect::<Vec<_>>(),
        [
            ("a-reported", "z-evidence"),
            ("z-local", "a-evidence"),
            ("z-local", "b-evidence"),
        ]
    );
    assert!(collection
        .evidence
        .iter()
        .all(|item| collection.collected_at_ms >= item.captured_at_ms));
    assert_eq!(collection.evidence[0].binding, request.binding);
    assert_eq!(collection.evidence[0].source_revision, "revision-1");
    assert_eq!(
        collection.evidence[0].provenance,
        Provenance::SourceReported
    );
    assert_eq!(collection.evidence[1].provenance, Provenance::LocalVerifier);
    assert_eq!(collection.gaps.len(), 1);
    assert_eq!(collection.gaps[0].source_id, "a-reported");
    assert_eq!(collection.gaps[0].scenario, Scenario::TaskScope);
    assert_eq!(
        collection
            .reports
            .iter()
            .map(|report| (report.source_id.as_str(), report.status))
            .collect::<Vec<_>>(),
        [
            ("a-reported", EvidenceSourceStatus::Partial),
            ("z-local", EvidenceSourceStatus::Complete),
        ]
    );
    assert_eq!(collection.reports[0].source_revision, "revision-1");
    assert_eq!(collection.reports[0].evidence_items, 1);
    assert_eq!(
        collection.reports[0].evidence_bytes,
        source_evidence_bytes(&collection, "a-reported")
    );
    assert_eq!(collection.reports[0].gap_count, 1);
    assert_eq!(collection.reports[1].source_revision, "revision-1");
    assert_eq!(collection.reports[1].evidence_items, 2);
    assert_eq!(
        collection.reports[1].evidence_bytes,
        source_evidence_bytes(&collection, "z-local")
    );
    assert_eq!(collection.reports[1].gap_count, 0);

    let observed = observations.lock().unwrap().clone();
    assert_eq!(
        observed
            .iter()
            .map(|item| item.source_id.as_str())
            .collect::<Vec<_>>(),
        ["a-reported", "z-local"]
    );
    assert!(observed
        .iter()
        .all(|item| item.request_id == request.request_id
            && item.scenarios == request.scenarios
            && item.task_text.as_deref() == Some("Replace the generated resource")
            && item.candidate == request.candidate));

    request.check_at_ms = collection.collected_at_ms;
    request.evidence = collection.evidence;
    request.gaps = collection.gaps;
    let response = check(&request).unwrap();
    assert_eq!(response.outcome.control.candidate, CandidateControl::Block);
    assert!(response
        .answers
        .iter()
        .any(|answer| answer.reason == Reason::EvidenceGap));

    request.scenarios = vec![Scenario::Preservation];
    request
        .evidence
        .retain(|item| item.source_id == "a-reported");
    request.gaps.clear();
    let response = check(&request).unwrap();
    assert_eq!(response.answers[0].reason, Reason::UntrustedEvidence);
    assert_eq!(response.outcome.control.candidate, CandidateControl::Block);
}

#[tokio::test]
async fn unavailable_timeout_and_unusable_lifetimes_become_ordered_gaps() {
    let ingress = Ingress::capture().unwrap();
    let request = request(ingress.now_ms());
    let now = ingress.now_ms();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let pending_completed = Arc::new(AtomicBool::new(false));
    let mut registry = EvidenceSourceRegistry::new();

    for (source_id, evidence) in [
        (
            "future",
            source_evidence("future-id", now + 10_000, now + 11_000),
        ),
        (
            "invalid-lifetime",
            source_evidence("invalid-id", now + 10, now + 5),
        ),
        (
            "over-age",
            source_evidence("old-id", now - 100, now + 2_000),
        ),
        ("stale", source_evidence("stale-id", now - 10, now + 1)),
    ] {
        let mut descriptor = descriptor(source_id, Provenance::SourceReported);
        if source_id == "over-age" {
            descriptor.max_age_ms = 1;
        }
        registry
            .register(FixtureSource::returning(
                descriptor,
                EvidenceSourceOutput {
                    evidence: vec![evidence],
                    gaps: vec![],
                },
                observations.clone(),
            ))
            .unwrap();
    }
    let mut pending_descriptor = descriptor("pending", Provenance::SourceReported);
    pending_descriptor.timeout_ms = 10;
    registry
        .register(FixtureSource {
            descriptor: pending_descriptor,
            behavior: Behavior::Pending(pending_completed.clone()),
            observations: observations.clone(),
        })
        .unwrap();
    registry
        .register(FixtureSource::unavailable(
            descriptor("unavailable", Provenance::SourceReported),
            observations.clone(),
        ))
        .unwrap();

    let selected = [
        "unavailable".into(),
        "stale".into(),
        "pending".into(),
        "over-age".into(),
        "invalid-lifetime".into(),
        "future".into(),
    ];
    let collection = registry
        .collect(&selected, EvidenceRequest::from(&request), ingress)
        .await
        .unwrap();

    assert!(collection.evidence.is_empty());
    assert_eq!(collection.gaps.len(), selected.len() * 2);
    for pair in collection.gaps.chunks_exact(2) {
        assert_eq!(pair[0].source_id, pair[1].source_id);
        assert_eq!(pair[0].scenario, Scenario::Preservation);
        assert_eq!(pair[1].scenario, Scenario::TaskScope);
    }
    assert_eq!(
        collection
            .reports
            .iter()
            .map(|report| (report.source_id.as_str(), report.status))
            .collect::<Vec<_>>(),
        [
            ("future", EvidenceSourceStatus::Stale),
            ("invalid-lifetime", EvidenceSourceStatus::Stale),
            ("over-age", EvidenceSourceStatus::Stale),
            ("pending", EvidenceSourceStatus::Timeout),
            ("stale", EvidenceSourceStatus::Stale),
            ("unavailable", EvidenceSourceStatus::Unavailable),
        ]
    );
    assert!(collection.reports.iter().all(|report| {
        report.source_revision == "revision-1"
            && report.evidence_items == 0
            && report.evidence_bytes == 0
            && report.gap_count == 2
    }));
    assert!(!pending_completed.load(Ordering::SeqCst));
    let observed = observations.lock().unwrap();
    assert_eq!(observed.len(), selected.len());
    let pending = observed
        .iter()
        .find(|item| item.source_id == "pending")
        .unwrap();
    assert_eq!(
        pending.deadline,
        ingress.deadline_at(request.budget.deadline_ms, 10)
    );
}

#[tokio::test]
async fn invalid_outputs_and_tight_source_or_global_budgets_are_hard_errors() {
    let cases = [
        "invalid-id",
        "unsupported-gap",
        "duplicate-gap",
        "source-items",
        "source-bytes",
        "global-items",
        "global-bytes",
    ];
    for case in cases {
        let ingress = Ingress::capture().unwrap();
        let mut request = request(ingress.now_ms());
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut source_descriptor = descriptor("source", Provenance::LocalVerifier);
        let mut output = EvidenceSourceOutput {
            evidence: vec![source_evidence(
                "evidence-1",
                ingress.now_ms(),
                ingress.now_ms() + 1_000,
            )],
            gaps: vec![],
        };
        match case {
            "invalid-id" => output.evidence[0].id = "invalid id".into(),
            "unsupported-gap" => {
                source_descriptor.supported_scenarios = vec![Scenario::Preservation];
                output.gaps = vec![Scenario::TaskScope];
            }
            "duplicate-gap" => output.gaps = vec![Scenario::Preservation, Scenario::Preservation],
            "source-items" => {
                source_descriptor.max_evidence_items = 1;
                output.evidence.push(source_evidence(
                    "evidence-2",
                    ingress.now_ms(),
                    ingress.now_ms() + 1_000,
                ));
            }
            "source-bytes" => source_descriptor.max_evidence_bytes = 1,
            "global-items" => request.budget.max_evidence_items = 0,
            "global-bytes" => request.budget.max_evidence_bytes = 1,
            _ => unreachable!(),
        }
        let mut registry = EvidenceSourceRegistry::new();
        registry
            .register(FixtureSource::returning(
                source_descriptor,
                output,
                observations,
            ))
            .unwrap();
        let expected = match case {
            "invalid-id" | "unsupported-gap" | "duplicate-gap" => {
                EvidenceCollectionError::InvalidOutput
            }
            _ => EvidenceCollectionError::EvidenceBudgetExceeded,
        };
        assert_eq!(
            registry
                .collect(&["source".into()], EvidenceRequest::from(&request), ingress,)
                .await,
            Err(expected),
            "{case}"
        );
    }

    let ingress = Ingress::capture().unwrap();
    let request = request(ingress.now_ms());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut registry = EvidenceSourceRegistry::new();
    for source_id in ["a", "b"] {
        registry
            .register(FixtureSource::returning(
                descriptor(source_id, Provenance::LocalVerifier),
                EvidenceSourceOutput {
                    evidence: vec![source_evidence(
                        "duplicate-id",
                        ingress.now_ms(),
                        ingress.now_ms() + 1_000,
                    )],
                    gaps: vec![],
                },
                observations.clone(),
            ))
            .unwrap();
    }
    assert_eq!(
        registry
            .collect(
                &["a".into(), "b".into()],
                EvidenceRequest::from(&request),
                ingress,
            )
            .await,
        Err(EvidenceCollectionError::InvalidOutput)
    );
}

#[tokio::test]
async fn registration_selection_and_context_are_validated_before_collection() {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let output = EvidenceSourceOutput::default();
    let mut registry = EvidenceSourceRegistry::new();
    let valid = descriptor("valid", Provenance::SourceReported);
    registry
        .register(FixtureSource::returning(
            valid.clone(),
            output.clone(),
            observations.clone(),
        ))
        .unwrap();
    assert_eq!(
        registry.register(FixtureSource::returning(
            valid,
            output.clone(),
            observations.clone(),
        )),
        Err(EvidenceCollectionError::DuplicateSource)
    );

    for mut invalid in [
        descriptor("invalid id", Provenance::SourceReported),
        descriptor("duplicate-scenarios", Provenance::SourceReported),
        descriptor("zero-items", Provenance::SourceReported),
        descriptor("zero-bytes", Provenance::SourceReported),
        descriptor("too-many-items", Provenance::SourceReported),
        descriptor("too-many-bytes", Provenance::SourceReported),
        descriptor("too-old", Provenance::SourceReported),
        descriptor("too-slow", Provenance::SourceReported),
    ] {
        match invalid.source_id.as_str() {
            "duplicate-scenarios" => {
                invalid.supported_scenarios = vec![Scenario::Preservation, Scenario::Preservation]
            }
            "zero-items" => invalid.max_evidence_items = 0,
            "zero-bytes" => invalid.max_evidence_bytes = 0,
            "too-many-items" => invalid.max_evidence_items = MAX_EVIDENCE_ITEMS + 1,
            "too-many-bytes" => invalid.max_evidence_bytes = MAX_MESSAGE_BYTES + 1,
            "too-old" => invalid.max_age_ms = MAX_SOURCE_AGE_MS + 1,
            "too-slow" => invalid.timeout_ms = MAX_SOURCE_TIMEOUT_MS + 1,
            _ => {}
        }
        assert_eq!(
            EvidenceSourceRegistry::new().register(FixtureSource::returning(
                invalid,
                output.clone(),
                observations.clone(),
            )),
            Err(EvidenceCollectionError::InvalidDescriptor)
        );
    }

    let ingress = Ingress::capture().unwrap();
    let request = request(ingress.now_ms());
    for selected in [
        vec!["invalid id".into()],
        vec!["missing".into()],
        vec!["valid".into(), "valid".into()],
    ] {
        assert_eq!(
            registry
                .collect(&selected, EvidenceRequest::from(&request), ingress)
                .await,
            Err(EvidenceCollectionError::InvalidSelection)
        );
    }
    let mut invalid_context = request.clone();
    invalid_context.candidate.resource_id.clear();
    assert_eq!(
        registry
            .collect(
                &["valid".into()],
                EvidenceRequest::from(&invalid_context),
                ingress,
            )
            .await,
        Err(EvidenceCollectionError::InvalidContext)
    );
    assert!(observations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn source_and_request_byte_limits_accept_exact_boundaries() {
    let ingress = Ingress::capture().unwrap();
    let now = ingress.now_ms();
    let baseline = collect_one(ingress, now, MAX_MESSAGE_BYTES, MAX_MESSAGE_BYTES)
        .await
        .unwrap();
    let evidence_bytes = serde_json::to_vec(&baseline.evidence).unwrap().len();
    assert_eq!(baseline.reports[0].evidence_bytes, evidence_bytes);

    assert!(collect_one(ingress, now, evidence_bytes, MAX_MESSAGE_BYTES)
        .await
        .is_ok());
    assert_eq!(
        collect_one(ingress, now, evidence_bytes - 1, MAX_MESSAGE_BYTES).await,
        Err(EvidenceCollectionError::EvidenceBudgetExceeded)
    );
    assert!(collect_one(ingress, now, MAX_MESSAGE_BYTES, evidence_bytes)
        .await
        .is_ok());
    assert_eq!(
        collect_one(ingress, now, MAX_MESSAGE_BYTES, evidence_bytes - 1).await,
        Err(EvidenceCollectionError::EvidenceBudgetExceeded)
    );
}

#[tokio::test]
async fn empty_success_batch_counts_compact_array_bytes() {
    let ingress = Ingress::capture().unwrap();
    let now = ingress.now_ms();
    let request = request(now);
    let mut exact_descriptor = descriptor("exact", Provenance::SourceReported);
    exact_descriptor.max_evidence_bytes = 2;
    let mut exact_registry = EvidenceSourceRegistry::new();
    exact_registry
        .register(FixtureSource::returning(
            exact_descriptor,
            EvidenceSourceOutput::default(),
            Arc::new(Mutex::new(Vec::new())),
        ))
        .unwrap();

    let collection = exact_registry
        .collect(&["exact".into()], EvidenceRequest::from(&request), ingress)
        .await
        .unwrap();
    assert!(collection.evidence.is_empty());
    assert_eq!(collection.reports[0].evidence_bytes, 2);

    let mut tight_descriptor = descriptor("tight", Provenance::SourceReported);
    tight_descriptor.max_evidence_bytes = 1;
    let mut tight_registry = EvidenceSourceRegistry::new();
    tight_registry
        .register(FixtureSource::returning(
            tight_descriptor,
            EvidenceSourceOutput::default(),
            Arc::new(Mutex::new(Vec::new())),
        ))
        .unwrap();
    assert_eq!(
        tight_registry
            .collect(&["tight".into()], EvidenceRequest::from(&request), ingress,)
            .await,
        Err(EvidenceCollectionError::EvidenceBudgetExceeded)
    );
}

#[tokio::test]
async fn collection_timestamp_finalizes_the_recorded_check_clock() {
    let ingress = Ingress::capture().unwrap();
    let now = ingress.now_ms();
    let mut request = request(now);
    let mut registry = EvidenceSourceRegistry::new();
    registry
        .register(FixtureSource::delayed(
            descriptor("source", Provenance::LocalVerifier),
            Duration::from_millis(30),
            EvidenceSourceOutput {
                evidence: vec![source_evidence("evidence", now + 10, now + 2_000)],
                gaps: vec![],
            },
            Arc::new(Mutex::new(Vec::new())),
        ))
        .unwrap();

    let collection = registry
        .collect(&["source".into()], EvidenceRequest::from(&request), ingress)
        .await
        .unwrap();
    assert!(request.check_at_ms < collection.evidence[0].captured_at_ms);
    request.evidence = collection.evidence;
    request.gaps = collection.gaps;
    let stale = check(&request).unwrap();
    assert_eq!(stale.outcome.control.candidate, CandidateControl::Block);
    assert!(stale
        .answers
        .iter()
        .any(|answer| answer.reason == Reason::StaleEvidence));

    request.check_at_ms = collection.collected_at_ms;
    let response = check(&request).unwrap();
    assert_eq!(response.outcome.control.candidate, CandidateControl::Allow);
}

#[tokio::test]
async fn duplicate_ids_in_a_discarded_stale_batch_are_rejected() {
    let ingress = Ingress::capture().unwrap();
    let now = ingress.now_ms();
    let request = request(now);
    let mut registry = EvidenceSourceRegistry::new();
    registry
        .register(FixtureSource::returning(
            descriptor("a-stale", Provenance::LocalVerifier),
            EvidenceSourceOutput {
                evidence: vec![source_evidence("duplicate", now, now)],
                gaps: vec![],
            },
            Arc::new(Mutex::new(Vec::new())),
        ))
        .unwrap();
    registry
        .register(FixtureSource::returning(
            descriptor("z-valid", Provenance::LocalVerifier),
            EvidenceSourceOutput {
                evidence: vec![source_evidence("duplicate", now, now + 2_000)],
                gaps: vec![],
            },
            Arc::new(Mutex::new(Vec::new())),
        ))
        .unwrap();

    assert_eq!(
        registry
            .collect(
                &["z-valid".into(), "a-stale".into()],
                EvidenceRequest::from(&request),
                ingress,
            )
            .await,
        Err(EvidenceCollectionError::InvalidOutput)
    );
}

#[tokio::test]
async fn final_freeze_discards_an_earlier_batch_expired_by_a_later_source() {
    let ingress = Ingress::capture().unwrap();
    let now = ingress.now_ms();
    let request = request(now);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut registry = EvidenceSourceRegistry::new();

    let mut early_descriptor = descriptor("a-early", Provenance::LocalVerifier);
    early_descriptor.source_revision = "early-revision".into();
    registry
        .register(FixtureSource::returning(
            early_descriptor,
            EvidenceSourceOutput {
                evidence: vec![source_evidence("early-evidence", now, now + 200)],
                gaps: vec![Scenario::TaskScope],
            },
            observations.clone(),
        ))
        .unwrap();
    let mut late_descriptor = descriptor("z-late", Provenance::LocalVerifier);
    late_descriptor.source_revision = "late-revision".into();
    registry
        .register(FixtureSource::delayed(
            late_descriptor,
            Duration::from_millis(350),
            EvidenceSourceOutput {
                evidence: vec![source_evidence("late-evidence", now, now + 2_000)],
                gaps: vec![],
            },
            observations,
        ))
        .unwrap();

    let collection = registry
        .collect(
            &["z-late".into(), "a-early".into()],
            EvidenceRequest::from(&request),
            ingress,
        )
        .await
        .unwrap();

    assert!(collection.collected_at_ms >= now + 300);
    assert_eq!(collection.evidence.len(), 1);
    assert_eq!(collection.evidence[0].source_id, "z-late");
    assert_eq!(
        collection
            .gaps
            .iter()
            .map(|gap| (gap.source_id.as_str(), gap.scenario))
            .collect::<Vec<_>>(),
        [
            ("a-early", Scenario::Preservation),
            ("a-early", Scenario::TaskScope),
        ]
    );
    assert_eq!(collection.reports[0].source_id, "a-early");
    assert_eq!(collection.reports[0].source_revision, "early-revision");
    assert_eq!(collection.reports[0].status, EvidenceSourceStatus::Stale);
    assert_eq!(collection.reports[0].evidence_items, 0);
    assert_eq!(collection.reports[0].evidence_bytes, 0);
    assert_eq!(collection.reports[0].gap_count, 2);
    assert_eq!(collection.reports[1].source_id, "z-late");
    assert_eq!(collection.reports[1].source_revision, "late-revision");
    assert_eq!(collection.reports[1].evidence_items, 1);
    assert_eq!(
        collection.reports[1].evidence_bytes,
        source_evidence_bytes(&collection, "z-late")
    );
    assert_eq!(collection.reports[1].gap_count, 0);
}

#[tokio::test]
async fn collection_order_is_independent_of_registration_and_selection_order() {
    let ingress = Ingress::capture().unwrap();
    let now = ingress.now_ms();
    let request = request(now);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let source = |source_id: &str, evidence_id: &str, gaps: Vec<Scenario>| {
        FixtureSource::returning(
            descriptor(source_id, Provenance::LocalVerifier),
            EvidenceSourceOutput {
                evidence: vec![source_evidence(evidence_id, now, now + 2_000)],
                gaps,
            },
            observations.clone(),
        )
    };

    let mut first = EvidenceSourceRegistry::new();
    first
        .register(source("a-source", "z-evidence", vec![Scenario::TaskScope]))
        .unwrap();
    first
        .register(source("z-source", "a-evidence", vec![]))
        .unwrap();
    let mut second = EvidenceSourceRegistry::new();
    second
        .register(source("z-source", "a-evidence", vec![]))
        .unwrap();
    second
        .register(source("a-source", "z-evidence", vec![Scenario::TaskScope]))
        .unwrap();

    let first_collection = first
        .collect(
            &["z-source".into(), "a-source".into()],
            EvidenceRequest::from(&request),
            ingress,
        )
        .await
        .unwrap();
    let second_collection = second
        .collect(
            &["a-source".into(), "z-source".into()],
            EvidenceRequest::from(&request),
            ingress,
        )
        .await
        .unwrap();

    assert_eq!(first_collection.evidence, second_collection.evidence);
    assert_eq!(first_collection.gaps, second_collection.gaps);
    assert_eq!(first_collection.reports, second_collection.reports);
}

#[test]
fn collection_error_variants_have_stable_sanitized_wire_names() {
    for (error, name) in [
        (
            EvidenceCollectionError::InvalidDescriptor,
            "invalid_descriptor",
        ),
        (EvidenceCollectionError::DuplicateSource, "duplicate_source"),
        (
            EvidenceCollectionError::InvalidSelection,
            "invalid_selection",
        ),
        (EvidenceCollectionError::InvalidContext, "invalid_context"),
        (EvidenceCollectionError::InvalidOutput, "invalid_output"),
        (
            EvidenceCollectionError::EvidenceBudgetExceeded,
            "evidence_budget_exceeded",
        ),
    ] {
        assert_eq!(error.to_string(), name);
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            format!("\"{name}\"")
        );
    }
}
