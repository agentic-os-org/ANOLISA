//! Shared semantic validation support.

/// A stable, path-addressed semantic validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct ValidationError {
    /// JSON-style path to the invalid field.
    pub path: String,
    /// Human-readable failure detail.
    pub message: String,
}

impl ValidationError {
    /// Creates a validation error for `path`.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Semantic validation performed after serde has checked the wire shape.
pub trait Validate {
    /// Validates cross-field and value-level invariants.
    ///
    /// # Errors
    /// Returns the first stable, path-addressed validation failure.
    fn validate(&self) -> Result<(), ValidationError>;
}
