/// Reasons a plan is rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    /// The JSON document does not decode into a plan.
    #[error("plan does not decode: {0}")]
    Decode(String),
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
