//! Typed compare-and-replace contract for policy ownership handoff.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    DestinationScope, PolicyMode, PolicyValidationError,
};

const CREDENTIAL_POLICY_SNAPSHOT_VERSION: u32 = 1;

/// Versioned immutable product-policy provenance persisted with a handoff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CredentialPolicySnapshot {
    /// Snapshot schema understood by the reader.
    pub version: u32,
    /// Complete normalized product policy, including non-DSL semantics.
    pub policy: CredentialExfiltrationPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialPolicySnapshotWire {
    version: u32,
    policy: StrictCredentialPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictCredentialPolicy {
    policy_id: String,
    revision: u64,
    source_patterns: Vec<String>,
    trusted_endpoints: Vec<String>,
    taint_label: String,
    taint_ttl_secs: u64,
    destination_scope: DestinationScope,
    mode: PolicyMode,
}

impl<'de> Deserialize<'de> for CredentialPolicySnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CredentialPolicySnapshotWire::deserialize(deserializer)?;
        Ok(Self {
            version: wire.version,
            policy: CredentialExfiltrationPolicy {
                policy_id: wire.policy.policy_id,
                revision: wire.policy.revision,
                source_patterns: wire.policy.source_patterns,
                trusted_endpoints: wire.policy.trusted_endpoints,
                taint_label: wire.policy.taint_label,
                taint_ttl_secs: wire.policy.taint_ttl_secs,
                destination_scope: wire.policy.destination_scope,
                mode: wire.policy.mode,
            },
        })
    }
}

impl CredentialPolicySnapshot {
    /// Captures one validated policy using the current snapshot schema.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error when the policy is not safe to persist.
    pub fn capture(
        policy: CredentialExfiltrationPolicy,
    ) -> Result<Self, CredentialPolicySnapshotError> {
        policy.validate()?;
        Ok(Self {
            version: CREDENTIAL_POLICY_SNAPSHOT_VERSION,
            policy,
        })
    }

    /// Returns the policy only when both schema and policy remain valid.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unsupported schema or invalid persisted policy.
    pub fn policy(&self) -> Result<&CredentialExfiltrationPolicy, CredentialPolicySnapshotError> {
        if self.version != CREDENTIAL_POLICY_SNAPSHOT_VERSION {
            return Err(CredentialPolicySnapshotError::UnsupportedVersion(
                self.version,
            ));
        }
        self.policy.validate()?;
        Ok(&self.policy)
    }
}

/// Fail-closed validation for persisted credential-policy provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum CredentialPolicySnapshotError {
    /// The persisted schema requires migration before it can be trusted.
    #[error("unsupported credential policy snapshot version {0}")]
    UnsupportedVersion(u32),
    /// The persisted product policy no longer satisfies protocol invariants.
    #[error("invalid credential policy snapshot: {0}")]
    InvalidPolicy(#[from] PolicyValidationError),
}

/// Policy request that replaces one currently enforced binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "request", rename_all = "snake_case")]
pub enum ReplacementPolicy {
    /// Applies an already compiled product policy expressed as ActPlane DSL.
    Generic(ApplyPolicy),
    /// Compiles a product-level credential policy inside the privileged adapter.
    Credential(ApplyCredentialPolicy),
}

/// Explicit policy kind of the binding being replaced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "snapshot", rename_all = "snake_case")]
pub enum ReplacementSource {
    /// Source was applied as already compiled generic ActPlane DSL.
    Generic,
    /// Source was applied from a structured credential policy.
    Credential(CredentialPolicySnapshot),
}

impl ReplacementSource {
    /// Returns structured provenance only for an explicit credential source.
    pub fn credential_snapshot(&self) -> Option<&CredentialPolicySnapshot> {
        match self {
            Self::Credential(snapshot) => Some(snapshot),
            Self::Generic => None,
        }
    }
}

impl ReplacementPolicy {
    /// Returns the stable identifier of the replacement binding.
    pub fn binding_id(&self) -> Uuid {
        match self {
            Self::Generic(request) => request.binding_id,
            Self::Credential(request) => request.binding_id,
        }
    }

    fn agent_id(&self) -> &str {
        match self {
            Self::Generic(request) => &request.agent_id,
            Self::Credential(request) => &request.agent_id,
        }
    }

    fn session_id(&self) -> Option<&str> {
        match self {
            Self::Generic(request) => request.session_id.as_deref(),
            Self::Credential(request) => request.session_id.as_deref(),
        }
    }

    fn matches_acknowledgement(&self, request: &ApplyPolicy) -> bool {
        match self {
            Self::Generic(expected) => expected == request,
            Self::Credential(expected) => {
                expected.binding_id == request.binding_id
                    && expected.agent_id == request.agent_id
                    && expected.session_id == request.session_id
                    && expected.root_pid == request.root_pid
                    && expected.process_start_time == request.process_start_time
                    && expected.policy.policy_id == request.policy_id
                    && expected.policy.revision.to_string() == request.policy_revision
            }
        }
    }
}

/// Compare-and-replace request for one exact active binding snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplacePolicy {
    /// Complete source acknowledgement that must still own the runtime.
    pub expected: Binding,
    /// Explicit source kind and any provenance required for restoration.
    pub source: ReplacementSource,
    /// Desired policy that receives ownership after a successful handoff.
    pub replacement: ReplacementPolicy,
}

impl ReplacePolicy {
    /// Validates invariants required before privileged runtime mutation.
    ///
    /// # Errors
    ///
    /// Returns a typed error when source and target identities overlap, the
    /// source is not an enforced acknowledgement, or a credential target is
    /// not a valid bounded product policy.
    pub fn validate(&self) -> Result<(), ReplaceValidationError> {
        if self.expected.request.binding_id == self.replacement.binding_id() {
            return Err(ReplaceValidationError::SameBindingId);
        }
        if self.expected.state != BindingState::Enforced {
            return Err(ReplaceValidationError::SourceNotEnforced);
        }
        if self.expected.domain_id.is_none() {
            return Err(ReplaceValidationError::SourceDomainMissing);
        }
        let source = &self.expected.request;
        if self.replacement.agent_id() != source.agent_id {
            return Err(ReplaceValidationError::AgentMismatch);
        }
        if self.replacement.session_id() != source.session_id.as_deref() {
            return Err(ReplaceValidationError::SessionMismatch);
        }
        if let ReplacementSource::Credential(snapshot) = &self.source {
            let policy = snapshot.policy()?;
            if policy.policy_id != source.policy_id
                || policy.revision.to_string() != source.policy_revision
            {
                return Err(ReplaceValidationError::SourcePolicyMismatch);
            }
        }
        if let ReplacementPolicy::Credential(request) = &self.replacement {
            request.policy.validate()?;
        }
        Ok(())
    }

    /// Validates that the target acknowledgement preserves the source runtime.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the acknowledgement differs from the target
    /// request or does not prove continued ownership of the source domain.
    pub fn validate_acknowledgement(
        &self,
        acknowledgement: &Binding,
    ) -> Result<(), ReplaceValidationError> {
        self.validate()?;
        if acknowledgement.state != BindingState::Enforced
            || !self
                .replacement
                .matches_acknowledgement(&acknowledgement.request)
        {
            return Err(ReplaceValidationError::TargetAcknowledgementMismatch);
        }
        if acknowledgement.domain_id != self.expected.domain_id {
            return Err(ReplaceValidationError::RuntimeDomainMismatch);
        }
        Ok(())
    }

    /// Restores the source policy on the process identity that owns the target binding.
    pub fn reverse(&self, target: Binding) -> Self {
        let source = &self.expected.request;
        let current = &target.request;
        let replacement = match &self.source {
            ReplacementSource::Credential(snapshot) => {
                ReplacementPolicy::Credential(ApplyCredentialPolicy {
                    binding_id: source.binding_id,
                    agent_id: current.agent_id.clone(),
                    session_id: current.session_id.clone(),
                    root_pid: current.root_pid,
                    process_start_time: current.process_start_time,
                    policy: snapshot.policy.clone(),
                })
            }
            ReplacementSource::Generic => {
                let mut request = source.clone();
                request.agent_id = current.agent_id.clone();
                request.session_id = current.session_id.clone();
                request.root_pid = current.root_pid;
                request.process_start_time = current.process_start_time;
                ReplacementPolicy::Generic(request)
            }
        };
        let source = match &self.replacement {
            ReplacementPolicy::Credential(request) => {
                ReplacementSource::Credential(CredentialPolicySnapshot {
                    version: CREDENTIAL_POLICY_SNAPSHOT_VERSION,
                    policy: request.policy.clone(),
                })
            }
            ReplacementPolicy::Generic(_) => ReplacementSource::Generic,
        };
        Self {
            expected: target,
            source,
            replacement,
        }
    }
}

/// Stable replacement validation failure safe to expose across the UDS boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ReplaceValidationError {
    /// Source and target bindings must be different immutable identities.
    #[error("source and replacement binding IDs must differ")]
    SameBindingId,
    /// A handoff may replace only a binding acknowledged as enforced.
    #[error("source binding must be enforced")]
    SourceNotEnforced,
    /// The source acknowledgement must identify an assigned runtime domain.
    #[error("source binding must identify its runtime domain")]
    SourceDomainMissing,
    /// Replacement must preserve the source Agent identity.
    #[error("replacement agent must match source agent")]
    AgentMismatch,
    /// Replacement must preserve the source session identity.
    #[error("replacement session must match source session")]
    SessionMismatch,
    /// Structured source policy must describe the acknowledged source revision.
    #[error("source credential policy does not match source binding")]
    SourcePolicyMismatch,
    /// Persisted source policy snapshot is unknown or invalid.
    #[error("source credential policy snapshot is invalid: {0}")]
    SourcePolicySnapshot(#[from] CredentialPolicySnapshotError),
    /// The product-level credential target is invalid.
    #[error("replacement credential policy is invalid: {0}")]
    CredentialPolicy(#[from] PolicyValidationError),
    /// Target acknowledgement does not match the requested replacement.
    #[error("replacement acknowledgement does not match target request")]
    TargetAcknowledgementMismatch,
    /// Target acknowledgement did not preserve the source runtime domain.
    #[error("replacement acknowledgement did not preserve source runtime domain")]
    RuntimeDomainMismatch,
}

/// Stable failure category returned for a replacement that did not apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceFailureCode {
    /// Backend lacks a provable no-gap ownership-transfer primitive.
    UnsupportedHandoff,
    /// Actual runtime ownership did not match the expected source.
    BindingConflict,
    /// The replacement process identity was stale or reused.
    StaleProcess,
    /// The replacement or source policy could not be compiled.
    CompileFailure,
    /// Kernel attachment or runtime state management failed.
    KernelFailure,
}

/// Result of a serialized replacement attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "data", rename_all = "snake_case")]
pub enum ReplaceOutcome {
    /// Replacement was acknowledged and now owns the runtime.
    Applied(Binding),
    /// Replacement was rejected before detachment and the source remained active.
    SourceRetained {
        /// Source binding that still owns the runtime.
        binding: Binding,
        /// Stable reason the replacement was not attempted.
        code: ReplaceFailureCode,
    },
    /// Replacement failed after detachment and the source was restored.
    SourceRestored {
        /// Source binding acknowledged after rollback.
        binding: Binding,
        /// Stable reason the replacement failed.
        code: ReplaceFailureCode,
    },
    /// A different binding owned the runtime and was left untouched.
    Conflict {
        /// Stable ownership-conflict category.
        code: ReplaceFailureCode,
    },
    /// Neither replacement nor source ownership could be proven.
    Indeterminate {
        /// Stable category for the failed runtime operation.
        code: ReplaceFailureCode,
    },
}
