// SPDX-License-Identifier: Apache-2.0
//! Wire DTOs shared with a compatible sandbox guest agent.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Firecracker vsock port used by the compatible sandbox guest agent.
pub const DEFAULT_GUEST_PORT: u32 = 5000;

/// Maximum accepted JSON response line, excluding the newline delimiter.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Guest control protocol version understood by this Blaze build.
///
/// Operation availability is negotiated independently, so this value changes
/// only when framing or response semantics become incompatible.
pub const GUEST_PROTOCOL_VERSION: u32 = 1;

/// Guest operation names implemented by `sandbox-agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestOp {
    /// Check whether the guest agent can serve requests.
    Ping,
    /// Report the protocol version and supported operation names.
    Hello,
    /// Inject fresh host entropy and force the guest random generator to reseed.
    #[serde(rename = "reseed_rng")]
    ReseedRng,
    /// Run required guest initialization after restoring captured memory.
    #[serde(rename = "post_restore")]
    PostRestore,
    /// Synchronize guest-visible state before the host captures it.
    #[serde(rename = "prepare_hibernate")]
    PrepareHibernate,
    /// Execute one shell command.
    Exec,
    /// Read one guest file.
    Read,
    /// Replace one guest file.
    Write,
}

impl GuestOp {
    /// Return the stable operation name used on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Hello => "hello",
            Self::ReseedRng => "reseed_rng",
            Self::PostRestore => "post_restore",
            Self::PrepareHibernate => "prepare_hibernate",
            Self::Exec => "exec",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
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
    /// Standard-base64 host entropy for [`GuestOp::ReseedRng`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_b64: Option<String>,
    /// Host real time in Unix milliseconds for [`GuestOp::PostRestore`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_ts_ms: Option<i64>,
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
    /// Guest protocol version returned by [`GuestOp::Hello`].
    #[serde(default)]
    pub proto_version: Option<u32>,
    /// Supported operation names returned by [`GuestOp::Hello`].
    ///
    /// Strings remain untyped so newer agents can advertise unknown operations.
    #[serde(default)]
    pub ops: Option<Vec<String>>,
    /// Number of entropy bytes consumed by [`GuestOp::ReseedRng`].
    #[serde(default)]
    pub seed_bytes: Option<usize>,
    /// Whether [`GuestOp::ReseedRng`] forced the guest random generator to reseed.
    #[serde(default)]
    pub reseed: Option<bool>,
    /// Guest real time in Unix milliseconds after [`GuestOp::PostRestore`].
    #[serde(default)]
    pub ts_ms: Option<i64>,
    /// Host-minus-guest offset observed before clock correction.
    #[serde(default)]
    pub delta_ms: Option<i64>,
    /// Whether [`GuestOp::PostRestore`] stepped the guest real-time clock.
    #[serde(default)]
    pub clock_stepped: Option<bool>,
    /// Whether guest writes completed before [`GuestOp::PrepareHibernate`] replied.
    #[serde(default)]
    pub synced: Option<bool>,
    /// Whether the guest also dropped reclaimable caches before capture.
    #[serde(default)]
    pub drop_caches: Option<bool>,
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
            seed_b64: None,
            host_ts_ms: None,
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
        assert!(response.proto_version.is_none());
        assert!(response.ops.is_none());
        assert!(response.seed_bytes.is_none());
        assert!(response.reseed.is_none());
        assert!(response.ts_ms.is_none());
        assert!(response.delta_ms.is_none());
        assert!(response.clock_stepped.is_none());
        assert!(response.synced.is_none());
        assert!(response.drop_caches.is_none());
    }

    #[test]
    fn lifecycle_hooks_use_stable_wire_names() {
        let mut reseed = GuestRequest::new("request-1".to_string(), GuestOp::ReseedRng);
        reseed.seed_b64 = Some("AAECAw==".to_string());
        reseed.timeout = Some(10);
        assert_eq!(
            serde_json::to_value(reseed).expect("serialize reseed"),
            json!({
                "id": "request-1",
                "op": "reseed_rng",
                "timeout": 10,
                "seed_b64": "AAECAw==",
            })
        );

        let mut post_restore = GuestRequest::new("request-2".to_string(), GuestOp::PostRestore);
        post_restore.host_ts_ms = Some(1_787_808_000_123);
        post_restore.timeout = Some(10);
        assert_eq!(
            serde_json::to_value(post_restore).expect("serialize post restore"),
            json!({
                "id": "request-2",
                "op": "post_restore",
                "timeout": 10,
                "host_ts_ms": 1_787_808_000_123_i64,
            })
        );

        assert_eq!(GuestOp::Hello.as_str(), "hello");
        assert_eq!(GuestOp::PrepareHibernate.as_str(), "prepare_hibernate");
    }
}
