//! Error types for observability record validation.

use thiserror::Error;

/// Message emitted by v1 when the `hook` discriminator is unknown.
pub const UNKNOWN_HOOK_ERROR: &str = "unknown observability hook";
/// Message emitted by v1 when no allowed metric was supplied.
pub const EMPTY_METRICS_ERROR: &str = "metrics must include at least one allowed metric";
/// Message emitted by v1 when `observedAt` carries no UTC offset.
pub const NAIVE_TIMESTAMP_ERROR: &str = "observedAt must be timezone-aware";

/// Validation failures for one observability record payload.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObservabilityError {
    /// The `hook` value is not one of the six supported hooks.
    ///
    /// Wording mirrors v1 `_validate_known_hook`.
    #[error("{UNKNOWN_HOOK_ERROR} '{hook}'; expected one of [{expected}]")]
    UnknownHook {
        /// The rejected hook name.
        hook: String,
        /// Comma-separated, sorted list of accepted hook names.
        expected: String,
    },

    /// The metrics payload contained no allowed metric name.
    #[error("{EMPTY_METRICS_ERROR}")]
    EmptyMetrics,

    /// `observedAt` had no timezone offset.
    #[error("{NAIVE_TIMESTAMP_ERROR}")]
    NaiveTimestamp,

    /// `observedAt` was not a parseable timestamp at all.
    #[error("observedAt is not a valid timestamp: {value}")]
    InvalidTimestamp {
        /// The rejected raw value.
        value: String,
    },

    /// A required metadata field was absent for this hook.
    #[error("metadata.{field} is required for hook '{hook}'")]
    MissingMetadata {
        /// Wire (camelCase) name of the missing field.
        field: &'static str,
        /// Hook that requires the field.
        hook: String,
    },

    /// A payload member had the wrong JSON type.
    ///
    /// Wording mirrors v1 `_ensure_mapping`.
    #[error("{name} must be an object")]
    NotAnObject {
        /// The offending member name.
        name: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_hook_message_matches_v1() {
        let err = ObservabilityError::UnknownHook {
            hook: "nope".to_owned(),
            expected: "'after_agent_run', 'before_agent_run'".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "unknown observability hook 'nope'; \
             expected one of ['after_agent_run', 'before_agent_run']"
        );
    }

    #[test]
    fn fixed_messages_match_v1_constants() {
        assert_eq!(
            ObservabilityError::EmptyMetrics.to_string(),
            "metrics must include at least one allowed metric"
        );
        assert_eq!(
            ObservabilityError::NaiveTimestamp.to_string(),
            "observedAt must be timezone-aware"
        );
        assert_eq!(
            ObservabilityError::NotAnObject { name: "metrics" }.to_string(),
            "metrics must be an object"
        );
    }
}
