//! Pure execution contracts shared by lifecycle application services.
//!
//! These types separate plan-only requests from apply-ready work while keeping
//! the planner's existing step vocabulary as the single source of truth.

use crate::planner::{NoOpReason, Plan, PlanNote, Step};

/// Whether a planned lifecycle operation is previewed or applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionIntent {
    /// Return the planned work without performing side effects.
    Plan,
    /// Make the planned work available to an effect executor.
    Apply,
}

impl ExecutionIntent {
    /// Classifies planner output for this execution intent.
    pub fn prepare(self, plan: Plan) -> PreparedExecution {
        match plan {
            Plan::NoOp { reason } => PreparedExecution::NoOp { reason },
            Plan::Execute { steps, notes } => match self {
                Self::Plan => PreparedExecution::Preview { steps, notes },
                Self::Apply => PreparedExecution::Apply { steps, notes },
            },
        }
    }
}

/// Planner output classified for preview or application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedExecution {
    /// Facts already satisfy the requested lifecycle intent.
    NoOp {
        /// Why no work is required.
        reason: NoOpReason,
    },
    /// Ordered work exposed only for rendering a plan-only request.
    Preview {
        /// Planner steps in execution order.
        steps: Vec<Step>,
        /// Non-fatal findings attached by the planner.
        notes: Vec<PlanNote>,
    },
    /// Ordered work authorized for an effect executor.
    Apply {
        /// Planner steps in execution order.
        steps: Vec<Step>,
        /// Non-fatal findings attached by the planner.
        notes: Vec<PlanNote>,
    },
}

/// Terminal classification of an applied command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcomeStatus {
    /// The requested work completed without a terminal failure.
    Completed,
    /// Some effects completed, but the operation needs reconciliation.
    Partial {
        /// Why the applied work requires reconciliation.
        reason: String,
    },
    /// The operation failed without reporting partial completion.
    Failed {
        /// Why the operation could not complete.
        reason: String,
    },
}

/// Typed terminal result returned by a lifecycle application service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome<C> {
    status: CommandOutcomeStatus,
    operation_id: Option<String>,
    changes: Vec<C>,
    warnings: Vec<String>,
}

impl<C> CommandOutcome<C> {
    /// Creates a terminal outcome from domain-specific changes and warnings.
    pub fn new(
        status: CommandOutcomeStatus,
        operation_id: Option<String>,
        changes: Vec<C>,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            status,
            operation_id,
            changes,
            warnings,
        }
    }

    /// Returns the terminal classification.
    pub fn status(&self) -> &CommandOutcomeStatus {
        &self.status
    }

    /// Returns the durable operation identifier when one was created.
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    /// Returns the domain-specific objects changed before termination.
    pub fn changes(&self) -> &[C] {
        &self.changes
    }

    /// Returns non-fatal diagnostics accumulated during the operation.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable_plan() -> Plan {
        Plan::Execute {
            steps: vec![Step::DownloadVerify],
            notes: vec![PlanNote::PackageAlreadyAbsent],
        }
    }

    #[test]
    fn plan_intent_preserves_steps_and_notes_for_preview() {
        assert_eq!(
            ExecutionIntent::Plan.prepare(executable_plan()),
            PreparedExecution::Preview {
                steps: vec![Step::DownloadVerify],
                notes: vec![PlanNote::PackageAlreadyAbsent],
            }
        );
    }

    #[test]
    fn apply_intent_preserves_steps_and_notes_for_execution() {
        assert_eq!(
            ExecutionIntent::Apply.prepare(executable_plan()),
            PreparedExecution::Apply {
                steps: vec![Step::DownloadVerify],
                notes: vec![PlanNote::PackageAlreadyAbsent],
            }
        );
    }

    #[test]
    fn no_op_never_becomes_apply_ready() {
        for intent in [ExecutionIntent::Plan, ExecutionIntent::Apply] {
            assert_eq!(
                intent.prepare(Plan::NoOp {
                    reason: NoOpReason::AlreadyAdopted,
                }),
                PreparedExecution::NoOp {
                    reason: NoOpReason::AlreadyAdopted,
                }
            );
        }
    }

    #[test]
    fn completed_outcome_preserves_non_terminal_evidence() {
        let outcome = CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some("op-adopt-1".to_string()),
            vec!["tokenless"],
            vec!["service state needs verification".to_string()],
        );

        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(outcome.operation_id(), Some("op-adopt-1"));
        assert_eq!(outcome.changes(), &["tokenless"]);
        assert_eq!(
            outcome.warnings(),
            &["service state needs verification".to_string()]
        );
    }

    #[test]
    fn partial_outcome_separates_failure_from_warnings() {
        let outcome = CommandOutcome::new(
            CommandOutcomeStatus::Partial {
                reason: "manifest reconciliation failed".to_string(),
            },
            Some("op-update-1".to_string()),
            vec!["tokenless"],
            vec!["service state needs verification".to_string()],
        );

        assert_eq!(
            outcome.status(),
            &CommandOutcomeStatus::Partial {
                reason: "manifest reconciliation failed".to_string(),
            }
        );
        assert_eq!(outcome.operation_id(), Some("op-update-1"));
        assert_eq!(outcome.changes(), &["tokenless"]);
        assert_eq!(
            outcome.warnings(),
            &["service state needs verification".to_string()]
        );
    }

    #[test]
    fn failed_outcome_requires_a_terminal_reason() {
        let outcome = CommandOutcome::<&str>::new(
            CommandOutcomeStatus::Failed {
                reason: "package transaction failed".to_string(),
            },
            None,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            outcome.status(),
            &CommandOutcomeStatus::Failed {
                reason: "package transaction failed".to_string(),
            }
        );
        assert_eq!(outcome.operation_id(), None);
        assert!(outcome.changes().is_empty());
        assert!(outcome.warnings().is_empty());
    }
}
