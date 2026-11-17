//! Strict sec-core V2 daemon wire protocol client backed by `aw_exec::uds`.

use aw_exec::uds::{self, UdsError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

const MAX_CONTEXT_BYTES: usize = 32 * 1024;
const MAX_BUSINESS_BYTES: usize = 4 * 1024 * 1024;
const MAX_REQUEST_FRAME_BYTES: usize = MAX_BUSINESS_BYTES + MAX_CONTEXT_BYTES;
const MAX_RESPONSE_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Version constant for `TraceCarrierV1` (`1`).
pub const TRACE_CARRIER_VERSION_V1: u8 = 1;
/// Version constant for `CompatibilityV1` (`1`).
pub const COMPATIBILITY_VERSION_V1: u8 = 1;

/// W3C TraceContext + Baggage carrier matching `asc_daemon_protocol::TraceCarrierV1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceCarrierV1 {
    /// Required carrier schema version (`1`).
    pub version: u8,
    /// Optional W3C traceparent header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    /// Optional W3C tracestate header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
    /// Optional W3C baggage header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baggage: Option<String>,
}

/// Legacy correlation carrier matching `asc_daemon_protocol::CompatibilityV1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompatibilityV1 {
    /// Required compatibility schema version (`1`).
    pub version: u8,
    /// Optional legacy trace identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Optional invocation label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_label: Option<String>,
}

/// Request envelope matching `asc_daemon_protocol::DaemonRequest` (`deny_unknown_fields`, no client `requestId`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonRequest {
    /// Daemon RPC method name (`action.code_scan`, `action.prompt_scan`, `action.pii_scan`).
    pub method: String,
    /// Method-specific parameters object.
    pub params: Value,
    /// Optional W3C trace carrier (`version: 1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<TraceCarrierV1>,
    /// Optional compatibility carrier (`version: 1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<CompatibilityV1>,
}

/// Success response envelope matching `asc_daemon_protocol::SuccessResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuccessResponse {
    /// Server-generated request correlation identifier.
    pub request_id: String,
    /// Method result payload.
    pub result: Value,
}

/// Error payload matching `asc_daemon_protocol::DaemonError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonError {
    /// Stable daemon error code.
    pub code: String,
    /// Daemon diagnostic message.
    pub message: String,
}

/// Error response envelope matching `asc_daemon_protocol::ErrorResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorResponse {
    /// Server-generated request correlation identifier.
    pub request_id: String,
    /// Structured daemon error details.
    pub error: DaemonError,
}

/// Untagged response envelope matching `asc_daemon_protocol::DaemonResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DaemonResponse {
    /// Successful daemon RPC response.
    Success(SuccessResponse),
    /// Failed daemon RPC response.
    Error(ErrorResponse),
}

fn is_supported_action_method(method: &str) -> bool {
    matches!(
        method,
        "action.code_scan" | "action.prompt_scan" | "action.pii_scan"
    )
}

/// Builds a strict V2 `DaemonRequest` for one supported action method from params and AW trace context.
///
/// # Errors
/// Returns a static error code when the method is unsupported, parameters are not an object,
/// or fields exceed protocol limits.
pub fn build_request(
    method: &str,
    params: Value,
    trace_context: &Value,
) -> Result<DaemonRequest, &'static str> {
    if !is_supported_action_method(method) || !params.is_object() {
        return Err("invalid_daemon_request");
    }
    let params_len = serde_json::to_vec(&params)
        .map_err(|_| "invalid_daemon_request")?
        .len();
    if params_len > MAX_BUSINESS_BYTES {
        return Err("daemon_frame_limit");
    }
    let (trace_context, compatibility) = build_trace_and_compat(trace_context)?;
    Ok(DaemonRequest {
        method: method.to_owned(),
        params,
        trace_context,
        compatibility,
    })
}

/// Decodes and validates a single-line V2 `DaemonResponse` payload.
///
/// # Errors
/// Returns `"invalid_daemon_response"` on malformed JSON/empty `requestId` or `"daemon_failed"` on daemon errors.
pub fn decode_response(line: &[u8]) -> Result<Value, &'static str> {
    let response: DaemonResponse =
        serde_json::from_slice(line).map_err(|_| "invalid_daemon_response")?;
    match response {
        DaemonResponse::Success(ok) => {
            if ok.request_id.trim().is_empty() {
                return Err("invalid_daemon_response");
            }
            Ok(ok.result)
        }
        DaemonResponse::Error(err) => {
            if err.request_id.trim().is_empty()
                || err.error.code.trim().is_empty()
                || err.error.message.trim().is_empty()
            {
                return Err("invalid_daemon_response");
            }
            Err("daemon_failed")
        }
    }
}

/// Encodes `request` and enforces `asc_daemon_protocol::trace::request_fits_budget`
/// per-bucket limits (`context <= 32 KiB`, `business <= 4 MiB`, `total <= 4 MiB + 32 KiB`).
///
/// # Errors
/// Returns `"invalid_daemon_request"` on serialization failure or `"daemon_frame_limit"`
/// when either budget bucket or the combined request line limit is exceeded.
pub fn encode_bounded_request(request: &DaemonRequest) -> Result<(Vec<u8>, usize), &'static str> {
    if !is_supported_action_method(&request.method) || !request.params.is_object() {
        return Err("invalid_daemon_request");
    }
    let encoded = serde_json::to_vec(request).map_err(|_| "invalid_daemon_request")?;
    let mut context_bytes = 0usize;
    if let Some(tc) = &request.trace_context {
        let val_len = serde_json::to_vec(tc)
            .map_err(|_| "invalid_daemon_request")?
            .len();
        context_bytes = context_bytes.saturating_add(r#""traceContext":"#.len() + val_len + 1);
    }
    if let Some(compat) = &request.compatibility {
        let val_len = serde_json::to_vec(compat)
            .map_err(|_| "invalid_daemon_request")?
            .len();
        context_bytes = context_bytes.saturating_add(r#""compatibility":"#.len() + val_len + 1);
    }
    let total_bytes = encoded.len().saturating_add(1);
    let business_bytes = total_bytes.saturating_sub(context_bytes);
    if context_bytes > MAX_CONTEXT_BYTES
        || business_bytes > MAX_BUSINESS_BYTES
        || total_bytes > MAX_REQUEST_FRAME_BYTES
    {
        return Err("daemon_frame_limit");
    }
    Ok((encoded, business_bytes))
}

/// Executes a single-shot request/response exchange against the sec-core daemon socket.
///
/// # Errors
/// Returns a content-free static error code on timeout, peer UID rejection, framing, or daemon error.
pub fn call(
    socket: &Path,
    method: &str,
    params: Value,
    trace_context: &Value,
    timeout: Duration,
) -> Result<Value, &'static str> {
    if timeout.as_millis() == 0 {
        return Err("deadline_exceeded");
    }
    let request = build_request(method, params, trace_context)?;
    let (encoded, _) = encode_bounded_request(&request)?;
    let line = uds::exchange(
        socket,
        &encoded,
        timeout,
        expected_daemon_uid(),
        MAX_RESPONSE_FRAME_BYTES,
    )
    .map_err(|err| match err {
        UdsError::Timeout => "deadline_exceeded",
        UdsError::PeerRejected => "daemon_peer_auth_failed",
        UdsError::ResponseTooLarge => "daemon_response_limit",
        UdsError::InvalidTimeout => "invalid_daemon_request",
        UdsError::Unavailable | UdsError::InvalidFrame => "daemon_transport_error",
    })?;
    decode_response(&line)
}

#[cfg(not(feature = "testing"))]
const fn expected_daemon_uid() -> Option<u32> {
    Some(0)
}

#[cfg(feature = "testing")]
fn expected_daemon_uid() -> Option<u32> {
    if std::env::var_os("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK").is_some_and(|v| v == "1") {
        return None;
    }
    if let Ok(raw) = std::env::var("AW_SEC_CORE_TEST_DAEMON_UID") {
        if let Ok(uid) = raw.parse::<u32>() {
            return Some(uid);
        }
    }
    Some(0)
}

fn percent_encode_baggage(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let escape = *byte <= 0x1f
            || *byte >= 0x7f
            || matches!(*byte, b' ' | b'"' | b';' | b',' | b'=' | b'\\' | b'%');
        if escape {
            let _ = write!(out, "%{byte:02X}");
        } else {
            out.push(*byte as char);
        }
    }
    out
}

fn non_empty_str(val: Option<&Value>) -> Option<String> {
    val.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn build_trace_and_compat(
    trace_context: &Value,
) -> Result<(Option<TraceCarrierV1>, Option<CompatibilityV1>), &'static str> {
    let mut items = Vec::new();
    for (src_key, dst_key) in [
        ("agent_name", "agentsec.agent.name"),
        ("session_id", "agentsec.session.id"),
        ("run_id", "agentsec.run.id"),
        ("call_id", "agentsec.call.id"),
        ("tool_call_id", "agentsec.tool_call.id"),
    ] {
        if let Some(val) = non_empty_str(trace_context.get(src_key)) {
            items.push(format!("{dst_key}={}", percent_encode_baggage(&val)));
        }
    }
    let traceparent = non_empty_str(trace_context.get("traceparent"));
    let tracestate = non_empty_str(trace_context.get("tracestate"));
    let baggage = (!items.is_empty()).then(|| items.join(","));

    let trace = if traceparent.is_some() || tracestate.is_some() || baggage.is_some() {
        let carrier = TraceCarrierV1 {
            version: TRACE_CARRIER_VERSION_V1,
            traceparent,
            tracestate,
            baggage,
        };
        if serde_json::to_vec(&carrier)
            .map_err(|_| "invalid_daemon_request")?
            .len()
            > MAX_CONTEXT_BYTES
        {
            return Err("daemon_frame_limit");
        }
        Some(carrier)
    } else {
        None
    };

    let trace_id = non_empty_str(trace_context.get("trace_id"));
    let invocation_label = non_empty_str(trace_context.get("invocation_label"));
    let compat = if trace_id.is_some() || invocation_label.is_some() {
        Some(CompatibilityV1 {
            version: COMPATIBILITY_VERSION_V1,
            trace_id,
            invocation_label,
        })
    } else {
        None
    };

    Ok((trace, compat))
}
