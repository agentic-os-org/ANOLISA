// SPDX-License-Identifier: Apache-2.0
//! JSON-line client layered over Firecracker's vsock Unix socket proxy.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use blaze_core::guest_protocol::{
    DEFAULT_GUEST_PORT, DEFAULT_MAX_RESPONSE_BYTES, GuestOp, GuestRequest, GuestResponse,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{GuestError, Result};

const READY_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
const PROTOCOL_GRACE: Duration = Duration::from_secs(10);
const MAX_EXEC_COMMAND_BYTES: usize = 64 * 1024;
const MAX_EXEC_CWD_BYTES: usize = 4096;
const MAX_EXEC_ENV_ENTRIES: usize = 256;
const MAX_EXEC_ENV_BYTES: usize = 64 * 1024;

/// Result of one command executed by the guest agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestExecResult {
    /// Guest process exit status.
    pub exit_code: i32,
    /// Decoded stdout bytes.
    pub stdout: Vec<u8>,
    /// Decoded stderr bytes.
    pub stderr: Vec<u8>,
}

/// Client for one Firecracker guest agent.
#[derive(Debug, Clone)]
pub struct GuestClient {
    vsock_path: PathBuf,
    port: u32,
    io_timeout: Duration,
    max_response_bytes: usize,
    max_file_bytes: usize,
}

impl GuestClient {
    /// Create a client with production protocol defaults.
    pub fn new(vsock_path: PathBuf, io_timeout: Duration, max_file_bytes: usize) -> Self {
        Self {
            vsock_path,
            port: DEFAULT_GUEST_PORT,
            io_timeout,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_file_bytes,
        }
    }

    /// Override protocol limits for focused tests.
    #[cfg(test)]
    fn with_response_limit(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Check whether the guest agent is responsive.
    pub async fn ping(&self) -> Result<()> {
        let request = GuestRequest::new(Uuid::new_v4().to_string(), GuestOp::Ping);
        self.send_recv(&request).await?;
        Ok(())
    }

    /// Poll readiness with bounded exponential backoff.
    pub async fn wait_ready(
        &self,
        deadline: Duration,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let started = Instant::now();
        let mut backoff = Duration::from_millis(10);
        let mut last_error = None;
        while started.elapsed() < deadline {
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            let attempt_timeout = READY_ATTEMPT_TIMEOUT.min(remaining);
            tokio::select! {
                _ = cancellation.cancelled() => return Err(GuestError::Cancelled),
                result = tokio::time::timeout(attempt_timeout, self.ping()) => {
                    match result {
                        Ok(Ok(())) => return Ok(()),
                        Ok(Err(error)) => last_error = Some(error),
                        Err(_) => {
                            last_error = Some(GuestError::Timeout(format!(
                                "readiness ping exceeded {attempt_timeout:?}"
                            )));
                        }
                    }
                }
            }
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(GuestError::Cancelled),
                _ = tokio::time::sleep(backoff.min(remaining)) => {}
            }
            backoff = (backoff * 2).min(Duration::from_millis(250));
        }
        Err(GuestError::Timeout(format!(
            "guest at {} was not ready within {:?}: {}",
            self.vsock_path.display(),
            deadline,
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no attempt completed".to_string())
        )))
    }

    /// Execute one shell command in the guest.
    pub async fn exec(
        &self,
        command: String,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
        timeout_secs: u32,
    ) -> Result<GuestExecResult> {
        validate_exec_inputs(&command, cwd.as_deref(), env.as_ref())?;
        let mut request = GuestRequest::new(Uuid::new_v4().to_string(), GuestOp::Exec);
        request.cmd = Some(command);
        request.cwd = Some(cwd.unwrap_or_else(|| "/".to_string()));
        request.env = env;
        request.timeout = Some(timeout_secs);
        let timeout = operation_timeout(timeout_secs);
        if timeout > self.io_timeout {
            return Err(GuestError::InvalidArgument(format!(
                "exec timeout plus protocol grace ({timeout:?}) exceeds configured limit {:?}",
                self.io_timeout
            )));
        }
        let response = self.send_recv_with_timeout(&request, timeout).await?;
        let exit_code = response
            .rc
            .ok_or_else(|| {
                GuestError::Protocol("successful exec response is missing rc".to_string())
            })
            .map_err(|error| classify_after_request(GuestOp::Exec, error))?;
        let stdout = decode_limited(
            response.stdout_b64.as_deref().unwrap_or_default(),
            self.max_response_bytes,
        )
        .map_err(|error| classify_after_request(GuestOp::Exec, error))?;
        let stderr = decode_limited(
            response.stderr_b64.as_deref().unwrap_or_default(),
            self.max_response_bytes,
        )
        .map_err(|error| classify_after_request(GuestOp::Exec, error))?;
        Ok(GuestExecResult {
            exit_code,
            stdout,
            stderr,
        })
    }

    /// Read one guest file.
    pub async fn read_file(&self, path: String) -> Result<Vec<u8>> {
        validate_guest_path(&path)?;
        let mut request = GuestRequest::new(Uuid::new_v4().to_string(), GuestOp::Read);
        request.path = Some(path);
        let guest_timeout = request_timeout_secs(self.io_timeout);
        request.timeout = Some(guest_timeout);
        let response = self
            .send_recv_with_timeout(
                &request,
                operation_timeout(guest_timeout).min(self.io_timeout),
            )
            .await?;
        let data = response.data_b64.as_deref().ok_or_else(|| {
            GuestError::Protocol("successful read response is missing data_b64".to_string())
        })?;
        decode_limited(data, self.max_file_bytes)
    }

    /// Replace one guest file.
    pub async fn write_file(&self, path: String, data: &[u8]) -> Result<()> {
        validate_guest_path(&path)?;
        if data.len() > self.max_file_bytes {
            return Err(GuestError::PayloadTooLarge {
                actual: data.len(),
                limit: self.max_file_bytes,
            });
        }
        let mut request = GuestRequest::new(Uuid::new_v4().to_string(), GuestOp::Write);
        request.path = Some(path);
        request.data_b64 = Some(BASE64.encode(data));
        let guest_timeout = request_timeout_secs(self.io_timeout);
        request.timeout = Some(guest_timeout);
        self.send_recv_with_timeout(
            &request,
            operation_timeout(guest_timeout).min(self.io_timeout),
        )
        .await?;
        Ok(())
    }

    async fn send_recv(&self, request: &GuestRequest) -> Result<GuestResponse> {
        self.send_recv_with_timeout(request, self.io_timeout).await
    }

    async fn send_recv_with_timeout(
        &self,
        request: &GuestRequest,
        timeout: Duration,
    ) -> Result<GuestResponse> {
        let mut encoded = serde_json::to_vec(request)?;
        encoded.push(b'\n');

        let started = Instant::now();
        let mut stream = match tokio::time::timeout(timeout, self.connect_guest()).await {
            Ok(result) => result?,
            Err(_) => return Err(self.timeout_error(request, timeout, "before request delivery")),
        };
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(self.timeout_error(request, timeout, "before request delivery"));
        }

        match tokio::time::timeout(
            remaining,
            self.exchange_request(&mut stream, request, &encoded),
        )
        .await
        {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(classify_after_request(request.op, error)),
            Err(_) => Err(classify_after_request(
                request.op,
                self.timeout_error(request, timeout, "after request delivery began"),
            )),
        }
    }

    async fn connect_guest(&self) -> Result<UnixStream> {
        let mut stream = UnixStream::connect(&self.vsock_path).await?;
        stream
            .write_all(format!("CONNECT {}\n", self.port).as_bytes())
            .await?;
        let handshake = read_line(&mut stream, 128).await?;
        let handshake = std::str::from_utf8(&handshake).map_err(|error| {
            GuestError::Protocol(format!("CONNECT response is not UTF-8: {error}"))
        })?;
        let peer_cid = handshake
            .strip_prefix("OK ")
            .and_then(|value| value.parse::<u32>().ok());
        if peer_cid.is_none() {
            return Err(GuestError::Protocol(format!(
                "unexpected CONNECT {} response: expected \"OK <numeric-peer-cid>\", received {handshake:?}",
                self.port,
            )));
        }
        Ok(stream)
    }

    async fn exchange_request(
        &self,
        stream: &mut UnixStream,
        request: &GuestRequest,
        encoded: &[u8],
    ) -> Result<GuestResponse> {
        stream.write_all(encoded).await?;
        stream.flush().await?;
        let line = read_line(stream, self.max_response_bytes).await?;
        let response: GuestResponse = serde_json::from_slice(&line)?;
        if response.id != request.id {
            return Err(GuestError::Protocol(format!(
                "response id mismatch: sent {}, received {}",
                request.id, response.id
            )));
        }
        if !response.ok {
            return Err(GuestError::Rejected(response.err.unwrap_or_else(|| {
                "guest rejected request without an error".to_string()
            })));
        }
        Ok(response)
    }

    fn timeout_error(&self, request: &GuestRequest, timeout: Duration, phase: &str) -> GuestError {
        GuestError::Timeout(format!(
            "{:?} request to {} exceeded {:?} {phase}",
            request.op,
            self.vsock_path.display(),
            timeout
        ))
    }
}

fn classify_after_request(operation: GuestOp, error: GuestError) -> GuestError {
    if matches!(operation, GuestOp::Exec | GuestOp::Write)
        && !matches!(
            error,
            GuestError::Rejected(_) | GuestError::OutcomeUnknown(_)
        )
    {
        GuestError::OutcomeUnknown(error.to_string())
    } else {
        error
    }
}

fn operation_timeout(guest_timeout_secs: u32) -> Duration {
    Duration::from_secs(u64::from(guest_timeout_secs)).saturating_add(PROTOCOL_GRACE)
}

fn request_timeout_secs(io_timeout: Duration) -> u32 {
    io_timeout
        .saturating_sub(PROTOCOL_GRACE)
        .as_secs()
        .clamp(1, u64::from(u32::MAX)) as u32
}

async fn read_line<R>(stream: &mut R, limit: usize) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let bounded = limit.saturating_add(1);
    let mut reader = BufReader::new(stream).take(bounded as u64);
    let mut output = Vec::with_capacity(limit.min(8192));
    let count = reader.read_until(b'\n', &mut output).await?;
    if output.last() == Some(&b'\n') {
        output.pop();
        if output.len() <= limit {
            return Ok(output);
        }
    }
    if output.len() > limit {
        return Err(GuestError::ResponseTooLarge {
            actual: output.len(),
            limit,
        });
    }
    debug_assert_eq!(count, output.len());
    Err(GuestError::Protocol(
        "connection closed before newline delimiter".to_string(),
    ))
}

fn decode_limited(encoded: &str, limit: usize) -> Result<Vec<u8>> {
    let encoded_limit = limit.div_ceil(3).saturating_mul(4);
    if encoded.len() > encoded_limit {
        return Err(GuestError::ResponseTooLarge {
            actual: encoded.len(),
            limit: encoded_limit,
        });
    }
    let decoded = BASE64
        .decode(encoded)
        .map_err(|error| GuestError::Protocol(format!("invalid base64 payload: {error}")))?;
    if decoded.len() > limit {
        return Err(GuestError::ResponseTooLarge {
            actual: decoded.len(),
            limit,
        });
    }
    Ok(decoded)
}

fn validate_exec_inputs(
    command: &str,
    cwd: Option<&str>,
    env: Option<&HashMap<String, String>>,
) -> Result<()> {
    if command.is_empty() {
        return Err(GuestError::InvalidArgument(
            "exec command is empty".to_string(),
        ));
    }
    if command.len() > MAX_EXEC_COMMAND_BYTES || command.contains('\0') {
        return Err(GuestError::InvalidArgument(format!(
            "exec command must be NUL-free and at most {MAX_EXEC_COMMAND_BYTES} bytes"
        )));
    }
    if let Some(cwd) = cwd
        && (cwd.len() > MAX_EXEC_CWD_BYTES || cwd.contains('\0'))
    {
        return Err(GuestError::InvalidArgument(format!(
            "exec cwd must be NUL-free and at most {MAX_EXEC_CWD_BYTES} bytes"
        )));
    }
    if let Some(env) = env {
        if env.len() > MAX_EXEC_ENV_ENTRIES {
            return Err(GuestError::InvalidArgument(format!(
                "exec environment has {} entries; limit is {MAX_EXEC_ENV_ENTRIES}",
                env.len()
            )));
        }
        let mut total = 0_usize;
        for (key, value) in env {
            if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
                return Err(GuestError::InvalidArgument(
                    "exec environment keys must be non-empty and contain no '=', and keys and \
                     values must be NUL-free"
                        .to_string(),
                ));
            }
            total = total
                .checked_add(key.len())
                .and_then(|bytes| bytes.checked_add(value.len()))
                .ok_or_else(|| {
                    GuestError::InvalidArgument("exec environment size overflow".to_string())
                })?;
            if total > MAX_EXEC_ENV_BYTES {
                return Err(GuestError::InvalidArgument(format!(
                    "exec environment is {total} bytes; limit is {MAX_EXEC_ENV_BYTES}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_guest_path(path: &str) -> Result<()> {
    if path.is_empty() || !path.starts_with('/') || path.len() > 4096 || path.contains('\0') {
        return Err(GuestError::InvalidArgument(
            "guest file path must be absolute, NUL-free, and at most 4096 bytes".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use tokio::net::UnixListener;

    use super::*;

    async fn spawn_server(
        socket: PathBuf,
        response: Arc<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>,
    ) {
        let listener = UnixListener::bind(socket).expect("bind");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let response = response.clone();
                tokio::spawn(async move {
                    let connect = read_line(&mut stream, 128).await.expect("connect");
                    assert_eq!(connect, b"CONNECT 5000");
                    stream.write_all(b"OK 1073742006\n").await.expect("ok");
                    let request = read_line(&mut stream, 4096).await.expect("request");
                    let request: serde_json::Value =
                        serde_json::from_slice(&request).expect("json");
                    let response = response(request);
                    let mut bytes = serde_json::to_vec(&response).expect("encode");
                    bytes.push(b'\n');
                    stream.write_all(&bytes).await.expect("write");
                });
            }
        });
    }

    async fn accept_request(listener: &UnixListener) -> (UnixStream, serde_json::Value) {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let connect = read_line(&mut stream, 128).await.expect("connect");
        assert_eq!(connect, b"CONNECT 5000");
        stream.write_all(b"OK 1073742006\n").await.expect("ok");
        let request = read_line(&mut stream, 4096).await.expect("request");
        let request = serde_json::from_slice(&request).expect("json");
        (stream, request)
    }

    #[test]
    fn side_effect_errors_become_unknown_only_after_delivery_starts() {
        for operation in [GuestOp::Exec, GuestOp::Write] {
            assert!(matches!(
                classify_after_request(operation, GuestError::Protocol("EOF".into())),
                GuestError::OutcomeUnknown(_)
            ));
            assert!(matches!(
                classify_after_request(
                    operation,
                    GuestError::ResponseTooLarge {
                        actual: 5,
                        limit: 4,
                    },
                ),
                GuestError::OutcomeUnknown(_)
            ));
            assert!(matches!(
                classify_after_request(operation, GuestError::Rejected("denied".into())),
                GuestError::Rejected(_)
            ));
        }

        assert!(matches!(
            classify_after_request(GuestOp::Read, GuestError::Protocol("EOF".into())),
            GuestError::Protocol(_)
        ));
        assert!(matches!(
            classify_after_request(
                GuestOp::Read,
                GuestError::ResponseTooLarge {
                    actual: 5,
                    limit: 4,
                },
            ),
            GuestError::ResponseTooLarge { .. }
        ));
    }

    #[tokio::test]
    async fn ping_exec_read_and_write_follow_existing_protocol() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        spawn_server(
            socket.clone(),
            Arc::new(move |request| {
                server_requests.fetch_add(1, Ordering::Relaxed);
                match request["op"].as_str().expect("op") {
                    "exec" => json!({
                        "id": request["id"],
                        "ok": true,
                        "rc": 7,
                        "stdout_b64": BASE64.encode(b"out"),
                        "stderr_b64": BASE64.encode(b"err")
                    }),
                    "read" => {
                        assert_eq!(request["timeout"], 5);
                        json!({
                            "id": request["id"],
                            "ok": true,
                            "data_b64": BASE64.encode(b"data")
                        })
                    }
                    "write" => {
                        assert_eq!(request["timeout"], 5);
                        json!({"id": request["id"], "ok": true})
                    }
                    _ => json!({"id": request["id"], "ok": true}),
                }
            }),
        )
        .await;
        let client = GuestClient::new(socket, Duration::from_secs(15), 1024);
        client.ping().await.expect("ping");
        let exec = client
            .exec("exit 7".into(), None, None, 1)
            .await
            .expect("exec");
        assert_eq!(exec.exit_code, 7);
        assert_eq!(exec.stdout, b"out");
        assert_eq!(
            client.read_file("/tmp/x".into()).await.expect("read"),
            b"data"
        );
        client
            .write_file("/tmp/x".into(), b"replacement")
            .await
            .expect("write");
        assert_eq!(requests.load(Ordering::Relaxed), 4);
    }

    #[tokio::test]
    async fn mismatched_response_id_is_rejected() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        spawn_server(
            socket.clone(),
            Arc::new(|_| json!({"id": "wrong", "ok": true})),
        )
        .await;
        let error = GuestClient::new(socket, Duration::from_secs(1), 1024)
            .ping()
            .await
            .expect_err("mismatch");
        assert!(matches!(error, GuestError::Protocol(_)));
    }

    #[tokio::test]
    async fn malformed_json_is_rejected_without_poisoning_the_next_call() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = tokio::spawn(async move {
            let (mut first, _) = accept_request(&listener).await;
            first.write_all(b"{not-json}\n").await.expect("malformed");

            let (mut second, request) = accept_request(&listener).await;
            let mut response =
                serde_json::to_vec(&json!({"id": request["id"], "ok": true})).expect("response");
            response.push(b'\n');
            second.write_all(&response).await.expect("valid");
        });
        let client = GuestClient::new(socket, Duration::from_secs(1), 1024);
        assert!(matches!(client.ping().await, Err(GuestError::Json(_))));
        client.ping().await.expect("subsequent request");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn missing_socket_is_reported_as_connection_failure() {
        let temp = tempfile::tempdir().expect("temp");
        let client = GuestClient::new(
            temp.path().join("missing.uds"),
            Duration::from_millis(100),
            1024,
        );
        let error = client.ping().await.expect_err("connection failure");
        assert!(matches!(error, GuestError::Io(_)));

        let write_error = client
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("write did not reach the guest");
        assert!(matches!(write_error, GuestError::Io(_)));
    }

    #[tokio::test]
    async fn invalid_exec_timeout_is_a_caller_error() {
        let temp = tempfile::tempdir().expect("temp");
        let error = GuestClient::new(
            temp.path().join("missing.uds"),
            Duration::from_secs(15),
            1024,
        )
        .exec("true".into(), None, None, 6)
        .await
        .expect_err("timeout exceeds request budget");
        assert!(matches!(error, GuestError::InvalidArgument(_)));
        assert_eq!(
            crate::error::BlazeDaemonError::from(error).status_code(),
            400
        );
    }

    #[tokio::test]
    async fn connect_response_requires_a_numeric_peer_cid() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let connect = read_line(&mut stream, 128).await.expect("connect");
            assert_eq!(connect, b"CONNECT 5000");
            stream
                .write_all(b"OK not-a-cid\n")
                .await
                .expect("invalid peer cid");
        });
        let error = GuestClient::new(socket, Duration::from_secs(1), 1024)
            .ping()
            .await
            .expect_err("invalid peer cid");
        assert!(matches!(error, GuestError::Protocol(_)));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn guest_rejection_is_returned_without_panicking() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        spawn_server(
            socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": false, "err": "denied by guest"})),
        )
        .await;
        let error = GuestClient::new(socket, Duration::from_secs(1), 1024)
            .ping()
            .await
            .expect_err("rejected");
        assert!(matches!(error, GuestError::Rejected(message) if message == "denied by guest"));
    }

    #[tokio::test]
    async fn read_and_write_response_timeouts_are_bounded() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = accept_request(&listener).await;
                tokio::spawn(async move {
                    let _stream = stream;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                });
            }
        });
        let client = GuestClient::new(socket, Duration::from_millis(30), 1024);
        let read_error = client
            .read_file("/tmp/x".into())
            .await
            .expect_err("read timeout");
        assert!(matches!(read_error, GuestError::Timeout(_)));
        let write_error = client
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("write timeout");
        assert!(matches!(write_error, GuestError::OutcomeUnknown(_)));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn mutating_handshake_timeout_is_retryable() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        let listener = UnixListener::bind(&socket).expect("bind");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let error = GuestClient::new(socket, Duration::from_millis(20), 1024)
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("handshake timeout");
        assert!(matches!(error, GuestError::Timeout(_)));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn mutating_response_corruption_has_unknown_outcome() {
        let temp = tempfile::tempdir().expect("temp");

        let malformed_socket = temp.path().join("malformed.uds");
        let malformed_listener = UnixListener::bind(&malformed_socket).expect("bind");
        tokio::spawn(async move {
            let (mut stream, _) = accept_request(&malformed_listener).await;
            stream.write_all(b"{not-json}\n").await.expect("response");
        });
        let malformed = GuestClient::new(malformed_socket, Duration::from_secs(15), 1024)
            .exec("true".into(), None, None, 1)
            .await
            .expect_err("malformed response");
        assert!(matches!(malformed, GuestError::OutcomeUnknown(_)));

        let mismatch_socket = temp.path().join("mismatch.uds");
        spawn_server(
            mismatch_socket.clone(),
            Arc::new(|_| json!({"id": "wrong", "ok": true})),
        )
        .await;
        let mismatch = GuestClient::new(mismatch_socket, Duration::from_secs(1), 1024)
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("mismatched response");
        assert!(matches!(mismatch, GuestError::OutcomeUnknown(_)));

        let missing_ok_socket = temp.path().join("missing-ok.uds");
        spawn_server(
            missing_ok_socket.clone(),
            Arc::new(|request| json!({"id": request["id"]})),
        )
        .await;
        let missing_ok = GuestClient::new(missing_ok_socket, Duration::from_secs(1), 1024)
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("missing outcome flag");
        assert!(matches!(missing_ok, GuestError::OutcomeUnknown(_)));

        let eof_socket = temp.path().join("eof.uds");
        let eof_listener = UnixListener::bind(&eof_socket).expect("bind");
        tokio::spawn(async move {
            let (_stream, _) = accept_request(&eof_listener).await;
        });
        let eof = GuestClient::new(eof_socket, Duration::from_secs(1), 1024)
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("response EOF");
        assert!(matches!(eof, GuestError::OutcomeUnknown(_)));

        let oversized_socket = temp.path().join("oversized-write.uds");
        spawn_server(
            oversized_socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": true, "padding": "xxxxxxxx"})),
        )
        .await;
        let oversized = GuestClient::new(oversized_socket, Duration::from_secs(1), 1024)
            .with_response_limit(4)
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("oversized response");
        assert!(matches!(oversized, GuestError::OutcomeUnknown(_)));
    }

    #[tokio::test]
    async fn explicit_mutating_rejection_has_known_outcome() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("rejected.uds");
        spawn_server(
            socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": false, "err": "write denied"})),
        )
        .await;

        let error = GuestClient::new(socket, Duration::from_secs(1), 1024)
            .write_file("/tmp/x".into(), b"value")
            .await
            .expect_err("guest rejection");
        assert!(matches!(error, GuestError::Rejected(message) if message == "write denied"));
    }

    #[tokio::test]
    async fn successful_responses_require_operation_fields() {
        let temp = tempfile::tempdir().expect("temp");
        let exec_socket = temp.path().join("exec.uds");
        spawn_server(
            exec_socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": true})),
        )
        .await;
        let exec = GuestClient::new(exec_socket, Duration::from_secs(15), 1024)
            .exec("true".into(), None, None, 1)
            .await
            .expect_err("missing rc");
        assert!(
            matches!(exec, GuestError::OutcomeUnknown(message) if message.contains("missing rc"))
        );

        let read_socket = temp.path().join("read.uds");
        spawn_server(
            read_socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": true})),
        )
        .await;
        let read = GuestClient::new(read_socket, Duration::from_secs(1), 1024)
            .read_file("/tmp/x".into())
            .await
            .expect_err("missing data");
        assert!(
            matches!(read, GuestError::Protocol(message) if message.contains("missing data_b64"))
        );
    }

    #[tokio::test]
    async fn exec_inputs_are_bounded_before_connecting() {
        let temp = tempfile::tempdir().expect("temp");
        let client = GuestClient::new(
            temp.path().join("missing.uds"),
            Duration::from_secs(15),
            1024,
        );
        let command = "x".repeat(MAX_EXEC_COMMAND_BYTES + 1);
        assert!(matches!(
            client.exec(command, None, None, 1).await,
            Err(GuestError::InvalidArgument(_))
        ));

        let mut env = HashMap::new();
        env.insert("KEY".to_string(), "x".repeat(MAX_EXEC_ENV_BYTES));
        assert!(matches!(
            client.exec("true".into(), None, Some(env), 1).await,
            Err(GuestError::InvalidArgument(_))
        ));
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        spawn_server(
            socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": true, "data_b64": "AAAAAAAA"})),
        )
        .await;
        let error = GuestClient::new(socket, Duration::from_secs(1), 1024)
            .with_response_limit(4)
            .read_file("/tmp/x".into())
            .await
            .expect_err("oversized line");
        assert!(matches!(error, GuestError::ResponseTooLarge { .. }));
    }

    #[tokio::test]
    async fn line_reader_accepts_the_limit_and_rejects_one_more_byte() {
        let (mut exact_reader, mut exact_writer) = tokio::io::duplex(64);
        tokio::spawn(async move {
            exact_writer
                .write_all(b"1234\n")
                .await
                .expect("write exact");
        });
        assert_eq!(
            read_line(&mut exact_reader, 4).await.expect("exact limit"),
            b"1234"
        );

        let (mut oversized_reader, mut oversized_writer) = tokio::io::duplex(64);
        tokio::spawn(async move {
            oversized_writer
                .write_all(b"12345\n")
                .await
                .expect("write oversized");
        });
        assert!(matches!(
            read_line(&mut oversized_reader, 4)
                .await
                .expect_err("one byte over"),
            GuestError::ResponseTooLarge {
                actual: 5,
                limit: 4
            }
        ));
    }

    #[tokio::test]
    async fn invalid_and_decoded_oversized_base64_are_rejected() {
        let temp = tempfile::tempdir().expect("temp");
        let invalid_socket = temp.path().join("invalid.uds");
        spawn_server(
            invalid_socket.clone(),
            Arc::new(|request| json!({"id": request["id"], "ok": true, "data_b64": "not/base64!"})),
        )
        .await;
        let invalid = GuestClient::new(invalid_socket, Duration::from_secs(1), 16)
            .read_file("/tmp/x".into())
            .await
            .expect_err("invalid base64");
        assert!(matches!(invalid, GuestError::Protocol(_)));

        let oversized_socket = temp.path().join("oversized.uds");
        spawn_server(
            oversized_socket.clone(),
            Arc::new(|request| {
                json!({
                    "id": request["id"],
                    "ok": true,
                    "data_b64": BASE64.encode(b"12345")
                })
            }),
        )
        .await;
        let oversized = GuestClient::new(oversized_socket, Duration::from_secs(1), 4)
            .read_file("/tmp/x".into())
            .await
            .expect_err("decoded limit");
        assert!(matches!(
            oversized,
            GuestError::ResponseTooLarge {
                actual: 5,
                limit: 4
            }
        ));
    }

    #[tokio::test]
    async fn oversized_write_is_rejected_before_connecting() {
        let temp = tempfile::tempdir().expect("temp");
        let error = GuestClient::new(temp.path().join("missing.uds"), Duration::from_secs(1), 4)
            .write_file("/tmp/x".into(), b"12345")
            .await
            .expect_err("write limit");
        assert!(matches!(
            error,
            GuestError::PayloadTooLarge {
                actual: 5,
                limit: 4
            }
        ));
    }

    #[tokio::test]
    async fn wait_ready_honors_cancellation() {
        let temp = tempfile::tempdir().expect("temp");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = GuestClient::new(
            temp.path().join("missing.uds"),
            Duration::from_millis(10),
            1024,
        )
        .wait_ready(Duration::from_secs(1), &cancellation)
        .await
        .expect_err("cancelled");
        assert!(matches!(error, GuestError::Cancelled));
    }

    #[tokio::test]
    async fn wait_ready_stops_at_its_deadline() {
        let temp = tempfile::tempdir().expect("temp");
        let started = Instant::now();
        let error = GuestClient::new(
            temp.path().join("missing.uds"),
            Duration::from_secs(1),
            1024,
        )
        .wait_ready(Duration::from_millis(60), &CancellationToken::new())
        .await
        .expect_err("deadline");
        assert!(matches!(error, GuestError::Timeout(_)));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn wait_ready_retries_after_one_stalled_connection() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("vsock.uds");
        let listener = UnixListener::bind(&socket).expect("bind");
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = attempts.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let attempt = server_attempts.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(async move {
                    if attempt == 0 {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        return;
                    }
                    let connect = read_line(&mut stream, 128).await.expect("connect");
                    assert_eq!(connect, b"CONNECT 5000");
                    stream.write_all(b"OK 5000\n").await.expect("ok");
                    let request = read_line(&mut stream, 4096).await.expect("request");
                    let request: serde_json::Value =
                        serde_json::from_slice(&request).expect("json");
                    let response = json!({"id": request["id"], "ok": true});
                    let mut bytes = serde_json::to_vec(&response).expect("encode");
                    bytes.push(b'\n');
                    stream.write_all(&bytes).await.expect("write");
                });
            }
        });

        GuestClient::new(socket, Duration::from_secs(5), 1024)
            .wait_ready(Duration::from_secs(1), &CancellationToken::new())
            .await
            .expect("second readiness attempt");
        assert!(attempts.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn guest_path_validation_does_not_treat_guest_paths_as_host_paths() {
        assert!(validate_guest_path("/tmp/../etc/hosts").is_ok());
        assert!(matches!(
            validate_guest_path("relative"),
            Err(GuestError::InvalidArgument(_))
        ));
    }
}
