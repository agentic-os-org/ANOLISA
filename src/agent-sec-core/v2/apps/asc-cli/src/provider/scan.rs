//! Translate daemon failures and scanner verdicts without leaking raw evidence.

use std::path::Path;
use std::time::Duration;

use asc_daemon_protocol::{CodeScanParams, DaemonRequest, DaemonResponse, method};
use serde_json::Value;

use super::config::Language;

pub(super) enum Verdict {
    Pass,
    Risk,
}

pub(super) fn call(
    socket: &Path,
    code: &str,
    language: Language,
    timeout: Duration,
) -> Result<Verdict, &'static str> {
    let request = DaemonRequest {
        trace_context: None,
        compatibility: None,
        method: method::ACTION_CODE_SCAN.to_owned(),
        params: serde_json::to_value(CodeScanParams {
            code: code.to_owned(),
            language: language.as_str().to_owned(),
            rules: None,
            mode: Some("regex".to_owned()),
        })
        .map_err(|_| "invalid_tool_input")?,
    };
    let response =
        asc_daemon_client::call(socket, &request, timeout).map_err(|error| match error {
            asc_daemon_client::ClientError::Timeout { .. } => "deadline_exceeded",
            _ => "daemon_transport_error",
        })?;
    match response {
        DaemonResponse::Error(_) => Err("daemon_error"),
        DaemonResponse::Success(response) => verdict(&response.result),
    }
}

fn verdict(result: &Value) -> Result<Verdict, &'static str> {
    match (result["ok"].as_bool(), result["verdict"].as_str()) {
        (Some(false), Some("error")) => Err("scan_error"),
        (Some(true), Some("pass")) => Ok(Verdict::Pass),
        // Matches the existing explicit enable_block threshold: warn and deny.
        (Some(true), Some("warn" | "deny")) => Ok(Verdict::Risk),
        _ => Err("invalid_scan_result"),
    }
}
