//! Immutable Canonical Policy IR envelope.

use serde::{Deserialize, Serialize};

use crate::authoring::PolicyTemplate;
use crate::error::{Validate, ValidationError};
use crate::identifiers::{PolicyId, ProfileId, ResourceId, Revision};
use crate::ir::CanonicalPolicyIr;
use crate::profile::{IR_SCHEMA_VERSION_V1, PROFILE_V1ALPHA1_DEMO1};

/// Immutable backend-independent Policy revision produced by PAP lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyEnvelope {
    /// Canonical IR envelope schema version.
    pub ir_schema_version: u16,
    /// Immutable semantic profile implemented by the payload.
    pub profile_id: ProfileId,
    /// Stable policy identity.
    pub policy_id: PolicyId,
    /// Immutable policy revision.
    pub revision: Revision,
    /// Backend-independent security semantics.
    pub payload: CanonicalPolicyIr,
}

impl Validate for PolicyEnvelope {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.ir_schema_version != IR_SCHEMA_VERSION_V1 {
            return Err(ValidationError::new(
                "irSchemaVersion",
                format!("unsupported IR schema version {}", self.ir_schema_version),
            ));
        }
        if self.profile_id.as_str() != PROFILE_V1ALPHA1_DEMO1 {
            return Err(ValidationError::new(
                "profileId",
                "unsupported Canonical Policy IR profile",
            ));
        }
        self.payload
            .validate()
            .map_err(|error| ValidationError::new(format!("payload.{}", error.path), error.message))
    }
}

/// Durable Policy revision with its authored and deterministic lowered forms.
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
    pub template: PolicyTemplate,
    /// Backend-independent lowered policy.
    pub canonical_policy: PolicyEnvelope,
}

impl Validate for PreparedPolicy {
    fn validate(&self) -> Result<(), ValidationError> {
        if validate_policy_name(&self.policy_name).is_err() {
            return Err(ValidationError::new(
                "policyName",
                "must contain a visible, control-free value of at most 256 bytes",
            ));
        }
        if self.policy_id.as_str() != self.canonical_policy.policy_id.as_str() {
            return Err(ValidationError::new(
                "canonicalPolicy.policyId",
                "must match the prepared Policy identity",
            ));
        }
        if self.revision != self.canonical_policy.revision {
            return Err(ValidationError::new(
                "canonicalPolicy.revision",
                "must match the prepared Policy revision",
            ));
        }
        self.canonical_policy.validate().map_err(|error| {
            ValidationError::new(format!("canonicalPolicy.{}", error.path), error.message)
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
