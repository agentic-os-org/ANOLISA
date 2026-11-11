//! Authorization contracts for a fixed sandbox invocation.

pub use asc_sandbox_types::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Caller-supplied Agent classification; individual identity is not modeled yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPrincipal {
    /// Agent family, for example codex or qoder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Request family; future families use separate payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    /// Fixed OS process-tree permissions.
    Os,
}

/// Command facts used by Cedar. The executor retains the environment and object identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    /// Original command token.
    pub program: String,
    /// Canonical executable resolved by the caller.
    pub resolved_program: String,
    /// Ordered arguments after argv zero.
    pub args: Vec<String>,
    /// Canonical working directory.
    pub cwd: String,
}

impl From<&LaunchSpec> for CommandRequest {
    fn from(value: &LaunchSpec) -> Self {
        Self {
            program: value.program.clone(),
            resolved_program: value.resolved_program.clone(),
            args: value.args.clone(),
            cwd: value.cwd.clone(),
        }
    }
}

impl Validate for CommandRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_os_path(&self.resolved_program)?;
        validate_os_path(&self.cwd)?;
        if self.program.is_empty()
            || self.program.contains('\0')
            || self.args.iter().any(|a| a.contains('\0'))
        {
            return Err(ValidationError::new(
                "launch",
                "empty program or NUL in argv",
            ));
        }
        Ok(())
    }
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
    /// Identifier of the invocation across authorization and execution.
    pub execution_id: String,
    /// Agent classification supplied by the caller.
    pub principal: AgentPrincipal,
    /// Fully prepared launch.
    pub launch: CommandRequest,
    /// Explicit permissions including runtime dependencies.
    pub requested_permissions: OsPermissionSet,
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

/// The part of a fixed invocation that triggered confirmation, not a separate grant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApprovalTarget {
    /// Command-level confirmation, including requirements retained after a transform.
    Execution {
        /// The requirement came from the original command before policy transformation.
        original_request: bool,
    },
    /// A final, enforceable filesystem region requiring confirmation.
    Filesystem {
        /// Canonical region root; a real directory includes its descendants.
        path: String,
        /// Write retains the OS permission contract, including read access.
        right: FileRight,
        /// More-specific regions are evaluated independently and excluded from this item.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excluded_paths: Vec<String>,
    },
    /// Host networking without target or port filtering.
    Network,
}

/// Display provenance bound to the same immutable decision as the final launch and permissions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequirement {
    /// The execution or resource access that requires confirmation.
    pub target: ApprovalTarget,
    /// Human-facing policy message; a reason code is not treated as explanatory prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
    pub launch: CommandRequest,
    /// Decision state.
    pub verdict: DecisionVerdict,
    /// Present only for executable or approval-pending decisions.
    #[serde(default)]
    pub effective_permissions: Option<OsPermissionSet>,
    /// Human-readable explanation.
    pub reason: String,
    /// Only requirements that triggered approval; never the complete permission set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_requirements: Vec<ApprovalRequirement>,
    /// All applicable warning advice.
    pub warnings: Vec<String>,
    /// Point-only rights are enforceable only when these paths are files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_files: Vec<String>,
}

impl Decision {
    /// Checks the contract a PEP must bind before approval or handoff.
    /// The local backend separately verifies every supplied identity against the actual object.
    ///
    /// # Errors
    /// Rejects command substitution, different permissions, aliases, and recursive point grants.
    pub fn validate_execution(&self, spec: &ExecutionSpec) -> Result<(), ValidationError> {
        spec.validate()?;
        if CommandRequest::from(&spec.launch) != self.launch
            || self.effective_permissions.as_ref() != Some(&spec.permissions)
        {
            return Err(ValidationError::new(
                "execution",
                "execution handoff does not match decision",
            ));
        }
        for path in spec
            .permissions
            .filesystem
            .rules
            .iter()
            .map(|r| &r.path)
            .chain(&spec.permissions.execution.deny_paths)
        {
            if !spec
                .path_facts
                .iter()
                .any(|f| &f.path == path && f.canonical_path == f.path)
            {
                return Err(ValidationError::new(
                    "execution",
                    format!("policy path has a different canonical target: {path}"),
                ));
            }
        }
        if self.require_files.iter().any(|path| {
            !spec
                .path_facts
                .iter()
                .any(|f| &f.path == path && f.canonical_path == f.path && !f.is_directory)
        }) {
            return Err(ValidationError::new(
                "execution",
                "point-only policy cannot authorize directory descendants",
            ));
        }
        Ok(())
    }
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

/// A host handoff never changes or reevaluates the authorized invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPhase {
    /// Accept before expiry, then wait for host approval without consuming the deadline.
    Accept,
    /// Transfer the accepted invocation to its executor exactly once.
    Execute,
    /// The foreground host has finished with this invocation.
    Abandon,
    /// The serial host has advanced beyond the previous tool in this run.
    FinishRun,
}

/// Owner-bound handoff of an existing decision, not a new authorization request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionClaim {
    /// Decision previously returned to this Unix peer; omitted for run cleanup.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub decision_id: String,
    /// Requested handoff transition.
    pub phase: ClaimPhase,
    /// Cosh run owning this handoff, present on accept or run cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Pinned local state transferred only when accepting a decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionSpec>,
}

/// Immutable invocation retained by the daemon while the host asks the user.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedAuthorization {
    /// Pinned by the PEP before presenting any approval.
    pub execution: ExecutionSpec,
    /// Original request bound to the decision.
    pub request: AuthorizationRequest,
    /// Final decision; callers cannot replace it during handoff.
    pub decision: Decision,
    /// Time at which the PEP accepted the decision before asking the user.
    pub accepted_at: u64,
}

/// A consumed or already completed handoff has no invocation to execute again.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimResponse {
    /// Correlates this response with the requested decision.
    pub decision_id: String,
    /// Returned only to the first executor.
    pub authorization: Option<AcceptedAuthorization>,
}

impl Validate for AuthorizationRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        for id in [&self.request_id, &self.execution_id] {
            if id.is_empty() || id.len() > 256 || id.contains('\0') {
                return Err(ValidationError::new(
                    "identity",
                    "expected a nonempty bounded identifier",
                ));
            }
        }
        self.principal.validate()?;
        self.launch.validate()?;
        self.requested_permissions.validate()
    }
}

impl Validate for AgentPrincipal {
    fn validate(&self) -> Result<(), ValidationError> {
        if let Some(agent_type) = &self.agent_type
            && (agent_type.is_empty() || agent_type.len() > 256 || agent_type.contains('\0'))
        {
            return Err(ValidationError::new(
                "principal.agent_type",
                "expected a nonempty bounded type name",
            ));
        }
        Ok(())
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
