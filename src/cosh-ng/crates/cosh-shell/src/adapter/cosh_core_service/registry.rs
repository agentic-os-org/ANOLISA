//! Live registry command execution against the persistent cosh-core process.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::super::cosh_core_registry::{registry_timeout, RegistryQueryError};
use super::command::RegistryCommand;
use super::process::{send_json, PersistentProcess};

pub(super) fn log_registry_transport_failure(
    result: &Result<Value, RegistryQueryError>,
    command: &RegistryCommand,
) -> bool {
    if let Err(RegistryQueryError::Transport(error)) = result {
        // Dual-write: include the full error value so timeout, EOF, write
        // failure, and "unavailable" are distinguishable in post-mortem logs.
        tracing::warn!(
            domain = %command.domain,
            action = %command.action,
            error = %error,
            "live registry transport failed; resetting cosh-core process"
        );
        true
    } else {
        false
    }
}

pub(super) fn execute_registry(
    process: &mut PersistentProcess,
    command: &RegistryCommand,
) -> Result<Value, RegistryQueryError> {
    let request = serde_json::json!({
        "type": "registry_request",
        "request_id": command.request_id,
        "domain": command.domain,
        "action": command.action,
        "params": command.params,
    });
    send_json(&process.stdin, &request.to_string()).map_err(RegistryQueryError::Transport)?;
    let deadline = Instant::now() + registry_timeout(&command.domain, &command.action);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RegistryQueryError::Transport(
                "live registry query timed out".to_string(),
            ));
        }
        let line = match process.output_rx.recv_timeout(remaining) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => return Err(RegistryQueryError::Transport(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(RegistryQueryError::Transport(
                    "live registry query timed out".to_string(),
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(RegistryQueryError::Transport(
                    "cosh-core output stream disconnected".to_string(),
                ));
            }
        };
        let response: Value = match serde_json::from_str(line.trim()) {
            Ok(response) => response,
            Err(_) => continue,
        };
        // The service loop serializes Agent turns and registry commands through one stdout reader.
        // A line belongs to this command only when both its discriminator and correlation ID match.
        if !is_registry_response_for(&response, &command.request_id) {
            continue;
        }
        if response
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(response.get("data").cloned().unwrap_or(Value::Null));
        }
        return Err(RegistryQueryError::Response {
            message: response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown live registry error")
                .to_string(),
            code: response
                .get("data")
                .and_then(|data| data.get("error_code"))
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
}

pub(super) fn is_registry_response_for(response: &Value, request_id: &str) -> bool {
    response.get("type").and_then(Value::as_str) == Some("registry_response")
        && response.get("request_id").and_then(Value::as_str) == Some(request_id)
}
