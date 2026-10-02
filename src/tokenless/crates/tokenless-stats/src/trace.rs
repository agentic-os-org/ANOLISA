//! W3C Trace Context correlation for tokenless observability records.
//!
//! An agent observability backend such as AgentLoop joins component telemetry
//! by trace identity, but tokenless emits no spans of its own: it runs as a
//! short-lived hook process spawned by the agent host. The environment is the
//! propagation channel that needs no host-side protocol change, but it is a
//! contract the launcher has to fulfil: OpenTelemetry propagates W3C context
//! through in-process carriers and does not export the active span into a
//! child environment, so a host or adapter that wants correlation must inject
//! the `traceparent` of the span it is currently in before spawning tokenless.
//! tokenless is the receiving end — it stamps the identity it was given onto
//! the observability records it writes, and writes them uncorrelated when it
//! was given none.
//!
//! Only the exported (SLS) records carry the identity. The local `stats.db`
//! deliberately does not: nothing reads trace columns back there, and a
//! persisted column would cascade into the database migration, the CLI output,
//! and the Python SDK record model without a local consumer.
//!
//! Parsing is strict and fail-soft — an absent, malformed, or all-zero context
//! yields `None` and the record is written uncorrelated, matching the rest of
//! tokenless-stats, which never blocks compression on observability.

use serde::{Deserialize, Serialize};

/// Adapter-facing override, checked before [`STANDARD_TRACEPARENT_ENV`].
///
/// Lets an adapter pin the trace identity when the host exports `TRACEPARENT`
/// for a different span, or when a sandbox strips inherited variables the way
/// the DeepSeek Harness adapter already works around for `TOKENLESS_*`.
pub const TOKENLESS_TRACEPARENT_ENV: &str = "TOKENLESS_TRACEPARENT";

/// W3C Trace Context environment propagation variable.
///
/// The name follows the W3C/OpenTelemetry convention, but nothing populates it
/// automatically: an OpenTelemetry SDK keeps the active span in an in-process
/// carrier, so the launching host or adapter has to inject the current
/// `traceparent` into the environment it spawns tokenless with.
pub const STANDARD_TRACEPARENT_ENV: &str = "TRACEPARENT";

/// Length of the `trace-id` field: 16 bytes as lowercase hex.
const TRACE_ID_LEN: usize = 32;
/// Length of the `parent-id` field: 8 bytes as lowercase hex.
const SPAN_ID_LEN: usize = 16;
/// Version value reserved by the specification as invalid.
const INVALID_VERSION: &str = "ff";
/// Fields in a version `00` `traceparent`; higher versions may append more.
const V0_FIELD_COUNT: usize = 4;

/// Trace identity of the host span a tokenless operation ran under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    /// W3C trace id: 32 lowercase hex characters, never all zero.
    pub trace_id: String,
    /// Id of the enclosing host span: 16 lowercase hex characters, never all zero.
    ///
    /// This is the `parent-id` field of `traceparent`. tokenless creates no
    /// span, so the host span is the one its work belongs to.
    pub span_id: String,
}

impl TraceContext {
    /// Parses a W3C `traceparent` value.
    ///
    /// Accepts version `00` plus any higher version except the reserved `ff`;
    /// above `00` trailing fields are ignored, per the specification's
    /// forward-compatibility rule. Returns `None` for a malformed value,
    /// uppercase hex, a short or long field, or an all-zero trace or span id.
    #[must_use]
    pub fn parse(traceparent: &str) -> Option<Self> {
        let fields: Vec<&str> = traceparent.split('-').collect();
        if fields.len() < V0_FIELD_COUNT {
            return None;
        }
        let (version, trace_id, span_id, flags) = (fields[0], fields[1], fields[2], fields[3]);

        if version.len() != 2 || version == INVALID_VERSION || !is_lower_hex(version) {
            return None;
        }
        // Version 00 is fully specified: a trailing field means the sender
        // disagrees about the layout, so the value is not trusted.
        if version == "00" && fields.len() != V0_FIELD_COUNT {
            return None;
        }
        if trace_id.len() != TRACE_ID_LEN || !is_lower_hex(trace_id) || is_all_zero(trace_id) {
            return None;
        }
        if span_id.len() != SPAN_ID_LEN || !is_lower_hex(span_id) || is_all_zero(span_id) {
            return None;
        }
        if flags.len() != 2 || !is_lower_hex(flags) {
            return None;
        }

        Some(Self {
            trace_id: trace_id.to_owned(),
            span_id: span_id.to_owned(),
        })
    }

    /// Reads the trace context from the process environment.
    ///
    /// [`TOKENLESS_TRACEPARENT_ENV`] wins when it holds a usable value; an
    /// empty or unparsable override falls through to
    /// [`STANDARD_TRACEPARENT_ENV`] so a typo cannot silently drop correlation
    /// for a whole session. This mirrors how an invalid `TOKENLESS_SLS_PATH`
    /// falls back to the default target.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Environment lookup injection point for [`Self::from_env`].
    ///
    /// Production always reads `std::env`; tests inject a map so precedence
    /// and fallback can be asserted without mutating the shared process
    /// environment from parallel test threads.
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        for key in [TOKENLESS_TRACEPARENT_ENV, STANDARD_TRACEPARENT_ENV] {
            let Some(value) = lookup(key) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            if let Some(context) = Self::parse(&value) {
                return Some(context);
            }
        }
        None
    }
}

/// Whether every byte is an ASCII lowercase hex digit.
///
/// Uppercase is rejected rather than normalized: the specification requires
/// lowercase on the wire, and accepting both would let two spellings of the
/// same trace reach the observability backend as distinct values.
fn is_lower_hex(value: &str) -> bool {
    value
        .as_bytes()
        .iter()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// Whether an id is the invalid all-zero value.
fn is_all_zero(value: &str) -> bool {
    value.bytes().all(|b| b == b'0')
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/trace_tests.rs");
}
