//! Revisioned authored Policy snapshots retained by Scope and Binding.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::authoring::{PolicySource, source_serde};
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
    /// Cedar schema and syntax checks belong to frontend admission/loading.
    #[serde(with = "source_serde")]
    pub template: PolicySource,
    /// Server-resolved pair identities, pinned independently of mutable source files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_bindings: Vec<CredentialBinding>,
}

/// A validated source rule's server-owned credential association.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialBinding {
    /// Cedar rule identity within this immutable source.
    pub rule_id: String,
    /// Opaque identity of a registered real/fake pair; never a secret or its digest.
    pub pair_id: String,
}

impl PreparedPolicy {
    /// Whether this snapshot requires credential execution and root administration.
    #[must_use]
    pub fn uses_credentials(&self) -> bool {
        !self.credential_bindings.is_empty() || self.template.uses_credentials()
    }
}

impl Validate for PreparedPolicy {
    fn validate(&self) -> Result<(), ValidationError> {
        if validate_policy_name(&self.policy_name).is_err() {
            return Err(ValidationError::new(
                "policyName",
                "must contain a visible, control-free value of at most 256 bytes",
            ));
        }
        if !self.credential_bindings.is_empty() && !matches!(self.template, PolicySource::Cedar(_))
        {
            return Err(ValidationError::new(
                "credentialBindings",
                "requires Cedar source",
            ));
        }
        if self.credential_bindings.len() > 256 {
            return Err(ValidationError::new(
                "credentialBindings",
                "exceeds 256 rules",
            ));
        }
        let mut rules = BTreeSet::new();
        for binding in &self.credential_bindings {
            ResourceId::new(&binding.rule_id).map_err(|_| {
                ValidationError::new("credentialBindings.ruleId", "invalid rule identity")
            })?;
            ResourceId::new(&binding.pair_id).map_err(|_| {
                ValidationError::new("credentialBindings.pairId", "invalid pair identity")
            })?;
            if !rules.insert(&binding.rule_id) {
                return Err(ValidationError::new(
                    "credentialBindings",
                    "duplicate rule identity",
                ));
            }
        }
        let validation = match &self.template {
            PolicySource::SecCore(template) => template.validate(),
            source @ PolicySource::Cedar(_) => source.validate(),
        };
        validation.map_err(|error| {
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
