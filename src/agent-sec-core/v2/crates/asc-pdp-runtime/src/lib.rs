//! Runtime authorization combines Cedar decisions with enforceable OS permissions.

#![forbid(unsafe_code)]

mod cedar;
mod decision;
mod os_permissions;
mod path_conditions;
mod request;

use std::collections::HashMap;
use std::sync::Mutex;

use asc_policy_types::Validate;
use asc_policy_types::authorization::{AuthorizationRequest, Decision};
use asc_policy_types::cedar::CedarPolicySource;
use asc_policy_types::permissions::os::OsPermissionSet;
use cedar_policy::Schema;
use serde::{Deserialize, Serialize};

use crate::cedar::PolicyLayer;
use crate::decision::Continuation;

/// Trusted, immutable publication consumed by the runtime, never by a PEP request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySnapshot {
    /// Publication identity used in every issued decision.
    pub version: String,
    /// Mandatory upper bound on task policies.
    pub baseline: CedarPolicySource,
    /// Optional task policy, evaluated separately from the baseline.
    #[serde(default)]
    pub task: Option<CedarPolicySource>,
    /// Deployment-specific upper bound on requested OS permissions.
    pub environment_permissions: OsPermissionSet,
    /// Time to accept an issued decision or resume a transform continuation.
    /// Does not limit approval waiting or execution after the PEP accepts a decision.
    pub ttl_seconds: u64,
}

/// A version-pinned PDP with bounded continuation state for missing execution facts.
pub struct PdpRuntime {
    snapshot: PolicySnapshot,
    schema: Schema,
    layers: Vec<PolicyLayer>,
    continuations: Mutex<HashMap<String, Continuation>>,
}

impl PdpRuntime {
    /// Validates a published snapshot before it can be used for authorization.
    ///
    /// # Errors
    /// Rejects unsupported schemas, invalid policies, unprojectable file conditions,
    /// and invalid publication or permission metadata.
    pub fn from_snapshot(snapshot: PolicySnapshot) -> Result<Self, PdpError> {
        if snapshot.version.is_empty() || snapshot.ttl_seconds == 0 || snapshot.ttl_seconds > 3600 {
            return Err(PdpError::Configuration(
                "publication version and a TTL in 1..=3600 seconds are required".into(),
            ));
        }
        snapshot.environment_permissions.validate()?;
        let schema = asc_policy_cedar::schema::load_schema(&snapshot.baseline.schema)?;
        let mut layers = vec![PolicyLayer::new(&snapshot.baseline)?];
        if let Some(task) = &snapshot.task {
            if task.schema != snapshot.baseline.schema {
                return Err(PdpError::Configuration(
                    "task and baseline must use the same schema revision".into(),
                ));
            }
            layers.push(PolicyLayer::new(task)?);
        }
        Ok(Self {
            snapshot,
            schema,
            layers,
            continuations: Mutex::new(HashMap::new()),
        })
    }

    /// Evaluates one OS request without starting its target or performing approval.
    ///
    /// # Errors
    /// Rejects malformed requests and internal continuation-state failures. Policy
    /// denial, unsupported projections, and missing facts are returned as decisions.
    pub fn evaluate(&self, request: &AuthorizationRequest, now: u64) -> Result<Decision, PdpError> {
        request.validate()?;
        self.evaluate_request(request, now)
    }

    /// Immutable publication identity used by this runtime.
    pub fn policy_version(&self) -> &str {
        &self.snapshot.version
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
    /// The request lacks facts required for a complete decision.
    #[error("path facts required: {0:?}")]
    MissingFacts(Vec<String>),
    /// Cedar request construction or evaluation failed closed.
    #[error("Cedar evaluation failed: {0}")]
    Evaluation(String),
    /// A required grant was absent or explicitly forbidden.
    #[error("authorization denied: {0}")]
    Denied(String),
    /// The in-memory continuation store cannot safely be used.
    #[error("authorization state unavailable")]
    State,
}
