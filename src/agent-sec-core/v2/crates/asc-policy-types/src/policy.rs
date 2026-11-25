//! Revisioned authored Policy snapshots retained by Scope and Binding.

use std::collections::{BTreeMap, BTreeSet};

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
    /// Present only on a daemon-derived assignment. PAP retains the unmodified source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentSelection>,
    /// Server-resolved pair identities, pinned independently of mutable source files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_bindings: Vec<CredentialBinding>,
}

/// Policy read response with deployment outcomes observed for this source revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDetails {
    /// Original PAP source and its revision.
    #[serde(flatten)]
    pub policy: PreparedPolicy,
    /// Current automatic planning outcomes; actual process enforcement is reported by Bindings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automatic_diagnostics: Vec<AutomaticDiagnostic>,
}

/// An automatic assignment or the reason a backend could not plan its statements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomaticDiagnostic {
    /// Policy that owns the statements.
    pub policy_id: ResourceId,
    /// Revision for which the planner produced this result.
    pub revision: Revision,
    /// Statements affected by this planning decision.
    pub statement_ids: BTreeSet<String>,
    /// Deployment backend that evaluated the candidate.
    pub backend: DeploymentBackend,
    /// Scope to inspect for process Bindings, when a candidate exists.
    pub scope_id: Option<ResourceId>,
    /// Assignment state, independent of each process's actual installation state.
    pub state: String,
    /// Planner or admission detail, when available.
    pub reason: Option<String>,
}

/// Statements from this exact source revision assigned to a single process Scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentSelection {
    /// Adapter whose supported rules were selected.
    pub backend: DeploymentBackend,
    /// Cedar parser IDs within the original source, independently of credential annotations.
    pub statement_ids: BTreeSet<String>,
    /// Complete selected Cedar text for partial statements, keyed by their original ID.
    /// An absent entry consumes the whole statement from this pinned revision.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fragments: BTreeMap<String, String>,
}

/// Backends that install rules on discovered processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeploymentBackend {
    /// File enforcement on the process tree.
    ActPlane,
    /// HTTP credential injection.
    Gateway,
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
        match &self.deployment {
            Some(selection) => selection.backend == DeploymentBackend::Gateway,
            None => !self.credential_bindings.is_empty() || self.template.uses_credentials(),
        }
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
