//! Runtime authorization combines Cedar decisions with enforceable OS permissions.

#![forbid(unsafe_code)]

mod cedar;
mod decision;
mod os_permissions;
mod path_conditions;
mod request;

use asc_daemon_protocol::authorization::{Decision, OsAuthorizationRequest};
use asc_policy_types::Validate;
use asc_policy_types::cedar::AuthorizationPolicySet;
use cedar_policy::Schema;

use crate::cedar::CompiledPolicySet;

/// Deadline for accepting a decision; accepted approval and execution may finish later.
pub const DECISION_TTL_SECONDS: u64 = 60;

/// A version-pinned PDP.
pub struct PdpRuntime {
    version: String,
    schema: Schema,
    policy: CompiledPolicySet,
}

impl PdpRuntime {
    /// Loads one PAP-derived set with global Cedar forbid precedence.
    ///
    /// # Errors
    /// Rejects invalid metadata, ineligible members, incompatible schemas or OS
    /// projections. The empty set is valid and denies every request.
    pub fn from_policy_set(snapshot: AuthorizationPolicySet) -> Result<Self, PdpError> {
        if snapshot.version.is_empty() {
            return Err(PdpError::Configuration(
                "policy set version is required".into(),
            ));
        }
        let schema =
            asc_policy_cedar::schema::load_schema(&asc_policy_cedar::schema::embedded_reference())?;
        let policy = CompiledPolicySet::from_members(&snapshot.policies)?;
        Ok(Self {
            version: snapshot.version,
            schema,
            policy,
        })
    }

    /// Evaluates one OS request without starting its target or performing approval.
    ///
    /// # Errors
    /// Rejects malformed requests. Policy
    /// denial, unsupported projections, and missing facts are returned as decisions.
    pub fn evaluate(
        &self,
        request: &OsAuthorizationRequest,
        now: u64,
    ) -> Result<Decision, PdpError> {
        request.validate()?;
        self.evaluate_request(request, now)
    }

    /// Immutable publication identity used by this runtime.
    pub fn policy_version(&self) -> &str {
        &self.version
    }
}

/// Failures admitting a snapshot or preparing an authorization result.
#[derive(Debug, thiserror::Error)]
pub enum PdpError {
    /// A caller supplied invalid typed data.
    #[error("invalid authorization input: {0}")]
    Validation(#[from] asc_policy_types::ValidationError),
    /// The shared frontend rejected the policy source.
    #[error("invalid Cedar source: {0}")]
    Cedar(#[from] asc_policy_cedar::CedarError),
    /// Publication configuration is not usable.
    #[error("invalid PDP configuration: {0}")]
    Configuration(String),
    /// A policy cannot be represented by the current OS executor.
    #[error("unsupported sandbox policy: {0}")]
    Unsupported(String),
    /// Cedar request construction or evaluation failed closed.
    #[error("Cedar evaluation failed: {0}")]
    Evaluation(String),
    /// A required grant was absent or explicitly forbidden.
    #[error("authorization denied: {0}")]
    Denied(String),
}
