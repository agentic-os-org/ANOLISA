//! Unix socket NDJSON client for the agent-sec daemon.

use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const SOCKET_ENV: &str = "AGENT_SEC_DAEMON_SOCKET";
const RUNTIME_SUBDIR: &str = "agent-sec-core";
const SOCKET_FILENAME: &str = "daemon.sock";
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct AgentSecClient {
    socket_path: PathBuf,
    timeout_ms: u64,
    timeout: Duration,
    max_response_bytes: usize,
}

impl AgentSecClient {
    pub fn new(socket_path: Option<PathBuf>) -> Result<Self, AgentSecClientError> {
        Self::with_timeout(socket_path, DEFAULT_TIMEOUT_MS)
    }

    pub fn with_timeout(
        socket_path: Option<PathBuf>,
        timeout_ms: u64,
    ) -> Result<Self, AgentSecClientError> {
        if timeout_ms == 0 {
            return Err(AgentSecClientError::Protocol(
                "agent-sec daemon timeout must be positive".to_string(),
            ));
        }

        Ok(Self {
            socket_path: resolve_socket_path(socket_path)?,
            timeout_ms,
            timeout: Duration::from_millis(timeout_ms),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        })
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub fn call(&self, method: &str, params: Value) -> Result<DaemonResponse, AgentSecClientError> {
        let payload = self.build_request_payload(method, params)?;

        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|err| classify_io_error("connect", err))?;
        self.send_payload_and_read_response(&mut stream, &payload)
    }

    fn send_payload_and_read_response(
        &self,
        stream: &mut UnixStream,
        payload: &[u8],
    ) -> Result<DaemonResponse, AgentSecClientError> {
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|err| classify_io_error("set read timeout", err))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|err| classify_io_error("set write timeout", err))?;
        stream
            .write_all(payload)
            .map_err(|err| classify_io_error("write request", err))?;

        let response_line = self.read_response_line(stream)?;
        self.decode_response_line(&response_line)
    }

    fn build_request_payload(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Vec<u8>, AgentSecClientError> {
        if !params.is_object() {
            return Err(AgentSecClientError::Protocol(
                "daemon params must be a JSON object".to_string(),
            ));
        }

        let request = json!({
            "method": method,
            "params": params,
            "trace_context": {},
            "caller": "agentsight",
            "timeout_ms": self.timeout_ms,
        });

        let mut payload = serde_json::to_vec(&request).map_err(|err| {
            AgentSecClientError::Protocol(format!("failed to encode daemon request: {err}"))
        })?;
        payload.push(b'\n');

        Ok(payload)
    }

    fn read_response_line<R: Read>(&self, reader: &mut R) -> Result<Vec<u8>, AgentSecClientError> {
        let mut chunks = Vec::new();
        let mut buffer = [0_u8; 4096];

        loop {
            let n = reader
                .read(&mut buffer)
                .map_err(|err| classify_io_error("read response", err))?;
            if n == 0 {
                break;
            }

            chunks.extend_from_slice(&buffer[..n]);
            if chunks.len() > self.max_response_bytes {
                return Err(AgentSecClientError::ResponseTooLarge(
                    self.max_response_bytes,
                ));
            }
            if let Some(newline) = chunks.iter().position(|byte| *byte == b'\n') {
                chunks.truncate(newline);
                break;
            }
        }

        if chunks.is_empty() {
            return Err(AgentSecClientError::Transport(
                "daemon returned an empty response".to_string(),
            ));
        }

        Ok(chunks)
    }

    fn decode_response_line(
        &self,
        response_line: &[u8],
    ) -> Result<DaemonResponse, AgentSecClientError> {
        serde_json::from_slice(response_line).map_err(|err| {
            AgentSecClientError::Protocol(format!("daemon returned invalid JSON: {err}"))
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DaemonResponse {
    pub request_id: String,
    pub ok: bool,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i64,
    #[serde(default)]
    pub error: Option<DaemonErrorPayload>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DaemonErrorPayload {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum AgentSecClientError {
    #[error("agent-sec daemon socket path could not be resolved: {0}")]
    SocketPath(String),
    #[error("agent-sec daemon is unavailable: {0}")]
    Transport(String),
    #[error("agent-sec daemon request timed out: {0}")]
    Timeout(String),
    #[error("agent-sec daemon response exceeds {0} bytes")]
    ResponseTooLarge(usize),
    #[error("agent-sec daemon protocol error: {0}")]
    Protocol(String),
}

fn resolve_socket_path(socket_path: Option<PathBuf>) -> Result<PathBuf, AgentSecClientError> {
    if let Some(path) = socket_path {
        return Ok(path);
    }

    if let Some(path) = env::var_os(SOCKET_ENV) {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    Ok(resolve_socket_path_with_runtime_dir(
        env::var_os("XDG_RUNTIME_DIR"),
        unsafe { libc::getuid() },
    ))
}

fn resolve_socket_path_with_runtime_dir(
    xdg_runtime_dir: Option<std::ffi::OsString>,
    uid: libc::uid_t,
) -> PathBuf {
    let runtime_dir = xdg_runtime_dir
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/user").join(uid.to_string()));

    runtime_dir.join(RUNTIME_SUBDIR).join(SOCKET_FILENAME)
}

fn classify_io_error(action: &str, err: std::io::Error) -> AgentSecClientError {
    match err.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            AgentSecClientError::Timeout(format!("{action}: {err}"))
        }
        _ => AgentSecClientError::Transport(format!("{action}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{AgentSecClient, AgentSecClientError};

    fn test_client(max_response_bytes: usize) -> AgentSecClient {
        AgentSecClient {
            socket_path: "/tmp/agent-sec-test.sock".into(),
            timeout_ms: 123,
            timeout: Duration::from_millis(123),
            max_response_bytes,
        }
    }

    #[test]
    fn call_rejects_non_object_params_before_connecting() {
        let client = AgentSecClient::new(Some("/tmp/agent-sec-test.sock".into()))
            .expect("client with explicit socket path should be created");

        let err = client
            .call("daemon.health", Value::String("bad".to_string()))
            .expect_err("non-object params should be rejected");

        assert!(
            matches!(err, AgentSecClientError::Protocol(message) if message.contains("params"))
        );
    }

    #[test]
    fn build_request_payload_serializes_daemon_ndjson() {
        let client = AgentSecClient::with_timeout(Some("/tmp/agent-sec-test.sock".into()), 123)
            .expect("client should be created");
        let payload = client
            .build_request_payload("sec.summary", json!({ "limit": 10 }))
            .expect("request payload should serialize");

        assert_eq!(payload.last(), Some(&b'\n'));

        let request: Value =
            serde_json::from_slice(&payload[..payload.len() - 1]).expect("payload should be JSON");
        assert_eq!(request["method"], "sec.summary");
        assert_eq!(request["caller"], "agentsight");
        assert_eq!(request["timeout_ms"], 123);
        assert_eq!(request["trace_context"], json!({}));
        assert_eq!(request["params"], json!({ "limit": 10 }));
    }

    #[test]
    fn with_timeout_rejects_zero_timeout() {
        let err = AgentSecClient::with_timeout(Some("/tmp/agent-sec-test.sock".into()), 0)
            .expect_err("zero timeout should be rejected");

        assert!(
            matches!(err, AgentSecClientError::Protocol(message) if message.contains("positive"))
        );
    }

    #[test]
    fn resolve_socket_path_uses_explicit_path() {
        let path = super::resolve_socket_path(Some("/tmp/agent-sec-test.sock".into()))
            .expect("explicit socket path should resolve");

        assert_eq!(path, std::path::PathBuf::from("/tmp/agent-sec-test.sock"));
    }

    #[test]
    fn resolve_socket_path_falls_back_to_run_user_uid_when_xdg_runtime_dir_missing() {
        let path = super::resolve_socket_path_with_runtime_dir(None, 1000);

        assert_eq!(
            path,
            std::path::PathBuf::from("/run/user/1000/agent-sec-core/daemon.sock")
        );
    }

    #[test]
    fn resolve_socket_path_uses_xdg_runtime_dir_when_present() {
        let path = super::resolve_socket_path_with_runtime_dir(
            Some(std::ffi::OsString::from("/tmp/runtime")),
            1000,
        );

        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/runtime/agent-sec-core/daemon.sock")
        );
    }

    #[test]
    fn read_response_line_reads_until_newline() {
        let client = test_client(1024);
        let mut reader = Cursor::new(
            br#"{"request_id":"req-1","ok":true}
ignored"#,
        );

        let line = client
            .read_response_line(&mut reader)
            .expect("response line should be read");

        assert_eq!(line, br#"{"request_id":"req-1","ok":true}"#);
    }

    #[test]
    fn decode_response_line_returns_daemon_json() {
        let client = test_client(1024);
        let response = client
            .decode_response_line(br#"{"request_id":"req-1","ok":true,"data":{"total":1}}"#)
            .expect("valid daemon response should parse");

        assert!(response.ok);
        assert_eq!(response.data, json!({ "total": 1 }));
    }

    #[test]
    fn decode_response_line_rejects_invalid_json() {
        let client = test_client(1024);

        let err = client
            .decode_response_line(b"not-json")
            .expect_err("invalid daemon JSON should fail");

        assert!(
            matches!(err, AgentSecClientError::Protocol(message) if message.contains("invalid JSON"))
        );
    }

    #[test]
    fn read_response_line_rejects_empty_response() {
        let client = test_client(1024);
        let mut reader = Cursor::new(Vec::<u8>::new());

        let err = client
            .read_response_line(&mut reader)
            .expect_err("empty daemon response should fail");

        assert!(
            matches!(err, AgentSecClientError::Transport(message) if message.contains("empty"))
        );
    }

    #[test]
    fn read_response_line_rejects_oversized_response() {
        let client = test_client(4);
        let mut reader = Cursor::new(b"12345".to_vec());

        let err = client
            .read_response_line(&mut reader)
            .expect_err("oversized daemon response should fail");

        assert!(matches!(err, AgentSecClientError::ResponseTooLarge(4)));
    }

    #[test]
    fn classify_io_error_maps_timeout_and_transport_errors() {
        let timeout = super::classify_io_error(
            "read response",
            std::io::Error::from(std::io::ErrorKind::TimedOut),
        );
        let transport = super::classify_io_error(
            "connect",
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        );

        assert!(matches!(timeout, AgentSecClientError::Timeout(_)));
        assert!(matches!(transport, AgentSecClientError::Transport(_)));
    }
}
