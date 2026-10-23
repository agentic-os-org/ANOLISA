//! Recommendations and receipts; neither requesting nor approving proves application.

use serde::{Deserialize, Serialize};

use crate::contract::{AnswerValue, Binding, Reason, UnitAnswer};

/// Candidate restriction order; a later allowance cannot erase an earlier block.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateControl {
    /// No judgment was formed; gaps still require audit.
    Nop,
    /// Completed judgment with no objection, not an authorization grant.
    Allow,
    /// Hold the candidate pending separately authorized approval.
    Ask,
    /// Prevent the candidate through the consumer's native blocking interface.
    Block,
}

/// Subject actions require independent identity, scope and authorization checks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectAction {
    /// Suspend while preserving state.
    Freeze,
    /// Restrict reachability without assuming termination.
    Quarantine,
    /// Irreversible termination, requiring human confirmation.
    Kill,
}

/// One requested subject action; no implicit state absorption is performed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectDirective {
    /// Unique directive identity within one outcome.
    pub id: String,
    /// Bound stable process/session instance, never a bare PID.
    pub instance_id: String,
    /// Requested action, not confirmed state.
    pub action: SubjectAction,
    /// Consumer-resolved scope identifiers.
    pub scope_ids: Vec<String>,
}

/// Closed side-effect vocabulary containing identifiers rather than executable text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ObligationAction {
    /// Persist sanitized judgment and execution metadata.
    Audit {},
    /// Create a recovery point before candidate execution.
    Snapshot {
        /// Bound workspace identity.
        workspace_id: String,
    },
    /// Suggest recovery, requiring quiescence and a fresh explicit confirmation.
    Rollback {
        /// Bound workspace identity.
        workspace_id: String,
        /// Authorized recovery point.
        checkpoint_id: String,
    },
    /// Reclaim only explicitly authorized, no-longer-required recovery points.
    Cleanup {
        /// Bound workspace identity.
        workspace_id: String,
        /// Consumer-verified reclaimable checkpoints.
        checkpoint_ids: Vec<String>,
    },
    /// Notify an independently authorized recipient.
    Notify {
        /// Registered recipient identity, not an arbitrary URL.
        recipient_id: String,
    },
    /// Invoke a registered bounded diagnostic, not a generated command.
    Diagnose {
        /// Registered diagnostic identity.
        diagnostic_id: String,
    },
    /// Execute an authorized skill query.
    RetrieveSkill {
        /// Registered query identity.
        query_id: String,
    },
    /// Execute an authorized memory query.
    RetrieveMemory {
        /// Registered query identity.
        query_id: String,
    },
    /// Suggest injection through a separately admitted native channel.
    InjectContext {
        /// Reviewed material references.
        material_ids: Vec<String>,
    },
}

/// Failure policy cannot be downgraded for protection obligations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailurePolicy {
    /// Unconfirmed required action prevents dependent candidate execution.
    Closed,
    /// Failed enhancement remains visible without adding a candidate restriction.
    Open,
}

/// An ordered, independently receipted side-effect recommendation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
    /// Unique identity within the outcome.
    pub id: String,
    /// Escalates enhancements to protection failure semantics, never downgrades.
    pub critical: bool,
    /// Requested registered action and controlled references.
    pub action: ObligationAction,
}

impl Obligation {
    /// Resolve protection semantics independently of any configurable flag.
    pub fn failure_policy(&self) -> FailurePolicy {
        if self.critical
            || matches!(
                self.action,
                ObligationAction::Audit {}
                    | ObligationAction::Snapshot { .. }
                    | ObligationAction::Rollback { .. }
                    | ObligationAction::Cleanup { .. }
            )
        {
            FailurePolicy::Closed
        } else {
            FailurePolicy::Open
        }
    }
}

/// Candidate and subject restrictions have separate lifetimes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    /// One-shot restriction on the bound candidate.
    pub candidate: CandidateControl,
    /// Independent persistent subject requests.
    pub subject: Vec<SubjectDirective>,
}

/// Complete recommendation; consumers must still admit and authorize execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Candidate/subject restrictions.
    pub control: Control,
    /// Ordered side effects; no executor runs in this component.
    pub obligations: Vec<Obligation>,
    /// Evidence used for the recommendation.
    pub evidence_refs: Vec<String>,
}

pub(crate) fn compose(
    answers: &[UnitAnswer],
    binding: &Binding,
    controls: impl IntoIterator<Item = CandidateControl>,
) -> Outcome {
    let candidate = controls.into_iter().max().unwrap_or(CandidateControl::Nop);
    let mut obligations = Vec::new();
    if candidate != CandidateControl::Block
        && answers
            .iter()
            .any(|answer| answer.value == AnswerValue::NeedsSnapshot)
    {
        obligations.push(Obligation {
            id: "snapshot".into(),
            critical: true,
            action: ObligationAction::Snapshot {
                workspace_id: binding.workspace_id.clone(),
            },
        });
    }
    obligations.push(Obligation {
        id: "audit".into(),
        critical: true,
        action: ObligationAction::Audit {},
    });
    let mut evidence_refs: Vec<_> = answers
        .iter()
        .flat_map(|answer| answer.evidence_refs.iter().cloned())
        .collect();
    evidence_refs.sort();
    evidence_refs.dedup();
    Outcome {
        control: Control {
            candidate,
            subject: vec![],
        },
        obligations,
        evidence_refs,
    }
}

/// Only asks, subject actions and obligations require an Executor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Directive {
    /// Hold and ask about the bound candidate; approval is not execution.
    Approval {
        /// Reserved outcome-local identity.
        id: String,
        /// Candidate awaiting approval.
        candidate_id: String,
    },
    /// Subject request with independent receipt.
    Subject(SubjectDirective),
    /// Side effect with independent receipt.
    Obligation(Obligation),
}

impl Directive {
    /// Outcome-local identity used for immutable request/receipt correlation.
    pub fn id(&self) -> &str {
        match self {
            Self::Approval { id, .. } => id,
            Self::Subject(item) => &item.id,
            Self::Obligation(item) => &item.id,
        }
    }
}

impl Outcome {
    /// Project recommendations without executing, merging or absorbing requests.
    pub fn directives(&self, binding: &Binding) -> Vec<Directive> {
        let mut directives = Vec::new();
        if self.control.candidate == CandidateControl::Ask {
            directives.push(Directive::Approval {
                id: "candidate-approval".into(),
                candidate_id: binding.candidate_id.clone(),
            });
        }
        directives.extend(self.control.subject.iter().cloned().map(Directive::Subject));
        directives.extend(self.obligations.iter().cloned().map(Directive::Obligation));
        directives
    }
}

/// Confirmed execution state; an unknown receipt never authorizes automatic retry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    /// Target action was verified as effective.
    Applied,
    /// Target action was verified not to have taken effect.
    Rejected,
    /// Effect cannot be confirmed; query the original receipt before acting.
    Unknown,
}

/// Consumer-produced execution evidence, not produced by the check evaluator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Exact protocol version.
    pub api_version: String,
    /// Request identity retained by the consumer.
    pub request_id: String,
    /// Original immutable context binding.
    pub binding: Binding,
    /// Identity of the immutable requested directive.
    pub directive_id: String,
    /// Verified status, independent of transport success.
    pub state: ReceiptState,
    /// Stable, non-sensitive diagnostic code supplied by the consumer.
    pub reason: ReceiptReason,
}

/// Receipt reasons distinguish request acceptance from verified effects.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptReason {
    /// Independently verified target effect.
    Confirmed,
    /// Authorization or capability admission refused the request.
    Denied,
    /// Dispatch may have happened, but its result is unavailable.
    Unconfirmed,
}

/// Select the reference failure answer without interpreting text as permission.
pub(crate) fn blocked_reason(reason: Reason) -> bool {
    !matches!(reason, Reason::Allowed | Reason::NeedsSnapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            session_id: "session".into(),
            instance_id: "instance".into(),
            workspace_id: "workspace".into(),
            task_version: "task".into(),
            config_revision: "config".into(),
            candidate_id: "candidate".into(),
            candidate_revision: "revision".into(),
        }
    }

    #[test]
    fn common_composition_accepts_every_candidate_control() {
        for candidate in [
            CandidateControl::Nop,
            CandidateControl::Allow,
            CandidateControl::Ask,
            CandidateControl::Block,
        ] {
            let outcome = compose(&[], &binding(), [candidate]);
            assert_eq!(outcome.control.candidate, candidate);
        }
    }
}
