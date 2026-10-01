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

    #[test]
    fn every_operation_uses_its_wire_name() {
        for (op, wire) in [
            (GuestOp::Ping, "ping"),
            (GuestOp::Exec, "exec"),
            (GuestOp::Read, "read"),
            (GuestOp::Write, "write"),
        ] {
            assert_eq!(
                serde_json::to_value(op).expect("serialize op"),
                json!(wire),
                "wire name for {op:?}"
            );
            let parsed: GuestOp =
                serde_json::from_value(json!(wire)).expect("deserialize op by wire name");
            assert_eq!(parsed, op);
        }
        // Names outside the protocol must not deserialize into an operation.
        assert!(serde_json::from_value::<GuestOp>(json!("run")).is_err());
        assert!(serde_json::from_value::<GuestOp>(json!("EXEC")).is_err());
    }

    #[test]
    fn populated_exec_request_round_trips_every_field() {
        let mut request = GuestRequest::new("req-ümlaut".to_string(), GuestOp::Exec);
        request.cmd = Some("ls -la /workspace".to_string());
        request.cwd = Some("/workspace".to_string());
        request.env = Some(HashMap::from([("LANG".to_string(), "C.UTF-8".to_string())]));
        request.timeout = Some(30);

        let value = serde_json::to_value(&request).expect("serialize");
        assert_eq!(value["op"], "exec");
        assert_eq!(value["cmd"], "ls -la /workspace");
        assert_eq!(value["cwd"], "/workspace");
        assert_eq!(value["env"]["LANG"], "C.UTF-8");
        assert_eq!(value["timeout"], 30);

        let parsed: GuestRequest = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed.id, request.id);
        assert_eq!(parsed.op, GuestOp::Exec);
        assert_eq!(parsed.cmd, request.cmd);
        assert_eq!(parsed.cwd, request.cwd);
        assert_eq!(parsed.env, request.env);
        assert_eq!(parsed.timeout, request.timeout);
        assert!(parsed.path.is_none());
        assert!(parsed.data_b64.is_none());
    }

    #[test]
    fn write_request_carries_path_and_payload() {
        let mut request = GuestRequest::new("w1".to_string(), GuestOp::Write);
        request.path = Some("/etc/motd".to_string());
        request.data_b64 = Some("aGVsbG8=".to_string());

        let parsed: GuestRequest =
            serde_json::from_value(serde_json::to_value(&request).expect("serialize"))
                .expect("round trip");
        assert_eq!(parsed.path.as_deref(), Some("/etc/motd"));
        assert_eq!(parsed.data_b64.as_deref(), Some("aGVsbG8="));
        assert!(parsed.cmd.is_none(), "write must not inherit exec fields");
    }

    #[test]
    fn failure_response_carries_error_evidence() {
        let response: GuestResponse = serde_json::from_value(json!({
            "id": "request-9",
            "ok": false,
            "err": "command timed out after 30s",
            "rc": 124,
            "stderr_b64": "dGltZW91dA=="
        }))
        .expect("deserialize failure response");

        assert!(!response.ok);
        assert_eq!(response.err.as_deref(), Some("command timed out after 30s"));
        assert_eq!(response.rc, Some(124));
        assert_eq!(response.stdout_b64, None);
        assert_eq!(response.stderr_b64.as_deref(), Some("dGltZW91dA=="));
    }

    #[test]
    fn malformed_field_types_are_rejected() {
        // op must be a known operation name string.
        assert!(
            serde_json::from_value::<GuestRequest>(json!({
                "id": "r", "op": 7
            }))
            .is_err()
        );
        // timeout is a u32 number, not a string.
        assert!(
            serde_json::from_value::<GuestRequest>(json!({
                "id": "r", "op": "exec", "timeout": "30"
            }))
            .is_err()
        );
        // rc is an i32 number, not a boolean.
        assert!(
            serde_json::from_value::<GuestResponse>(json!({
                "id": "r", "ok": true, "rc": true
            }))
            .is_err()
        );
        // env entries must be string-to-string.
        assert!(
            serde_json::from_value::<GuestRequest>(json!({
                "id": "r", "op": "exec", "env": {"N": 3}
            }))
            .is_err()
        );
    }
}
