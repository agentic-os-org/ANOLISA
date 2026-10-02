// SPDX-License-Identifier: Apache-2.0
//! Wire DTOs shared with a compatible sandbox guest agent.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Firecracker vsock port used by the compatible sandbox guest agent.
pub const DEFAULT_GUEST_PORT: u32 = 5000;

/// Maximum accepted JSON response line, excluding the newline delimiter.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Guest operation names implemented by `sandbox-agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestOp {
    /// Check whether the guest agent can serve requests.
    Ping,
    /// Execute one shell command.
    Exec,
    /// Read one guest file.
    Read,
    /// Replace one guest file.
    Write,
}

/// One newline-delimited request sent after the Firecracker CONNECT handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestRequest {
    /// Correlation identifier echoed by the guest.
    pub id: String,
    /// Requested guest operation.
    pub op: GuestOp,
    /// Shell command for [`GuestOp::Exec`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Working directory for command execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Environment additions for command execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Guest-side timeout in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    /// Guest path for file operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Standard-base64 file bytes for [`GuestOp::Write`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_b64: Option<String>,
}

/// One newline-delimited response returned by the compatible guest agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestResponse {
    /// Correlation identifier copied from the request.
    pub id: String,
    /// Whether the operation completed successfully.
    pub ok: bool,
    /// Guest error message when `ok` is false.
    #[serde(default)]
    pub err: Option<String>,
    /// Command exit status.
    #[serde(default)]
    pub rc: Option<i32>,
    /// Standard-base64 command stdout.
    #[serde(default)]
    pub stdout_b64: Option<String>,
    /// Standard-base64 command stderr.
    #[serde(default)]
    pub stderr_b64: Option<String>,
    /// Standard-base64 file bytes.
    #[serde(default)]
    pub data_b64: Option<String>,
}

impl GuestRequest {
    /// Build a request with operation-specific fields initially absent.
    pub fn new(id: String, op: GuestOp) -> Self {
        Self {
            id,
            op,
            cmd: None,
            cwd: None,
            env: None,
            timeout: None,
            path: None,
            data_b64: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn request_uses_wire_operation_names_and_omits_absent_fields() {
        let request = GuestRequest::new("request-1".to_string(), GuestOp::Exec);

        assert_eq!(
            serde_json::to_value(request).expect("serialize request"),
            json!({
                "id": "request-1",
                "op": "exec",
            })
        );
    }

    #[test]
    fn response_requires_outcome_and_defaults_optional_fields() {
        assert!(serde_json::from_value::<GuestResponse>(json!({"id": "request-1"})).is_err());
        assert!(serde_json::from_value::<GuestResponse>(json!({"ok": true})).is_err());
        let response: GuestResponse = serde_json::from_value(json!({
            "id": "request-1",
            "ok": true
        }))
        .expect("deserialize response");

        assert_eq!(response.id, "request-1");
        assert!(response.ok);
        assert!(response.err.is_none());
        assert!(response.rc.is_none());
        assert!(response.stdout_b64.is_none());
        assert!(response.stderr_b64.is_none());
        assert!(response.data_b64.is_none());
    }
}
