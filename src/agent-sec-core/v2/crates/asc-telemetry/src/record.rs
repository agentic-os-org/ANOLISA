//! V1 scan telemetry allowlist. Unknown output fields never become telemetry.
use serde::Serialize;
use serde_json::{Map, Value};

/// Closed telemetry record. Construction always passes through the allowlist.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TelemetryRecord(Map<String, Value>);

/// Finalized scan facts; no request, raw error, or correlation fields are accepted.
pub struct ScanTelemetryInput<'a> {
    /// Registered scan event kind.
    pub event_type: &'a str,
    /// Registered scan category.
    pub category: &'a str,
    /// Whether execution succeeded, independently of verdict.
    pub succeeded: bool,
    /// Timestamp of the terminal audit event.
    pub timestamp: &'a str,
    /// Capability result from which only verdict and elapsed time are selected.
    pub result: &'a Map<String, Value>,
    /// Structured execution error type, checked against V1's scalar grammar.
    pub error_type: &'a str,
    /// Absent for an unhandled execution failure, as in V1 `on_error`.
    pub exit_code: Option<i64>,
    /// Optional untrusted Agent product attribution.
    pub agent_name: Option<&'a str>,
}

impl TelemetryRecord {
    /// Projects scan facts with the V1 telemetry schema and scalar validation.
    #[must_use]
    pub fn for_scan(input: &ScanTelemetryInput<'_>) -> Self {
        let mut fields = Map::new();
        for (key, value) in [
            ("component.name", "agent-sec-core"),
            ("component.version", env!("CARGO_PKG_VERSION")),
            ("component.agent_name", agent_name(input.agent_name)),
            ("seccore.event_type", input.event_type),
            ("seccore.category", input.category),
            (
                "seccore.result",
                if input.succeeded {
                    "succeeded"
                } else {
                    "failed"
                },
            ),
            ("seccore.timestamp", input.timestamp),
        ] {
            fields.insert(key.to_owned(), Value::String(value.to_owned()));
        }
        if input.event_type == "code_scan" {
            if let Some(value) = input.result.get("verdict")
                && matches!(value.as_str(), Some("pass" | "warn" | "deny" | "error"))
            {
                fields.insert("seccore.verdict".to_owned(), value.clone());
            }
            if let Some(value) = input.result.get("elapsed_ms")
                && value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
            {
                fields.insert("seccore.elapsed_ms".to_owned(), value.clone());
            }
        }
        if !input.succeeded && valid_error_type(input.error_type) {
            fields.insert("seccore.error_type".to_owned(), input.error_type.into());
            if let Some(exit_code) = input.exit_code {
                fields.insert("seccore.exit_code".to_owned(), exit_code.into());
            }
        }
        Self(fields)
    }
}

fn agent_name(value: Option<&str>) -> &str {
    value
        .map(str::trim)
        .filter(|name| {
            matches!(
                *name,
                "codex" | "cosh" | "hermes" | "openclaw" | "qoder" | "qwencode"
            )
        })
        .unwrap_or_default()
}

fn valid_error_type(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphabetic()
        && bytes
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.'))
}
