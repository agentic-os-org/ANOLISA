//! Shared PII scan input contracts, independent of detector implementation.

use serde::{Deserialize, Serialize};

/// Input origin supplied by the caller, never an authorization claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// User's current input.
    UserInput,
    /// Tool arguments.
    ToolInput,
    /// Tool result.
    ToolOutput,
    /// Final model response.
    ModelOutput,
    /// Observability payload before storage.
    Observability,
    /// Explicit interactive scan.
    Manual,
    /// Unspecified origin.
    #[default]
    Unknown,
}

/// Options shared by direct callers and the daemon adapter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
// These independent switches are the existing public scan options.
#[allow(clippy::struct_excessive_bools)]
pub struct PiiScanOptions {
    /// Input origin.
    pub source: Source,
    /// Retain findings below the v1 confidence threshold of 0.5.
    pub include_low_confidence: bool,
    /// Include raw evidence in the client response only.
    pub raw_evidence: bool,
    /// Return a full redacted copy of the scanned prefix.
    pub redact_output: bool,
    /// Optional UTF-8 byte prefix; must be greater than zero.
    pub max_bytes: Option<usize>,
    /// The client omitted bytes before sending this text.
    pub input_truncated: bool,
    /// Client prefix length retained for the legacy `bytes_scanned` counter.
    pub input_bytes_scanned: Option<usize>,
}

/// One scan invocation; raw text deliberately has no diagnostic `Debug` projection.
#[derive(Clone)]
pub struct PiiScanRequest {
    /// Exact UTF-8 text received from the caller.
    pub text: String,
    /// Explicit input coverage and response options.
    pub options: PiiScanOptions,
    /// Optional business metadata, never a trusted identity.
    pub agent_name: Option<String>,
}
