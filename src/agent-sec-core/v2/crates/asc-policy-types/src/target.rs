//! Minimal target Adapter result and dispatchable plan contracts.

use serde::{Deserialize, Serialize};

/// Result of translating one complete immutable Binding snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationOutcome {
    /// The Adapter produced a target plan that passed its static translation checks.
    Translated(TargetBindingPlan),
    /// The target deterministically cannot express the Binding safely.
    Rejected(TranslationRejection),
}

/// Opaque target-specific Binding payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetBindingPlan {
    /// Versioned target format understood by the matching target Client.
    pub format: String,
    /// Exact bytes retained with the Client's prepared request before any
    /// target mutation. Side-effect-free preparation may precede persistence.
    pub content: Vec<u8>,
}

/// Deterministic semantic rejection produced by a functioning Adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationRejection {
    /// Stable, specific code suitable for status projection and logs.
    pub code: String,
}

/// Internal Adapter failure distinct from a deterministic translation rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("target Adapter failed with code {code}")]
pub struct AdapterFault {
    /// Stable internal failure code.
    pub code: String,
}

/// Stable endpoint/configuration reference plus Client-owned cleanup input.
/// Credentials must never be included. Identity equality uses route and id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetRef {
    pub route: String,
    pub id: String,
    pub cleanup: Vec<u8>,
}

impl TargetRef {
    pub fn same_identity(&self, other: &Self) -> bool {
        self.route == other.route && self.id == other.id
    }
}

/// Presence is evidence, not a live remote-state probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Presence {
    Unknown,
    Present,
    Absent,
}

/// Exact, replayable, non-secret bytes returned by the Client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedApply {
    pub target: TargetRef,
    pub format: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureKind {
    Retryable,
    Rejected,
}

/// Only bounded machine codes cross this boundary; no remote body or DSL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    pub code: String,
}

impl Failure {
    pub fn new(kind: FailureKind, code: &str) -> Self {
        let safe = !code.is_empty()
            && code.len() <= 96
            && code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        Self {
            kind,
            code: if safe {
                code
            } else {
                "RECONCILE_INTERNAL_ERROR"
            }
            .to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    pub target: TargetRef,
    pub presence: Presence,
}

/// A failure may still contain useful confirmations from a partial operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentReport {
    pub observations: Vec<Observation>,
    pub error: Option<Failure>,
}
