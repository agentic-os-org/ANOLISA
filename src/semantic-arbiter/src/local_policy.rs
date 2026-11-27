//! Pure local protection policies over bounded consumer observations; signals never grant authority.

use serde::{Deserialize, Serialize};

use crate::contract::{AnswerValue, Candidate, Evidence, Reason, Scenario, UnitAnswer};
use crate::judgment;
use crate::protocol::ContractError;

/// State of an object at collection time, not a promise about execution time.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    /// No object was found through the bound directory.
    Absent,
    /// A regular file was inspected without following links.
    Regular,
    /// A directory requires a separate recursive inventory.
    Directory,
    /// A symbolic link was not followed.
    Symlink,
    /// The collector could not establish the object state.
    Unknown,
}

/// Comparison of a supported literal edit with the current file bytes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentDelta {
    /// Exact resulting bytes match the observed bytes.
    Unchanged,
    /// A supported edit changes the observed bytes.
    Changed,
    /// No exact resulting-byte comparison was available.
    Unknown,
}

/// A candidate-bound filesystem observation supplied by a trusted collector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileObservation {
    /// Identity of one candidate resource.
    pub resource_id: String,
    /// Observed object type or explicit uncertainty.
    pub state: FileState,
    /// Bounded content comparison, never inferred from a filename.
    pub delta: ContentDelta,
    /// Presence in the initial session inventory; incomplete inventories return None.
    pub existed_at_start: Option<bool>,
    /// Raw byte digest when a stable, bounded regular-file read succeeded.
    pub sha256: Option<String>,
}

/// Observable action categories, not a judgment of authorization or malice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Removal of a named object.
    Delete,
    /// Git history or working-state replacement.
    GitRewrite,
    /// Publication to a Git remote.
    GitPublish,
    /// Installation or mutation of dependencies.
    DependencyChange,
    /// A conventional configuration path is changed.
    ConfigChange,
    /// A conventional CI workflow path is changed.
    SharedWorkflowChange,
    /// A conventional test path is changed.
    TestChange,
    /// A recognized check-skipping option or edit is present.
    CheckBypass,
    /// A recognized send or publish operation is present.
    ExternalCommunication,
    /// A recognized network diagnostic is present.
    NetworkProbe,
    /// A recognized external-code retrieval is present.
    ExternalCode,
    /// A recognized persistent installation target is present.
    Persistence,
    /// A recognized system-clock mutation is present.
    ClockChange,
    /// Effects cannot be fully attributed by the lexical projection.
    OpaqueExecution,
    /// The lexical projection attributes no write target and no unresolved effect.
    ReadOnly,
    /// A test edit weakens or removes an existing assertion rather than adding coverage.
    AssertionWeakening,
    /// A read reaches conventional credential loci, or a recursive content search may reach them.
    SensitiveRead,
    /// A named git operation discards working-tree bytes that no retained recipe reproduces.
    WorkingStateDestruction,
}

/// A local signal for review, never a resource permission.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSignal {
    /// Closed effect vocabulary independent of Agent tool names.
    pub kind: ActionKind,
    /// Candidate resource when one is known; None denotes the complete call.
    pub resource_id: Option<String>,
}

/// Automatically collected context, frozen with the evaluation input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalContext {
    /// Exact candidate revision observed by the collector.
    pub candidate_revision: String,
    /// Collection instant in the ingress clock domain.
    pub captured_at_ms: u64,
    /// Exclusive validity limit under the original event deadline.
    pub valid_until_ms: u64,
    /// No more than one observation per candidate resource.
    pub files: Vec<FileObservation>,
    /// Bounded action signals, including incomplete projection.
    pub signals: Vec<ActionSignal>,
    /// Stable coverage codes; missing facts are never silently invented.
    pub gaps: Vec<String>,
}

impl LocalContext {
    /// Validate structure and binding without promoting assertions to authentication.
    pub fn validate(&self, candidate: &Candidate, revision: &str) -> Result<(), ContractError> {
        if self.candidate_revision != revision {
            return Err(ContractError::BindingMismatch);
        }
        if self.captured_at_ms >= self.valid_until_ms
            || self.files.len() > candidate.changes.len()
            || self.signals.len() > 64
            || self.gaps.len() > 32
        {
            return Err(ContractError::InvalidInput);
        }
        let contains = |id: &str| candidate.changes.iter().any(|c| c.resource_id == id);
        for (index, file) in self.files.iter().enumerate() {
            if !contains(&file.resource_id)
                || self.files[..index]
                    .iter()
                    .any(|f| f.resource_id == file.resource_id)
                || (file.delta != ContentDelta::Unknown && file.state != FileState::Regular)
                || file.sha256.as_ref().is_some_and(|digest| {
                    file.state != FileState::Regular
                        || digest.len() != 71
                        || !digest.starts_with("sha256:")
                        || !digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
                })
                || (file.delta != ContentDelta::Unknown && file.sha256.is_none())
            {
                return Err(ContractError::InvalidInput);
            }
        }
        for signal in &self.signals {
            if signal.resource_id.as_ref().is_some_and(|id| !contains(id)) {
                return Err(ContractError::InvalidInput);
            }
        }
        for gap in &self.gaps {
            crate::validation::id(gap)?;
        }
        Ok(())
    }

    /// Fresh observations only; expired data may inform a gap, never a local decision.
    pub fn fresh(&self, now_ms: u64) -> bool {
        self.captured_at_ms <= now_ms && now_ms < self.valid_until_ms
    }
}

/// Where a unit's result came from, distinct from whether HTTP succeeded.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRoute {
    /// A deterministic authorization or integrity constraint prevented evaluation.
    HardGate,
    /// A local fact and protection policy settled this unit.
    LocalPolicy,
    /// Semantic uncertainty was delegated to the configured backend.
    ModelFallback,
    /// Collected context expired before publication; no stale decision was adopted.
    ObservationExpired,
    /// The original deadline prevented further work.
    BudgetExhausted,
}

/// A deterministic policy decision with a stable, inspectable rationale.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDecision {
    /// Policy identity; neither filenames nor case labels select policies.
    pub policy_id: String,
    /// Typed unit answer composed by the ordinary outcome pipeline.
    pub answer: UnitAnswer,
}

/// Semantic absence is eligible for fallback; invalid provenance and binding are not.
pub fn can_fallback(base: &UnitAnswer) -> bool {
    matches!(
        base.reason,
        Reason::Allowed | Reason::MissingEvidence | Reason::EvidenceGap | Reason::Undetermined
    )
}

/// Supply the semantic unit with an undecided baseline, retaining evidence references.
pub(crate) fn semantic_baseline(base: &UnitAnswer) -> UnitAnswer {
    UnitAnswer {
        value: AnswerValue::Allow,
        reason: Reason::Allowed,
        confidence_ppm: 0,
        ..base.clone()
    }
}

/// Apply protection rules without filesystem access, network access or model inference.
///
/// Snapshot and preservation settle on effect facts that bound their own question:
/// loss magnitude for snapshot, mutation of retained bytes for preservation. Task
/// scope asks whether a call serves the task goal, which no effect fact answers, so
/// it always stays with the semantic backend.
pub fn decide(
    base: &UnitAnswer,
    candidate: &Candidate,
    evidence: &[Evidence],
    context: Option<&LocalContext>,
    now_ms: u64,
) -> Option<LocalDecision> {
    if !can_fallback(base) {
        return None;
    }
    let normalized = semantic_baseline(base);
    let context = context.filter(|c| c.fresh(now_ms));
    match base.unit {
        Scenario::Snapshot => decide_snapshot(&normalized, candidate, evidence, context),
        Scenario::Preservation => decide_preservation(&normalized, candidate, context),
        // Scope asks whether a call serves the task goal, which no lexical fact
        // settles: an unrelated or unauthorized read is out of scope even with
        // no write effect and no credential locus. Reads stay semantic; the
        // read_only/sensitive_read signals are hints for the model, not a gate.
        Scenario::TaskScope => None,
    }
}

/// A fully attributed call that names no write target and mutates nothing.
///
/// `ReadOnly` is emitted only when the lexical projection is complete, so the
/// absence of `OpaqueExecution` makes the two signals a joint completeness fact.
fn read_only_fact(candidate: &Candidate, context: Option<&LocalContext>) -> bool {
    candidate.changes.is_empty()
        && context.is_some_and(|context| {
            context
                .signals
                .iter()
                .any(|s| s.kind == ActionKind::ReadOnly)
                && !context
                    .signals
                    .iter()
                    .any(|s| s.kind == ActionKind::OpaqueExecution)
        })
}

fn has_signal(context: &LocalContext, kind: ActionKind) -> bool {
    context.signals.iter().any(|signal| signal.kind == kind)
}

fn allow(policy_id: &str, normalized: &UnitAnswer) -> LocalDecision {
    LocalDecision {
        policy_id: policy_id.into(),
        answer: UnitAnswer {
            confidence_ppm: 1_000_000,
            ..normalized.clone()
        },
    }
}

fn needs_snapshot(policy_id: &str, normalized: &UnitAnswer) -> LocalDecision {
    LocalDecision {
        policy_id: policy_id.into(),
        answer: UnitAnswer {
            value: AnswerValue::NeedsSnapshot,
            reason: Reason::NeedsSnapshot,
            confidence_ppm: 1_000_000,
            ..normalized.clone()
        },
    }
}

/// Whether every changed resource has a referenced identical rebuild fact and a
/// fresh observation, so a named destructive operation reproduces retained bytes.
fn all_changes_identical(
    base: &UnitAnswer,
    candidate: &Candidate,
    evidence: &[Evidence],
    context: &LocalContext,
) -> bool {
    !candidate.changes.is_empty()
        && candidate.changes.iter().all(|change| {
            context
                .files
                .iter()
                .any(|file| file.resource_id == change.resource_id)
                && evidence.iter().any(|item| {
                    base.evidence_refs.contains(&item.id)
                        && matches!(
                            &item.fact,
                            crate::contract::EvidenceFact::Rebuild { resource_id, result: crate::contract::RebuildResult::Identical }
                                if resource_id == &change.resource_id
                        )
                })
        })
}

fn decide_snapshot(
    normalized: &UnitAnswer,
    candidate: &Candidate,
    evidence: &[Evidence],
    context: Option<&LocalContext>,
) -> Option<LocalDecision> {
    if read_only_fact(candidate, context) {
        return Some(allow("no_loss_read_only", normalized));
    }
    if context.is_some_and(|context| {
        context.files.len() == candidate.changes.len()
            && !context.files.is_empty()
            && !has_signal(context, ActionKind::OpaqueExecution)
            && context
                .files
                .iter()
                .all(|f| f.state == FileState::Absent || f.delta == ContentDelta::Unchanged)
    }) {
        return Some(allow("no_current_bytes_lost", normalized));
    }
    if let Some(answer) = judgment::snapshot_floor(normalized, evidence) {
        return Some(LocalDecision {
            policy_id: "protect_unique_bytes".into(),
            answer,
        });
    }
    let context = context?;
    let preexisting = context.files.iter().any(|f| {
        f.existed_at_start == Some(true)
            && f.state == FileState::Regular
            && f.sha256.is_some()
            && f.delta != ContentDelta::Unchanged
            && candidate.changes.iter().any(|c| c.resource_id == f.resource_id)
            && !evidence.iter().any(|e| {
                normalized.evidence_refs.contains(&e.id)
                    && matches!(&e.fact, crate::contract::EvidenceFact::Rebuild { resource_id, result: crate::contract::RebuildResult::Identical } if resource_id == &f.resource_id)
            })
    });
    if preexisting {
        return Some(needs_snapshot("protect_preexisting_bytes", normalized));
    }
    // A named working-state destruction warrants a recovery point unless every
    // target it names is provably reproducible; unattributed remainders (whole
    // tree resets, cleans) always warrant one because their reach is unknown.
    if has_signal(context, ActionKind::WorkingStateDestruction)
        && !all_changes_identical(normalized, candidate, evidence, context)
    {
        return Some(needs_snapshot("protect_working_state", normalized));
    }
    None
}

fn decide_preservation(
    normalized: &UnitAnswer,
    candidate: &Candidate,
    context: Option<&LocalContext>,
) -> Option<LocalDecision> {
    if read_only_fact(candidate, context) {
        return Some(allow("preserve_no_mutation", normalized));
    }
    // Creating or removing a resource with no current bytes cannot violate a
    // retention requirement on existing bytes; declared retention lists remain
    // hard gates applied before this policy.
    if context.is_some_and(|context| {
        context.files.len() == candidate.changes.len()
            && !context.files.is_empty()
            && !has_signal(context, ActionKind::OpaqueExecution)
            && context.files.iter().all(|f| f.state == FileState::Absent)
    }) {
        return Some(allow("preserve_absent_target", normalized));
    }
    None
}
