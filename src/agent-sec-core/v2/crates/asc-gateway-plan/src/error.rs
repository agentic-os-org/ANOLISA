/// Reasons a plan is rejected.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// A Scope or Policy snapshot violates authoring semantics.
    #[error("invalid scope policy: {0}")]
    Policy(#[from] asc_policy_types::ValidationError),
    /// Cedar admission or exact HTTP lowering failed.
    #[error("invalid Cedar gateway policy: {0}")]
    Cedar(#[from] asc_policy_cedar::CedarError),
    /// A plan-level field violates the format contract.
    #[error("invalid plan {field}: {reason}")]
    Invalid {
        /// Offending field.
        field: &'static str,
        /// Violated constraint.
        reason: &'static str,
    },
    /// A rule violates the format contract.
    #[error("invalid plan rule {index} {field}: {reason}")]
    InvalidRule {
        /// Position of the rule in `rules`.
        index: usize,
        /// Offending field.
        field: &'static str,
        /// Violated constraint.
        reason: &'static str,
    },
}
