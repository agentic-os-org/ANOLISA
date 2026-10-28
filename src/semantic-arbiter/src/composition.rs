//! Deterministic composition of independently formed restrictions and obligations.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::contract::Binding;
use crate::outcome::{
    CandidateControl, Control, Directive, Obligation, ObligationAction, Outcome, SubjectAction,
    SubjectDirective,
};

/// Granular consumer capability required to admit one directive kind.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DirectiveCapability {
    /// Ask for a decision while the candidate remains held.
    Approval,
    /// Suspend a bound subject.
    Freeze,
    /// Restrict a bound subject.
    Quarantine,
    /// Terminate a bound subject.
    Kill,
    /// Persist sanitized decision metadata.
    Audit,
    /// Create a recovery point.
    Snapshot,
    /// Restore an authorized recovery point.
    Rollback,
    /// Reclaim authorized recovery points.
    Cleanup,
    /// Notify an authorized recipient.
    Notify,
    /// Run a registered diagnostic.
    Diagnose,
    /// Retrieve an authorized skill.
    RetrieveSkill,
    /// Retrieve authorized memory.
    RetrieveMemory,
    /// Inject reviewed context through a native channel.
    InjectContext,
}

/// Explicit capability set; declarations do not prove native support or authorization.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilityRegistry {
    capabilities: BTreeSet<DirectiveCapability>,
}

impl CapabilityRegistry {
    /// Build a registry from explicit granular capabilities.
    pub fn new(capabilities: impl IntoIterator<Item = DirectiveCapability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Check a declaration without promoting it to authorization.
    pub fn contains(&self, capability: DirectiveCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

impl DirectiveCapability {
    /// Resolve the exact capability needed for one immutable directive.
    pub fn for_directive(directive: &Directive) -> Self {
        match directive {
            Directive::Approval { .. } => Self::Approval,
            Directive::Subject(item) => match item.action {
                SubjectAction::Freeze => Self::Freeze,
                SubjectAction::Quarantine => Self::Quarantine,
                SubjectAction::Kill => Self::Kill,
            },
            Directive::Obligation(item) => match &item.action {
                ObligationAction::Audit {} => Self::Audit,
                ObligationAction::Snapshot { .. } => Self::Snapshot,
                ObligationAction::Rollback { .. } => Self::Rollback,
                ObligationAction::Cleanup { .. } => Self::Cleanup,
                ObligationAction::Notify { .. } => Self::Notify,
                ObligationAction::Diagnose { .. } => Self::Diagnose,
                ObligationAction::RetrieveSkill { .. } => Self::RetrieveSkill,
                ObligationAction::RetrieveMemory { .. } => Self::RetrieveMemory,
                ObligationAction::InjectContext { .. } => Self::InjectContext,
            },
        }
    }
}

/// One independently formed contribution to a combined outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutcomeContribution {
    /// Candidate restriction contributed by this judgment.
    pub candidate: CandidateControl,
    /// Persistent subject requests contributed by this judgment.
    pub subjects: Vec<SubjectDirective>,
    /// Independently receipted side-effect recommendations.
    pub obligations: Vec<Obligation>,
    /// Evidence adopted by this judgment.
    pub evidence_refs: Vec<String>,
}

impl OutcomeContribution {
    /// Create a contribution containing only a candidate restriction.
    pub fn candidate(candidate: CandidateControl) -> Self {
        Self {
            candidate,
            subjects: Vec::new(),
            obligations: Vec::new(),
            evidence_refs: Vec::new(),
        }
    }
}

/// Stable composition failures that require the caller to resolve the conflict.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CompositionError {
    /// A requested directive kind was not declared by the consumer.
    #[error("capability_unavailable")]
    CapabilityUnavailable,
    /// One directive identity referred to different immutable requests.
    #[error("directive_conflict")]
    DirectiveConflict,
    /// Subject identity or scope did not match the bound candidate context.
    #[error("binding_mismatch")]
    BindingMismatch,
    /// More than one audit request was supplied.
    #[error("duplicate_audit")]
    DuplicateAudit,
}

/// Compose restrictions without executing or authorizing any directive.
pub fn compose_outcome(
    binding: &Binding,
    capabilities: &CapabilityRegistry,
    contributions: impl IntoIterator<Item = OutcomeContribution>,
) -> Result<Outcome, CompositionError> {
    let mut candidate = CandidateControl::Nop;
    let mut subjects = BTreeMap::new();
    let mut obligations: BTreeMap<String, Obligation> = BTreeMap::new();
    let mut evidence_refs = BTreeSet::new();
    let mut contributed = false;

    for contribution in contributions {
        contributed = true;
        candidate = candidate.max(contribution.candidate);
        for subject in contribution.subjects {
            admit_subject(binding, capabilities, &subject)?;
            let key = subject_rank(subject.action);
            match subjects.get(&key) {
                Some(existing) if existing != &subject => {
                    return Err(CompositionError::DirectiveConflict);
                }
                Some(_) => {}
                None => {
                    subjects.insert(key, subject);
                }
            }
        }
        for obligation in contribution.obligations {
            admit_obligation(binding, capabilities, &obligation)?;
            match obligations.get(&obligation.id) {
                Some(existing) if existing != &obligation => {
                    return Err(CompositionError::DirectiveConflict);
                }
                Some(_) => {}
                None => {
                    obligations.insert(obligation.id.clone(), obligation);
                }
            }
        }
        evidence_refs.extend(contribution.evidence_refs);
    }

    if candidate == CandidateControl::Ask && !capabilities.contains(DirectiveCapability::Approval) {
        return Err(CompositionError::CapabilityUnavailable);
    }
    let audit_count = obligations
        .values()
        .filter(|item| matches!(item.action, ObligationAction::Audit {}))
        .count();
    if audit_count > 1 {
        return Err(CompositionError::DuplicateAudit);
    }
    if audit_count == 0 && contributed {
        if !capabilities.contains(DirectiveCapability::Audit) {
            return Err(CompositionError::CapabilityUnavailable);
        }
        if obligations.contains_key("audit") {
            return Err(CompositionError::DirectiveConflict);
        }
        obligations.insert(
            "audit".into(),
            Obligation {
                id: "audit".into(),
                critical: true,
                action: ObligationAction::Audit {},
            },
        );
    }

    let mut directive_ids = BTreeSet::new();
    if candidate == CandidateControl::Ask {
        directive_ids.insert("candidate-approval".to_string());
    }
    if subjects
        .values()
        .any(|item| !directive_ids.insert(item.id.clone()))
        || obligations
            .values()
            .any(|item| !directive_ids.insert(item.id.clone()))
    {
        return Err(CompositionError::DirectiveConflict);
    }

    let mut obligations: Vec<_> = obligations.into_values().collect();
    obligations.sort_by(|left, right| {
        obligation_rank(&left.action)
            .cmp(&obligation_rank(&right.action))
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(Outcome {
        control: Control {
            candidate,
            subject: subjects.into_values().collect(),
        },
        obligations,
        evidence_refs: evidence_refs.into_iter().collect(),
    })
}

fn admit_subject(
    binding: &Binding,
    capabilities: &CapabilityRegistry,
    subject: &SubjectDirective,
) -> Result<(), CompositionError> {
    if subject.id.is_empty()
        || subject.instance_id != binding.instance_id
        || subject.scope_ids.is_empty()
        || subject.scope_ids.iter().any(String::is_empty)
    {
        return Err(CompositionError::BindingMismatch);
    }
    let capability = match subject.action {
        SubjectAction::Freeze => DirectiveCapability::Freeze,
        SubjectAction::Quarantine => DirectiveCapability::Quarantine,
        SubjectAction::Kill => DirectiveCapability::Kill,
    };
    if !capabilities.contains(capability) {
        return Err(CompositionError::CapabilityUnavailable);
    }
    Ok(())
}

fn admit_obligation(
    binding: &Binding,
    capabilities: &CapabilityRegistry,
    obligation: &Obligation,
) -> Result<(), CompositionError> {
    let capability = match &obligation.action {
        ObligationAction::Audit {} => DirectiveCapability::Audit,
        ObligationAction::Snapshot { .. } => DirectiveCapability::Snapshot,
        ObligationAction::Rollback { .. } => DirectiveCapability::Rollback,
        ObligationAction::Cleanup { .. } => DirectiveCapability::Cleanup,
        ObligationAction::Notify { .. } => DirectiveCapability::Notify,
        ObligationAction::Diagnose { .. } => DirectiveCapability::Diagnose,
        ObligationAction::RetrieveSkill { .. } => DirectiveCapability::RetrieveSkill,
        ObligationAction::RetrieveMemory { .. } => DirectiveCapability::RetrieveMemory,
        ObligationAction::InjectContext { .. } => DirectiveCapability::InjectContext,
    };
    if !capabilities.contains(capability) {
        return Err(CompositionError::CapabilityUnavailable);
    }
    let valid = match &obligation.action {
        ObligationAction::Audit {} => true,
        ObligationAction::Snapshot { workspace_id } => workspace_id == &binding.workspace_id,
        ObligationAction::Rollback {
            workspace_id,
            checkpoint_id,
        } => workspace_id == &binding.workspace_id && !checkpoint_id.is_empty(),
        ObligationAction::Cleanup {
            workspace_id,
            checkpoint_ids,
        } => {
            workspace_id == &binding.workspace_id
                && !checkpoint_ids.is_empty()
                && checkpoint_ids.iter().all(|id| !id.is_empty())
        }
        ObligationAction::Notify { recipient_id } => !recipient_id.is_empty(),
        ObligationAction::Diagnose { diagnostic_id } => !diagnostic_id.is_empty(),
        ObligationAction::RetrieveSkill { query_id }
        | ObligationAction::RetrieveMemory { query_id } => !query_id.is_empty(),
        ObligationAction::InjectContext { material_ids } => {
            !material_ids.is_empty() && material_ids.iter().all(|id| !id.is_empty())
        }
    };
    if !valid || obligation.id.is_empty() {
        return Err(CompositionError::BindingMismatch);
    }
    Ok(())
}

fn subject_rank(action: SubjectAction) -> u8 {
    match action {
        SubjectAction::Freeze => 0,
        SubjectAction::Quarantine => 1,
        SubjectAction::Kill => 2,
    }
}

fn obligation_rank(action: &ObligationAction) -> u8 {
    match action {
        ObligationAction::Snapshot { .. } => 0,
        ObligationAction::Rollback { .. } => 1,
        ObligationAction::Cleanup { .. } => 2,
        ObligationAction::Notify { .. } => 3,
        ObligationAction::Diagnose { .. } => 4,
        ObligationAction::RetrieveSkill { .. } => 5,
        ObligationAction::RetrieveMemory { .. } => 6,
        ObligationAction::InjectContext { .. } => 7,
        ObligationAction::Audit {} => 8,
    }
}
