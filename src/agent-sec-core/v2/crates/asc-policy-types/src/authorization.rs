//! Authorization and execution contracts for a fixed sandbox invocation.

use crate::{OsPermissionSet, Validate, ValidationError, validate_os_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Logical Agent attribution; transport identity is established by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPrincipal {
    /// Stable identifier used as the Cedar Subject UID.
    pub agent_id: String,
    /// Agent family, for example codex or qoder.
    pub agent_type: String,
}

/// The original command and its resolved execution target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    /// Original command token, retained for command-level policy.
    pub program: String,
    /// Actual executable selected by PATH/cwd resolution.
    pub resolved_program: String,
    /// Ordered arguments excluding argv zero.
    pub args: Vec<String>,
    /// Absolute working directory used by the target.
    pub cwd: String,
    /// Exact target environment; never applied to the privileged setup helper.
    pub env: BTreeMap<String, String>,
}

/// Observed path identity, checked again before installing mounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathFact {
    /// Absolute requested path, preserving an intentional mount alias.
    pub path: String,
    /// Canonical identity resolved from the requested path.
    pub canonical_path: String,
    /// Whether this path represents a subtree.
    pub is_directory: bool,
    /// Device from stat.
    pub device: u64,
    /// Inode from stat.
    pub inode: u64,
}

/// Capabilities advertised by the executing PEP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
// Capabilities are independent protocol flags, not mutually exclusive execution states.
#[allow(clippy::struct_excessive_bools)]
pub struct PepCapabilities {
    /// Supports default-deny filesystem views.
    pub strict_filesystem: bool,
    /// Supports recursive mount noexec.
    pub noexec: bool,
    /// Supports IP network isolation.
    pub network_off: bool,
    /// Has an explicit approval callback.
    pub ask: bool,
}

/// Request family; future families use separate payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    /// Fixed OS process-tree permissions.
    Os,
}

/// One upper-PEP authorization request, never a policy upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationRequest {
    /// Dispatches to the OS Cedar request adapter.
    #[serde(rename = "type")]
    pub request_type: RequestType,
    /// Identifier of this preparation attempt.
    pub request_id: String,
    /// Identifier of the invocation across continuation attempts.
    pub execution_id: String,
    /// Logical Agent attributes.
    pub principal: AgentPrincipal,
    /// Fully prepared launch.
    pub launch: LaunchSpec,
    /// Explicit permissions including runtime dependencies.
    pub requested_permissions: OsPermissionSet,
    /// Facts needed to distinguish files from subtree roots.
    pub path_facts: Vec<PathFact>,
    /// Backend and approval capabilities.
    pub capabilities: PepCapabilities,
    /// Opaque PDP continuation for a previously deferred candidate.
    #[serde(default)]
    pub continuation: Option<String>,
}

/// AARM decision states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionVerdict {
    /// The supplied action is authorized.
    Allow,
    /// No execution is authorized.
    Deny,
    /// A fully rechecked candidate is authorized.
    Modify,
    /// The bound action awaits local approval.
    StepUp,
    /// More preparation is required, with no execution right.
    Defer,
}

/// Version- and request-bound result from the PDP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    /// Unique decision identifier used for receipts and replay rejection.
    pub decision_id: String,
    /// Preparation attempt that produced this result.
    pub request_id: String,
    /// Invocation to which this result belongs.
    pub execution_id: String,
    /// SHA-256 of the complete canonical request representation.
    pub request_digest: String,
    /// Published policy snapshot identifier.
    pub policy_version: String,
    /// Unix-second deadline for the PEP to accept this decision for one invocation.
    /// Once accepted, approval and execution may finish after this deadline.
    pub expires_at: u64,
    /// Bound logical Agent.
    pub principal: AgentPrincipal,
    /// Bound final launch.
    pub launch: LaunchSpec,
    /// Decision state.
    pub verdict: DecisionVerdict,
    /// Present only for executable or approval-pending decisions.
    #[serde(default)]
    pub effective_permissions: Option<OsPermissionSet>,
    /// Human-readable explanation.
    pub reason: String,
    /// All applicable warning advice.
    pub warnings: Vec<String>,
    /// Unapproved transformed candidate for complete re-preparation.
    #[serde(default)]
    pub candidate_launch: Option<LaunchSpec>,
    /// Additional path facts required by the policy range evaluator.
    pub missing_paths: Vec<String>,
    /// Opaque token binding re-preparation to previous advice.
    #[serde(default)]
    pub continuation: Option<String>,
}

/// Exact input to the isolated backend, independent of PDP communication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSpec {
    /// Backend wire version.
    pub version: u32,
    /// Executable and target environment.
    pub launch: LaunchSpec,
    /// Permissions to install without implicit grants.
    pub permissions: OsPermissionSet,
    /// Path identities observed during preparation.
    pub path_facts: Vec<PathFact>,
}

/// Setup handshake emitted by the trusted helper as JSON lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendStatus {
    /// All requested restrictions are active.
    Installed,
    /// Target exec succeeded.
    Started,
    /// Target terminated; the namespace is being reaped.
    Exited {
        /// Normal exit status, if any.
        exit_code: Option<i32>,
        /// Terminating signal, if any.
        signal: Option<i32>,
    },
    /// Setup or execution failed.
    Failed {
        /// Diagnostic without target environment contents.
        message: String,
    },
}

/// Installation and execution outcomes are reported independently.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    /// Received a successful restriction-installation handshake.
    pub installed: bool,
    /// Target exec succeeded.
    pub started: bool,
    /// Target's normal exit status.
    pub exit_code: Option<i32>,
    /// Target's terminating signal.
    pub signal: Option<i32>,
    /// Observed external signal that terminated the helper, separate from target termination.
    #[serde(default)]
    pub cancel_signal: Option<i32>,
    /// Installation, lifecycle, or authorization diagnostic.
    pub error: Option<String>,
}

/// Upper PEP receipt; direct CLI execution does not send one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionReceipt {
    /// PDP decision being implemented.
    pub decision_id: String,
    /// Bound preparation attempt.
    pub request_id: String,
    /// Bound invocation.
    pub execution_id: String,
    /// Result of the explicit approval callback, if requested.
    pub approval: Option<bool>,
    /// Actual outcome, including failures before target exec.
    pub result: ExecutionResult,
    /// Unix time at result collection.
    pub recorded_at: u64,
}

/// Confirms durable recording, not successful target execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAck {
    /// Recorded decision identifier.
    pub decision_id: String,
    /// True only after checked storage writes complete.
    pub recorded: bool,
}

impl Validate for LaunchSpec {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_os_path(&self.resolved_program)?;
        validate_os_path(&self.cwd)?;
        if self.program.is_empty()
            || self.program.contains('\0')
            || self.args.iter().any(|s| s.contains('\0'))
        {
            return Err(ValidationError::new(
                "launch",
                "empty program or NUL in argv",
            ));
        }
        if self
            .env
            .iter()
            .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
        {
            return Err(ValidationError::new(
                "launch.env",
                "invalid environment entry",
            ));
        }
        Ok(())
    }
}

impl Validate for AuthorizationRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        for id in [
            &self.request_id,
            &self.execution_id,
            &self.principal.agent_id,
            &self.principal.agent_type,
        ] {
            if id.is_empty() || id.len() > 256 || id.contains('\0') {
                return Err(ValidationError::new(
                    "identity",
                    "expected a nonempty bounded identifier",
                ));
            }
        }
        self.launch.validate()?;
        self.requested_permissions.validate()?;
        validate_facts(&self.path_facts)
    }
}

impl AuthorizationRequest {
    /// Hashes every field used to prepare the launch and authorize permissions.
    ///
    /// # Errors
    /// Returns a serialization error rather than producing a partial binding.
    pub fn digest(&self) -> Result<String, ValidationError> {
        let bytes =
            serde_json::to_vec(self).map_err(|e| ValidationError::new("request", e.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

impl Validate for ExecutionSpec {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.version != 1 {
            return Err(ValidationError::new(
                "version",
                "unsupported execution version",
            ));
        }
        self.launch.validate()?;
        self.permissions.validate()?;
        validate_facts(&self.path_facts)
    }
}

fn validate_facts(facts: &[PathFact]) -> Result<(), ValidationError> {
    let mut seen = BTreeSet::new();
    for fact in facts {
        validate_os_path(&fact.path)?;
        validate_os_path(&fact.canonical_path)?;
        if !seen.insert(&fact.path) {
            return Err(ValidationError::new(
                "path_facts",
                "duplicate path identity",
            ));
        }
    }
    Ok(())
}
