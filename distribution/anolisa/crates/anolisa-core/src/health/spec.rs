//! Structured health-check specifications (data only).
//!
//! A [`CheckSpec`] is the declarative `[component.health_check]` form from
//! the minimal component schema. serde's internal tag (`type = "..."`) maps
//! one-to-one onto the TOML discriminator, so `type = "binary_version"`
//! deserializes straight into [`CheckSpec::BinaryVersion`] without a manual
//! visitor. Execution lives in [`super::engine`]; nothing here touches the
//! filesystem or spawns processes.

use serde::{Deserialize, Serialize};

/// One health check — a leaf probe or an aggregate of child checks.
///
/// Internally tagged by `type` to mirror the wire form
/// `type = "binary_version"`. Leaf variants describe a single observation;
/// [`CheckSpec::AllOf`] / [`CheckSpec::AnyOf`] compose them so `doctor` can
/// render a tree. The engine implements a v1 subset (binary/file/command +
/// aggregates) and reports the rest as [`CheckStatus::Unsupported`] until the
/// owning slice lands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CheckSpec {
    /// Run `<binary> --version`, require exit 0, and optionally require the
    /// stdout to contain `expect_pattern`.
    BinaryVersion {
        /// Executable path; `{bindir}`-style placeholders are expanded and
        /// the result must resolve under an ANOLISA-owned root.
        binary: String,
        /// Substring the version output must contain (v1 is a plain
        /// substring match, not a regex).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_pattern: Option<String>,
        /// Per-process timeout override; falls back to the engine default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    /// Run `<binary> --help` and require exit 0.
    BinaryHelp {
        /// Executable path (placeholder-expanded, owned-root bounded).
        binary: String,
        /// Per-process timeout override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    /// systemd unit reports `active`. Unsupported until daemon components
    /// (Slice 3) wire a [`crate::service::ServiceManager`] into the engine.
    SystemdActive {
        /// Unit name, e.g. `agentsight.service`.
        service: String,
    },
    /// A regular file exists at `path` (optionally with the given mode).
    FileExists {
        /// Target path (placeholder-expanded, owned-root bounded, symlinks
        /// refused).
        path: String,
        /// Required Unix mode, e.g. `"0755"`; `None` skips the mode check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// Required owner; not enforced in v1 (recorded for diagnostics).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
    },
    /// A process is listening on `port`. Unsupported in v1.
    PortListen {
        /// TCP/UDP port number.
        port: u16,
        /// Transport; defaults to TCP.
        #[serde(default)]
        protocol: Protocol,
    },
    /// HTTP GET returns the expected status/body. Unsupported in v1.
    HttpGet {
        /// Absolute URL to probe.
        url: String,
        /// Required HTTP status code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_status: Option<u16>,
        /// Substring the body must contain.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_body_contains: Option<String>,
    },
    /// A binary carries the listed Linux capabilities. Unsupported until
    /// setcap support (T2.7) or Slice 8 lands.
    BinaryCapabilities {
        /// Executable path to inspect.
        binary: String,
        /// Capability names, e.g. `["cap_bpf", "cap_perfmon"]`.
        caps: Vec<String>,
    },
    /// Run an explicit argv (no shell) and check the exit code.
    Command {
        /// Argument vector; `argv[0]` is the executable (placeholder-expanded,
        /// owned-root bounded).
        argv: Vec<String>,
        /// Required exit code; defaults to 0 when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_exit_code: Option<i32>,
    },
    /// Aggregate: passes only when every child passes.
    AllOf {
        /// Child checks evaluated in order.
        checks: Vec<CheckSpec>,
        /// Aggregate timeout override (reserved; per-leaf timeouts apply in
        /// v1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    /// Aggregate: passes when at least one child passes.
    AnyOf {
        /// Child checks evaluated in order.
        checks: Vec<CheckSpec>,
        /// Aggregate timeout override (reserved).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
}

/// Transport for [`CheckSpec::PortListen`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// TCP listener (default).
    #[default]
    Tcp,
    /// UDP listener.
    Udp,
}

/// Status of a single check node.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Probe ran and passed.
    Ok,
    /// Probe ran and did not pass.
    Failed,
    /// Probe was not executed (dry-run short-circuit).
    Skipped,
    /// Check type is not implemented on this platform/version yet.
    Unsupported,
}

impl CheckStatus {
    /// Stable lowercase label (`ok`, `failed`, `skipped`, `unsupported`).
    ///
    /// Matches the serde `rename_all` wire form and the string persisted in
    /// [`crate::state::HealthEntry::status`], so callers can record a probe
    /// verdict into state without re-deriving the spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One leaf or aggregate check outcome, structured so `doctor`/`status` can
/// render a tree. [`children`](Self::children) is populated for aggregates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Short human label, e.g. `binary_version binary=/usr/local/bin/x`.
    pub spec_label: String,
    /// Aggregated status for this node.
    pub status: CheckStatus,
    /// Expected/actual diff or failure reason, when non-obvious.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Child outcomes for aggregate checks (`all_of` / `any_of`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CheckOutcome>,
}

impl CheckOutcome {
    /// Construct a leaf outcome with no children.
    pub(super) fn leaf(spec_label: String, status: CheckStatus, detail: Option<String>) -> Self {
        Self {
            spec_label,
            status,
            detail,
            children: Vec::new(),
        }
    }
}
