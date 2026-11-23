//! Execution contracts for a fixed sandbox invocation.

#![forbid(unsafe_code)]

pub use asc_os_permission_types::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    /// Original names of explicitly requested grants, linked to the final restricted mounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mount_aliases: Vec<String>,
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
    /// Installation or lifecycle diagnostic.
    pub error: Option<String>,
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
        for alias in &self.mount_aliases {
            validate_os_path(alias)?;
        }
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

/// Local helper capability probe used before asking for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendCapabilities {
    /// Supports default-deny filesystem views.
    pub strict_filesystem: bool,
    /// Supports recursive mount noexec.
    pub noexec: bool,
    /// Supports IP network isolation.
    pub network_off: bool,
}
