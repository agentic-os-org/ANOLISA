//! Shared action contracts independent of transport and persistence.
//!
//! Each capability owns its execution and audit projection while the runtime
//! owns the common finalization path.

#![forbid(unsafe_code)]

use serde_json::Map;
use serde_json::Value;

mod trace;
pub use trace::ActionTraceContext;

/// Identities supported by implemented action capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    /// Scans Bash or Python code before execution.
    CodeScan,
    /// Detects personal information and credentials in supplied text.
    PiiScan,
}

impl ActionId {
    /// Returns the v1-compatible event type for this action.
    #[must_use]
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::CodeScan => "code_scan",
            Self::PiiScan => "pii_scan",
        }
    }

    /// Returns the v1-compatible event category for this action.
    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::CodeScan => "code_scan",
            Self::PiiScan => "pii_scan",
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

/// Optional business-correlation fields carried by an action invocation.
///
/// These opaque V1 strings are not OpenTelemetry `TraceId` or `SpanId` values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Correlation {
    /// Opaque v1-compatible trace correlation string.
    pub trace_id: String,
    /// Optional agent session identifier.
    pub session_id: Option<String>,
    /// Optional agent run identifier.
    pub run_id: Option<String>,
    /// Optional LLM call identifier.
    pub call_id: Option<String>,
    /// Optional tool-call identifier.
    pub tool_call_id: Option<String>,
}

/// Trusted caller identity plus optional business correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionAttribution {
    /// Kernel-authenticated caller identity.
    pub caller: CallerIdentity,
    /// Business correlation fields.
    pub correlation: Correlation,
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
