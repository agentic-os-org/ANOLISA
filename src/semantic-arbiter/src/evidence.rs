//! Explicit, bounded evidence-source registration and collection.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::Instant;

use crate::contract::{
    Binding, Budget, Candidate, Evidence, EvidenceFact, EvidenceGap, Provenance, Scenario, Task,
};
use crate::deadline::Ingress;
use crate::protocol::{CheckRequest, MAX_EVIDENCE_ITEMS, MAX_MESSAGE_BYTES};
use crate::validation::{self, Input};

/// Maximum source collection timeout accepted at registration.
pub const MAX_SOURCE_TIMEOUT_MS: u64 = 120_000;
/// Maximum evidence age accepted at registration.
pub const MAX_SOURCE_AGE_MS: u64 = 120_000;

/// Frozen context visible to an evidence source.
#[derive(Clone, Copy, Debug)]
pub struct EvidenceRequest<'a> {
    /// Consumer-owned request identity.
    pub request_id: &'a str,
    /// Immutable task, candidate, workspace and policy association.
    pub binding: &'a Binding,
    /// Original event deadline and evidence limits.
    pub budget: &'a Budget,
    /// Explicitly selected scenarios.
    pub scenarios: &'a [Scenario],
    /// Authoritative task, or explicit absence.
    pub task: Option<&'a Task>,
    /// Frozen candidate operation.
    pub candidate: &'a Candidate,
}

impl<'a> From<&'a CheckRequest> for EvidenceRequest<'a> {
    fn from(value: &'a CheckRequest) -> Self {
        Self {
            request_id: &value.request_id,
            binding: &value.binding,
            budget: &value.budget,
            scenarios: &value.scenarios,
            task: value.task.as_ref(),
            candidate: &value.candidate,
        }
    }
}

/// Immutable source limits and provenance retained by the registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceSourceDescriptor {
    /// Explicit registry key.
    pub source_id: String,
    /// Source implementation or content revision.
    pub source_revision: String,
    /// Trust annotation injected into every collected item.
    pub provenance: Provenance,
    /// Scenarios for which this source can report coverage.
    pub supported_scenarios: Vec<Scenario>,
    /// Per-attempt item limit.
    pub max_evidence_items: usize,
    /// Per-attempt serialized evidence-array limit.
    pub max_evidence_bytes: usize,
    /// Maximum accepted age at collection completion.
    pub max_age_ms: u64,
    /// Collection cap measured from the original ingress.
    pub timeout_ms: u64,
}

/// Evidence fields supplied by a source before registry-owned context is injected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEvidence {
    /// Unique evidence identifier.
    pub id: String,
    /// Collection time in Unix milliseconds.
    pub captured_at_ms: u64,
    /// Exclusive validity deadline in Unix milliseconds.
    pub valid_until_ms: u64,
    /// Typed, non-executable fact.
    pub fact: EvidenceFact,
}

/// One source attempt, including explicit coverage omissions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EvidenceSourceOutput {
    /// Candidate evidence to validate and bind.
    pub evidence: Vec<SourceEvidence>,
    /// Supported selected scenarios not covered by this attempt.
    pub gaps: Vec<Scenario>,
}

/// Sanitized source failure without provider text or transport details.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EvidenceSourceError {
    /// The source could not provide a batch.
    #[error("unavailable")]
    Unavailable,
}

/// Stable registry failures without source output or contract details.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCollectionError {
    /// A source descriptor violates registry identity, coverage, or limit constraints.
    #[error("invalid_descriptor")]
    InvalidDescriptor,
    /// A source identity is already registered.
    #[error("duplicate_source")]
    DuplicateSource,
    /// Selected source identities or their requested coverage are invalid.
    #[error("invalid_selection")]
    InvalidSelection,
    /// The frozen request context violates its contract.
    #[error("invalid_context")]
    InvalidContext,
    /// Source output violates identity, coverage, or uniqueness constraints.
    #[error("invalid_output")]
    InvalidOutput,
    /// A source or request evidence item/byte budget was exceeded.
    #[error("evidence_budget_exceeded")]
    EvidenceBudgetExceeded,
}

/// In-process source invoked once under a registry-owned absolute deadline.
pub trait EvidenceSource: Send + Sync {
    /// Return the immutable identity, coverage and limits registered for this source.
    fn descriptor(&self) -> EvidenceSourceDescriptor;

    /// Collect one batch without retry, fallback or deadline renewal.
    ///
    /// Implementations must return the future promptly and remain cooperatively asynchronous while
    /// it is polled. Blocking work cannot be preempted by the registry's Tokio deadline.
    fn collect<'a>(
        &'a self,
        request: EvidenceRequest<'a>,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<EvidenceSourceOutput, EvidenceSourceError>> + Send + 'a>>;
}

/// Stable outcome of one source attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceStatus {
    /// The source returned a valid batch without explicit gaps.
    Complete,
    /// The valid batch retained one or more explicit gaps.
    Partial,
    /// The absolute source deadline expired.
    Timeout,
    /// The source reported no available batch.
    Unavailable,
    /// The entire batch was discarded because its lifetime was unusable.
    Stale,
}

/// Sanitized source-local collection report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSourceReport {
    /// Registered source identity.
    pub source_id: String,
    /// Registered source implementation or content revision.
    pub source_revision: String,
    /// Stable result classification.
    pub status: EvidenceSourceStatus,
    /// Evidence items retained from this source.
    pub evidence_items: usize,
    /// Serialized evidence-array bytes retained from this source.
    pub evidence_bytes: usize,
    /// Coverage gaps retained from this source.
    pub gap_count: usize,
}

/// Deterministically ordered evidence, gaps and source reports.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCollection {
    /// Final Unix millisecond instant shared by all lifetime checks.
    pub collected_at_ms: u64,
    /// Evidence ordered by source and evidence identifier.
    pub evidence: Vec<Evidence>,
    /// Gaps ordered by source and canonical scenario order.
    pub gaps: Vec<EvidenceGap>,
    /// Reports ordered by source identifier.
    pub reports: Vec<EvidenceSourceReport>,
}

struct Registration {
    descriptor: EvidenceSourceDescriptor,
    source: Box<dyn EvidenceSource>,
}

/// Explicit registry with no global, default or dynamic-plugin discovery.
#[derive(Default)]
pub struct EvidenceSourceRegistry {
    sources: BTreeMap<String, Registration>,
}

impl EvidenceSourceRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one validated source under its descriptor identity.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceCollectionError::InvalidDescriptor`] for a malformed descriptor or
    /// [`EvidenceCollectionError::DuplicateSource`] when its identity is already registered.
    pub fn register<S>(&mut self, source: S) -> Result<(), EvidenceCollectionError>
    where
        S: EvidenceSource + 'static,
    {
        let mut descriptor = source.descriptor();
        validate_descriptor(&descriptor)?;
        canonicalize_scenarios(&mut descriptor.supported_scenarios);
        if self.sources.contains_key(&descriptor.source_id) {
            return Err(EvidenceCollectionError::DuplicateSource);
        }
        self.sources.insert(
            descriptor.source_id.clone(),
            Registration {
                descriptor,
                source: Box::new(source),
            },
        );
        Ok(())
    }

    /// Inspect a registered descriptor without invoking its source.
    pub fn descriptor(&self, source_id: &str) -> Option<&EvidenceSourceDescriptor> {
        self.sources
            .get(source_id)
            .map(|registration| &registration.descriptor)
    }

    /// Collect selected sources serially under the original ingress and event budget.
    ///
    /// The completion instant is frozen once after every attempt and is used to recheck every
    /// successful batch before any evidence is returned.
    ///
    /// # Errors
    ///
    /// Returns a stable collection error for invalid context, selection, source output, duplicate
    /// evidence identifiers, or any source/request evidence budget excess.
    pub async fn collect(
        &self,
        selected_ids: &[String],
        request: EvidenceRequest<'_>,
        ingress: Ingress,
    ) -> Result<EvidenceCollection, EvidenceCollectionError> {
        validate_collection_context(&request, ingress)?;
        let potential_gaps = validate_selection(self, selected_ids, request.scenarios)?;
        let mut selected = selected_ids.iter().map(String::as_str).collect::<Vec<_>>();
        selected.sort_unstable();

        let mut batches = Vec::with_capacity(selected.len());
        let mut evidence_ids = BTreeSet::new();
        for source_id in selected {
            let registration = self
                .sources
                .get(source_id)
                .ok_or(EvidenceCollectionError::InvalidSelection)?;
            let supported = supported_scenarios(&registration.descriptor, request.scenarios);
            let deadline = ingress.deadline_at(
                request.budget.deadline_ms,
                registration.descriptor.timeout_ms,
            );
            let result = if Instant::now() >= deadline {
                None
            } else {
                tokio::time::timeout_at(deadline, registration.source.collect(request, deadline))
                    .await
                    .ok()
            };
            let result = if Instant::now() >= deadline {
                None
            } else {
                result
            };
            match result {
                None => batches.push(failed_batch(
                    &registration.descriptor,
                    supported,
                    EvidenceSourceStatus::Timeout,
                )),
                Some(Err(EvidenceSourceError::Unavailable)) => batches.push(failed_batch(
                    &registration.descriptor,
                    supported,
                    EvidenceSourceStatus::Unavailable,
                )),
                Some(Ok(output)) => {
                    let prepared = prepare_output(
                        output,
                        &registration.descriptor,
                        &supported,
                        &request,
                        ingress.now_ms(),
                    )?;
                    for evidence in &prepared.evidence {
                        if !evidence_ids.insert(evidence.id.clone()) {
                            return Err(EvidenceCollectionError::InvalidOutput);
                        }
                    }
                    if prepared.stale {
                        batches.push(failed_batch(
                            &registration.descriptor,
                            supported,
                            EvidenceSourceStatus::Stale,
                        ));
                        continue;
                    }
                    let status = if prepared.gaps.is_empty() {
                        EvidenceSourceStatus::Complete
                    } else {
                        EvidenceSourceStatus::Partial
                    };
                    batches.push(CollectedBatch {
                        source_id: source_id.to_owned(),
                        source_revision: registration.descriptor.source_revision.clone(),
                        supported,
                        max_age_ms: registration.descriptor.max_age_ms,
                        evidence: prepared.evidence,
                        evidence_bytes: prepared.evidence_bytes,
                        gaps: prepared.gaps,
                        status,
                    });
                }
            }
        }

        let collected_at_ms = ingress.now_ms();
        for batch in &mut batches {
            if matches!(
                batch.status,
                EvidenceSourceStatus::Complete | EvidenceSourceStatus::Partial
            ) && batch
                .evidence
                .iter()
                .any(|evidence| evidence_is_stale(evidence, collected_at_ms, batch.max_age_ms))
            {
                batch.discard_as_stale();
            }
        }

        let evidence = batches
            .iter()
            .flat_map(|batch| batch.evidence.iter().cloned())
            .collect::<Vec<_>>();
        if evidence.len() > request.budget.max_evidence_items {
            return Err(EvidenceCollectionError::EvidenceBudgetExceeded);
        }
        if serialized_len(&evidence)? > request.budget.max_evidence_bytes {
            return Err(EvidenceCollectionError::EvidenceBudgetExceeded);
        }
        let gaps = batches
            .iter()
            .flat_map(|batch| batch.gaps.iter().cloned())
            .collect::<Vec<_>>();
        debug_assert!(gaps.len() <= potential_gaps);
        let reports = batches.iter().map(CollectedBatch::report).collect();
        validation::context(
            &Input {
                request_id: request.request_id,
                binding: request.binding,
                budget: request.budget,
                scenarios: request.scenarios,
                task: request.task,
                candidate: request.candidate,
                evidence: &evidence,
                gaps: &gaps,
            },
            collected_at_ms,
        )
        .map_err(|_| EvidenceCollectionError::InvalidOutput)?;
        Ok(EvidenceCollection {
            collected_at_ms,
            evidence,
            gaps,
            reports,
        })
    }
}

struct PreparedOutput {
    evidence: Vec<Evidence>,
    evidence_bytes: usize,
    gaps: Vec<EvidenceGap>,
    stale: bool,
}

struct CollectedBatch {
    source_id: String,
    source_revision: String,
    supported: Vec<Scenario>,
    max_age_ms: u64,
    evidence: Vec<Evidence>,
    evidence_bytes: usize,
    gaps: Vec<EvidenceGap>,
    status: EvidenceSourceStatus,
}

impl CollectedBatch {
    fn discard_as_stale(&mut self) {
        self.evidence.clear();
        self.evidence_bytes = 0;
        self.gaps = gaps_for(&self.source_id, &self.supported);
        self.status = EvidenceSourceStatus::Stale;
    }

    fn report(&self) -> EvidenceSourceReport {
        EvidenceSourceReport {
            source_id: self.source_id.clone(),
            source_revision: self.source_revision.clone(),
            status: self.status,
            evidence_items: self.evidence.len(),
            evidence_bytes: self.evidence_bytes,
            gap_count: self.gaps.len(),
        }
    }
}

fn validate_descriptor(
    descriptor: &EvidenceSourceDescriptor,
) -> Result<(), EvidenceCollectionError> {
    validation::id(&descriptor.source_id)
        .map_err(|_| EvidenceCollectionError::InvalidDescriptor)?;
    validation::id(&descriptor.source_revision)
        .map_err(|_| EvidenceCollectionError::InvalidDescriptor)?;
    if descriptor.supported_scenarios.is_empty()
        || descriptor.supported_scenarios.len() > 2
        || descriptor.max_evidence_items == 0
        || descriptor.max_evidence_items > MAX_EVIDENCE_ITEMS
        || descriptor.max_evidence_bytes == 0
        || descriptor.max_evidence_bytes > MAX_MESSAGE_BYTES
        || descriptor.max_age_ms == 0
        || descriptor.max_age_ms > MAX_SOURCE_AGE_MS
        || descriptor.timeout_ms == 0
        || descriptor.timeout_ms > MAX_SOURCE_TIMEOUT_MS
        || (descriptor.supported_scenarios.len() == 2
            && descriptor.supported_scenarios[0] == descriptor.supported_scenarios[1])
    {
        return Err(EvidenceCollectionError::InvalidDescriptor);
    }
    Ok(())
}

fn validate_collection_context(
    request: &EvidenceRequest<'_>,
    ingress: Ingress,
) -> Result<(), EvidenceCollectionError> {
    validation::context(
        &Input {
            request_id: request.request_id,
            binding: request.binding,
            budget: request.budget,
            scenarios: request.scenarios,
            task: request.task,
            candidate: request.candidate,
            evidence: &[],
            gaps: &[],
        },
        ingress.now_ms(),
    )
    .map_err(|_| EvidenceCollectionError::InvalidContext)
}

fn validate_selection(
    registry: &EvidenceSourceRegistry,
    selected_ids: &[String],
    scenarios: &[Scenario],
) -> Result<usize, EvidenceCollectionError> {
    if selected_ids.is_empty() || selected_ids.len() > MAX_EVIDENCE_ITEMS {
        return Err(EvidenceCollectionError::InvalidSelection);
    }
    let mut seen = BTreeSet::new();
    let mut potential_gaps = 0_usize;
    for source_id in selected_ids {
        validation::id(source_id).map_err(|_| EvidenceCollectionError::InvalidSelection)?;
        if !seen.insert(source_id) {
            return Err(EvidenceCollectionError::InvalidSelection);
        }
        let registration = registry
            .sources
            .get(source_id)
            .ok_or(EvidenceCollectionError::InvalidSelection)?;
        let supported_count = supported_scenarios(&registration.descriptor, scenarios).len();
        if supported_count == 0 {
            return Err(EvidenceCollectionError::InvalidSelection);
        }
        potential_gaps = potential_gaps
            .checked_add(supported_count)
            .filter(|count| *count <= MAX_EVIDENCE_ITEMS)
            .ok_or(EvidenceCollectionError::InvalidSelection)?;
    }
    Ok(potential_gaps)
}

fn prepare_output(
    mut output: EvidenceSourceOutput,
    descriptor: &EvidenceSourceDescriptor,
    supported: &[Scenario],
    request: &EvidenceRequest<'_>,
    now_ms: u64,
) -> Result<PreparedOutput, EvidenceCollectionError> {
    if output.evidence.len() > descriptor.max_evidence_items {
        return Err(EvidenceCollectionError::EvidenceBudgetExceeded);
    }
    output
        .evidence
        .sort_by(|left, right| left.id.cmp(&right.id));
    let mut ids = BTreeSet::new();
    for item in &output.evidence {
        validation::id(&item.id).map_err(|_| EvidenceCollectionError::InvalidOutput)?;
        if !ids.insert(&item.id) {
            return Err(EvidenceCollectionError::InvalidOutput);
        }
        match &item.fact {
            EvidenceFact::Rebuild { resource_id, .. } => {
                validation::id(resource_id).map_err(|_| EvidenceCollectionError::InvalidOutput)?
            }
        }
    }

    canonicalize_scenarios(&mut output.gaps);
    if output.gaps.len() > supported.len()
        || output
            .gaps
            .iter()
            .any(|scenario| !supported.contains(scenario))
        || (output.gaps.len() == 2 && output.gaps[0] == output.gaps[1])
    {
        return Err(EvidenceCollectionError::InvalidOutput);
    }
    let evidence = output
        .evidence
        .into_iter()
        .map(|item| Evidence {
            id: item.id,
            source_id: descriptor.source_id.clone(),
            source_revision: descriptor.source_revision.clone(),
            provenance: descriptor.provenance,
            binding: request.binding.clone(),
            captured_at_ms: item.captured_at_ms,
            valid_until_ms: item.valid_until_ms,
            fact: item.fact,
        })
        .collect::<Vec<_>>();
    let evidence_bytes = serialized_len(&evidence)?;
    if evidence_bytes > descriptor.max_evidence_bytes {
        return Err(EvidenceCollectionError::EvidenceBudgetExceeded);
    }
    let stale = evidence
        .iter()
        .any(|item| evidence_is_stale(item, now_ms, descriptor.max_age_ms));
    Ok(PreparedOutput {
        evidence,
        evidence_bytes,
        gaps: output
            .gaps
            .into_iter()
            .map(|scenario| EvidenceGap {
                source_id: descriptor.source_id.clone(),
                scenario,
            })
            .collect(),
        stale,
    })
}

fn failed_batch(
    descriptor: &EvidenceSourceDescriptor,
    supported: Vec<Scenario>,
    status: EvidenceSourceStatus,
) -> CollectedBatch {
    CollectedBatch {
        source_id: descriptor.source_id.clone(),
        source_revision: descriptor.source_revision.clone(),
        gaps: gaps_for(&descriptor.source_id, &supported),
        supported,
        max_age_ms: descriptor.max_age_ms,
        evidence: Vec::new(),
        evidence_bytes: 0,
        status,
    }
}

fn gaps_for(source_id: &str, supported: &[Scenario]) -> Vec<EvidenceGap> {
    supported
        .iter()
        .copied()
        .map(|scenario| EvidenceGap {
            source_id: source_id.to_owned(),
            scenario,
        })
        .collect()
}

fn supported_scenarios(
    descriptor: &EvidenceSourceDescriptor,
    requested: &[Scenario],
) -> Vec<Scenario> {
    canonical_scenarios(requested)
        .into_iter()
        .filter(|scenario| descriptor.supported_scenarios.contains(scenario))
        .collect()
}

fn evidence_is_stale(evidence: &Evidence, now_ms: u64, max_age_ms: u64) -> bool {
    evidence.captured_at_ms > now_ms
        || evidence.captured_at_ms >= evidence.valid_until_ms
        || evidence.valid_until_ms <= now_ms
        || now_ms.saturating_sub(evidence.captured_at_ms) > max_age_ms
}

fn serialized_len(value: &[Evidence]) -> Result<usize, EvidenceCollectionError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| EvidenceCollectionError::InvalidOutput)
}

fn canonicalize_scenarios(scenarios: &mut [Scenario]) {
    scenarios.sort_by_key(|scenario| scenario_order(*scenario));
}

fn canonical_scenarios(scenarios: &[Scenario]) -> Vec<Scenario> {
    [Scenario::Preservation, Scenario::TaskScope]
        .into_iter()
        .filter(|scenario| scenarios.contains(scenario))
        .collect()
}

fn scenario_order(scenario: Scenario) -> u8 {
    match scenario {
        Scenario::Preservation => 0,
        Scenario::TaskScope => 1,
    }
}
