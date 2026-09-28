//! Shared action contracts independent of transport and persistence.
//!
//! Each capability owns its execution and audit projection while the runtime
//! owns the common finalization path.

#![forbid(unsafe_code)]

use serde_json::Map;
use serde_json::Value;

/// Implemented action identities used by the common lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    /// Scans Bash or Python code before execution.
    CodeScan,
    /// Scans prompt text for injection or jailbreak before it reaches a model.
    PromptScan,
}

impl ActionId {
    /// Returns the v1-compatible event type for this action.
    #[must_use]
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::CodeScan => "code_scan",
            Self::PromptScan => "prompt_scan",
        }
    }

    /// Returns the v1-compatible event category for this action.
    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::CodeScan => "code_scan",
            Self::PromptScan => "prompt_scan",
        }
    }
}

/// Kernel-authenticated process identity used only for event attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerIdentity {
    /// Effective user ID reported by the Unix socket kernel credentials.
    pub uid: u32,
    /// Effective group ID reported by the Unix socket kernel credentials.
    pub gid: u32,
    /// Process ID reported by the Unix socket kernel credentials.
    pub pid: u32,
}

/// Result returned by a capability execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionOutcome {
    /// Whether the capability completed without an execution failure.
    pub success: bool,
    /// Capability exit code using v1's convention.
    pub exit_code: i64,
    /// Optional human-readable execution failure.
    pub error: Option<String>,
    /// Structured execution failure type; empty on success.
    pub error_type: String,
    /// Capability-specific structured result payload.
    pub data: Map<String, Value>,
}

/// Execution failure fields appended to a completed audit projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Optional human-readable error copied only when v1 would include it.
    pub error: Option<String>,
    /// Structured error type.
    pub error_type: String,
    /// Capability exit code.
    pub exit_code: i64,
}

/// Audited request and outcome after capability-specific sanitization.
#[derive(Debug, Clone, PartialEq)]
pub enum AuditProjection {
    /// v1 `post_action`: request plus result, with optional failure fields.
    Completed {
        /// Audited request fields.
        request: Map<String, Value>,
        /// Audited result fields.
        result: Map<String, Value>,
        /// Failure fields when the executor returned an unsuccessful outcome.
        failure: Option<Failure>,
    },
    /// v1 `on_error`: request plus exception fields, without a result key.
    Failed {
        /// Audited request fields.
        request: Map<String, Value>,
        /// Sanitized exception text.
        error: String,
        /// Exception type.
        error_type: String,
    },
}

impl AuditProjection {
    /// Serializes this projection into the v1 `SecurityEvent.details` shape.
    #[must_use]
    pub fn into_details(self) -> Map<String, Value> {
        let mut details = Map::new();
        match self {
            Self::Completed {
                request,
                result,
                failure,
            } => {
                details.insert("request".to_owned(), Value::Object(request));
                details.insert("result".to_owned(), Value::Object(result));
                if let Some(failure) = failure {
                    if let Some(error) = failure.error {
                        details.insert("error".to_owned(), Value::String(error));
                    }
                    details.insert("error_type".to_owned(), Value::String(failure.error_type));
                    details.insert(
                        "exit_code".to_owned(),
                        Value::Number(failure.exit_code.into()),
                    );
                }
            }
            Self::Failed {
                request,
                error,
                error_type,
            } => {
                details.insert("request".to_owned(), Value::Object(request));
                details.insert("error".to_owned(), Value::String(error));
                details.insert("error_type".to_owned(), Value::String(error_type));
            }
        }
        details
    }
}

/// Request accepted by the code-scan capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeScanRequest {
    /// Code supplied by the caller.
    pub code: String,
    /// Language literal supplied by the caller.
    pub language: String,
    /// Optional selected rule IDs.
    pub rules: Option<Vec<String>>,
    /// Optional engine mode.
    pub mode: Option<String>,
}

/// Request accepted by the prompt-scan capability.
///
/// `history` carries the raw conversation turns of a `multi_turn` scan; each
/// entry keeps the wire shape (`{"role","content"}` object or legacy
/// `"role: content"` string) and the capability crate owns the tolerant
/// decode, so a malformed turn degrades to an `UNKNOWN` role instead of
/// failing the request. `serde_json::Value` has no `Eq`, so the contract
/// derives `PartialEq` only.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptScanRequest {
    /// Prompt text supplied by the caller (`current_query` in `multi_turn`).
    pub text: String,
    /// Optional scan mode (`fast`, `standard`, `strict`, `multi_turn`).
    pub mode: Option<String>,
    /// Optional input origin label recorded in result metadata.
    pub source: Option<String>,
    /// Optional L2 backend override; only consumed by `standard`/`strict`.
    pub model: Option<String>,
    /// Prior assistant response of the conversation triple (`multi_turn` only).
    pub assistant_response: Option<String>,
    /// Conversation history of the triple (`multi_turn` only).
    pub history: Option<Vec<serde_json::Value>>,
}

/// Probe accepted by the prompt-scan warmup port.
///
/// A readiness check, not a scan: it carries the mode whose layers are probed
/// and the same optional L2 backend override as [`PromptScanRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptScanWarmupRequest {
    /// Scan mode whose layers are probed (`fast`, `standard`, `strict`,
    /// `multi_turn`).
    pub mode: Option<String>,
    /// Optional L2 backend override; only consumed by `standard`/`strict`.
    pub model: Option<String>,
}
