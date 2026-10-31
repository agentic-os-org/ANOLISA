//! Revisioned authored Policy snapshots retained by Scope and Binding.

use serde::{Deserialize, Serialize};

use crate::authoring::PolicySource;
use crate::error::{Validate, ValidationError};
use crate::identifiers::{ResourceId, Revision};

/// Complete authored Policy revision; target Adapters compile its template on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedPolicy {
    /// Stable product policy identity.
    pub policy_id: ResourceId,
    /// Human-readable policy name; it is not unique.
    pub policy_name: String,
    /// Immutable revision.
    pub revision: Revision,
    /// Product authoring input.
    #[serde(with = "crate::authoring::policy_template")]
    pub template: PolicySource,
}

impl Validate for PreparedPolicy {
    fn validate(&self) -> Result<(), ValidationError> {
        if validate_policy_name(&self.policy_name).is_err() {
            return Err(ValidationError::new(
                "policyName",
                "must contain a visible, control-free value of at most 256 bytes",
            ));
        }
        let result = match &self.template {
            PolicySource::SecCore(template) => template.validate(),
            PolicySource::Cedar(_) => self.template.validate(),
        };
        result.map_err(|error| {
            ValidationError::new(format!("template.{}", error.path), error.message)
        })
    }
}

/// Shared name rules for authored input and complete Policy snapshots.
/// Callers retain their own error category and path projection.
///
/// # Errors
/// Returns a stable reason for an empty, oversized, or control-bearing name.
pub fn validate_policy_name(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err("must contain a visible character");
    }
    if value.len() > 256 {
        return Err("must not exceed 256 bytes");
    }
    if value.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    Ok(())
}
