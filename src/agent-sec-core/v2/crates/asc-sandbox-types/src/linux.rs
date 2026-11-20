//! Linux helper options, installation handshake, and private execution protocol.

use super::ExecutionSpec;
use serde::{Deserialize, Serialize};

/// Deployment choice for the Linux helper's additional filesystem write restrictions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandlockMode {
    /// Preserve the original helper protocol and skip the kernel probe.
    #[default]
    Off,
    /// Skip only on a recognized ABI compatibility outcome.
    Auto,
    /// Fail before exec unless the complete write ruleset can be installed.
    Required,
}

impl LandlockMode {
    /// Older helpers receive the original wire format for the default mode.
    pub fn is_off(&self) -> bool {
        *self == Self::Off
    }
}

impl std::str::FromStr for LandlockMode {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "required" => Ok(Self::Required),
            _ => Err("expected off, auto or required"),
        }
    }
}

/// Additional write layer reported by the trusted helper at installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum LandlockStatus {
    /// All requested write restrictions were installed.
    Enforced { abi: u32 },
    /// Auto mode retained the existing sandbox after a recognized ABI probe outcome.
    Skipped { reason: String },
}

impl LandlockStatus {
    /// Check the installation evidence against the requested write layer.
    pub fn satisfies(&self, mode: LandlockMode) -> bool {
        match self {
            Self::Enforced { abi } => !mode.is_off() && *abi >= 3,
            Self::Skipped { reason } => mode == LandlockMode::Auto && !reason.trim().is_empty(),
        }
    }
}

/// Installation settings owned by the Linux sandbox backend.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxSandboxOptions {
    /// Additional kernel write ceiling.
    #[serde(default, skip_serializing_if = "LandlockMode::is_off")]
    pub landlock: LandlockMode,
}

/// Linux helper wire format. A missing mode keeps older clients compatible.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxExecutionSpec<S = ExecutionSpec> {
    /// Common launch permissions and pinned paths.
    #[serde(flatten)]
    pub spec: S,
    /// Linux helper configuration on the original flat wire.
    #[serde(default, skip_serializing_if = "LandlockMode::is_off")]
    pub landlock: LandlockMode,
}

/// Setup handshake emitted by the trusted helper as JSON lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendStatus {
    /// All requested restrictions are active.
    Installed,
    /// All baseline restrictions are active and the Landlock outcome is known.
    InstalledWithLandlock { landlock: LandlockStatus },
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
