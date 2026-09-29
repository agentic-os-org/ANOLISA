use super::ecs_metadata::{classify_credentials, CredentialStatus, ProbeError};
use super::*;

use futures::FutureExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream as AsyncTcpStream};
use tokio::sync::oneshot;

const TEST_READ_TIMEOUT: Duration = Duration::from_secs(1);
const TEST_COMPLETION_BOUND: Duration = Duration::from_secs(5);
const TEST_WATCHDOG: Duration = Duration::from_secs(10);
const CHUNK_INTERVAL: Duration = Duration::from_millis(250);
const KEEPALIVE_CHUNKS: usize = 12;
const FIRST_EVENT: &str = "data: first\n\n";
const KEEPALIVE_EVENT: &str = "data: keep-alive\n\n";
const DONE_EVENT: &str = "data: [DONE]\n\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamingScenario {
    SilentBeforeHeaders,
    SilentAfterHeaders,
    SilentAfterFirstChunk,
    Healthy,
}

async fn write_sse_chunk(socket: &mut AsyncTcpStream, event: &str) -> std::io::Result<()> {
    let chunk = format!("{:x}\r\n{event}\r\n", event.len());
    socket.write_all(chunk.as_bytes()).await
}

async fn serve_streaming_scenario(
    listener: TcpListener,
    scenario: StreamingScenario,
    request_received: oneshot::Sender<()>,
) -> std::io::Result<()> {
    let (mut socket, _) = listener.accept().await?;
    socket.set_nodelay(true)?;
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let read = socket.read(&mut buffer).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed before sending request headers",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let _ = request_received.send(());

    if scenario != StreamingScenario::SilentBeforeHeaders {
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await?;
    }
    if matches!(
        scenario,
        StreamingScenario::SilentAfterFirstChunk | StreamingScenario::Healthy
    ) {
        write_sse_chunk(&mut socket, FIRST_EVENT).await?;
    }
    if scenario == StreamingScenario::Healthy {
        for _ in 0..KEEPALIVE_CHUNKS {
            tokio::time::sleep(CHUNK_INTERVAL).await;
            write_sse_chunk(&mut socket, KEEPALIVE_EVENT).await?;
        }
        write_sse_chunk(&mut socket, DONE_EVENT).await?;
        socket.write_all(b"0\r\n\r\n").await?;
        socket.shutdown().await?;
    } else {
        std::future::pending::<()>().await;
    }
    drop(socket);
    Ok(())
}

async fn check_streaming_scenario(scenario: StreamingScenario) {
    let mut server = None;
    let outcome = tokio::time::timeout(
        TEST_WATCHDOG,
        std::panic::AssertUnwindSafe(async {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind streaming fixture");
            let endpoint = endpoint::ResolvedEndpoint {
                host: listener.local_addr().expect("fixture address").to_string(),
                scheme: "http".to_string(),
                // Isolate this loopback fixture from host proxies; this is not a VPC probe.
                origin: endpoint::EndpointOrigin::VpcProxy,
            };
            let client =
                build_streaming_client(&endpoint, Duration::from_secs(3), TEST_READ_TIMEOUT)
                    .expect("build streaming client");
            let (request_received_tx, request_received_rx) = oneshot::channel();
            server = Some(tokio::spawn(serve_streaming_scenario(
                listener,
                scenario,
                request_received_tx,
            )));

            let started = std::time::Instant::now();
            let request = client.post(endpoint.base_url());
            let (received, response) = tokio::join!(request_received_rx, request.send());
            received.expect("fixture must accept the connection and receive request headers");

            if scenario == StreamingScenario::SilentBeforeHeaders {
                let error = response.expect_err("silent response headers must time out");
                assert!(error.is_timeout(), "expected a read timeout, got {error:?}");
            } else {
                let mut response = response.expect("receive SSE response headers");
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                assert_eq!(response.headers()["content-type"], "text/event-stream");

                if scenario == StreamingScenario::Healthy {
                    let body = response.text().await.expect("healthy SSE must finish");
                    let expected = format!(
                        "{FIRST_EVENT}{}{DONE_EVENT}",
                        KEEPALIVE_EVENT.repeat(KEEPALIVE_CHUNKS)
                    );
                    assert_eq!(body, expected);
                    assert!(
                        started.elapsed() > TEST_READ_TIMEOUT,
                        "healthy stream must outlive a total request timeout"
                    );
                } else {
                    if scenario == StreamingScenario::SilentAfterFirstChunk {
                        let mut first = Vec::new();
                        while first.len() < FIRST_EVENT.len() {
                            let chunk = response
                                .chunk()
                                .await
                                .expect("read the first SSE event")
                                .expect("first SSE event must not be EOF");
                            first.extend_from_slice(&chunk);
                        }
                        assert_eq!(first, FIRST_EVENT.as_bytes());
                    }
                    let error = response
                        .chunk()
                        .await
                        .expect_err("stalled SSE body must time out, not reach EOF");
                    assert!(error.is_timeout(), "expected a read timeout, got {error:?}");
                }
            }
            assert!(
                started.elapsed() < TEST_COMPLETION_BOUND,
                "{scenario:?} exceeded {TEST_COMPLETION_BOUND:?}"
            );
        })
        .catch_unwind(),
    )
    .await;

    // Reap the server even when an assertion panics or the watchdog expires.
    if let Some(server) = server {
        server.abort();
        match server.await {
            Ok(result) => result.expect("streaming fixture failed"),
            Err(error) => assert!(error.is_cancelled(), "fixture task failed: {error}"),
        }
    }
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(error) => panic!("{scenario:?} exceeded watchdog {TEST_WATCHDOG:?}: {error}"),
    }
}

#[tokio::test]
async fn streaming_client_bounds_a_silent_server() {
    check_streaming_scenario(StreamingScenario::SilentBeforeHeaders).await;
}

#[tokio::test]
async fn streaming_client_bounds_silence_after_sse_headers() {
    check_streaming_scenario(StreamingScenario::SilentAfterHeaders).await;
}

#[tokio::test]
async fn streaming_client_bounds_silence_after_first_chunk() {
    check_streaming_scenario(StreamingScenario::SilentAfterFirstChunk).await;
}

#[tokio::test]
async fn streaming_client_preserves_a_healthy_long_stream() {
    check_streaming_scenario(StreamingScenario::Healthy).await;
}

#[test]
fn region_id_strips_single_zone_suffix() {
    assert_eq!(
        region_id_from_zone_id("cn-hangzhou-j").as_deref(),
        Some("cn-hangzhou")
    );
    assert_eq!(
        region_id_from_zone_id("cn-beijing").as_deref(),
        Some("cn-beijing")
    );
    assert_eq!(region_id_from_zone_id(""), None);
}

#[test]
fn generate_console_url_uses_region_and_instance_id() {
    assert_eq!(
        generate_console_url("i-test123", "cn-hangzhou"),
        "https://alinux.console.aliyun.com/cn-hangzhou/guide/cosh?instance=i-test123"
    );
}

#[test]
fn build_request_preserves_user_provided_secrets() {
    let provider = SysomProvider {
        configured_endpoint: String::new(),
        credentials: std::sync::RwLock::new(SysomCredentials {
            access_key_id: "test-id".to_string(),
            access_key_secret: "test-secret".to_string(),
            security_token: None,
        }),
        is_sts: false,
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        instance_id: OnceCell::new(),
    };
    let secret = "short-provider-secret";
    let messages = vec![Message::user(&format!("api_key={secret}"))];

    let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());
    let payload = body.to_string();

    assert!(payload.contains(secret), "{payload}");
    assert!(!payload.contains("<redacted>"), "{payload}");
}

fn test_provider() -> SysomProvider {
    SysomProvider {
        configured_endpoint: String::new(),
        credentials: std::sync::RwLock::new(SysomCredentials {
            access_key_id: "test-id".to_string(),
            access_key_secret: "test-secret".to_string(),
            security_token: None,
        }),
        is_sts: false,
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        instance_id: OnceCell::new(),
    }
}

/// Parses the inner request the SysOM wrapper actually sends.
fn wire_inner(body: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(
        body["llmParamString"]
            .as_str()
            .expect("llmParamString is a string"),
    )
    .expect("inner request is JSON")
}

#[test]
fn extra_params_cannot_raise_the_wire_output_cap() {
    // Regression (#2240): the compaction budget reserves `O` from the same
    // resolver that produced `max_tokens`, so a wire request must never be
    // allowed to spend more than the reserve — not even via extra_params.
    let provider = test_provider();
    let config = GenerateConfig {
        max_tokens: 16_384,
        extra_params: Some(serde_json::json!({
            "max_tokens": 65_536u32,
            "max_completion_tokens": 65_536u32,
        })),
        ..GenerateConfig::default()
    };

    let inner = wire_inner(&provider.build_request_body(&[], &[], &config));

    assert_eq!(inner["max_tokens"], 16_384);
    assert_eq!(inner["max_completion_tokens"], 16_384);
}

#[test]
fn extra_params_may_still_lower_the_wire_output_cap() {
    // Asking for less than the reserve is always safe and is preserved.
    let provider = test_provider();
    let config = GenerateConfig {
        max_tokens: 16_384,
        extra_params: Some(serde_json::json!({"max_tokens": 512u32})),
        ..GenerateConfig::default()
    };

    let inner = wire_inner(&provider.build_request_body(&[], &[], &config));

    assert_eq!(inner["max_tokens"], 512);
}

fn ecs_metadata_now() -> chrono::DateTime<Utc> {
    "2026-09-15T12:00:00Z"
        .parse()
        .expect("fixed UTC test timestamp")
}

fn ecs_metadata_credentials() -> serde_json::Value {
    serde_json::json!({
        "Code": "Success",
        "AccessKeyId": "synthetic-ak",
        "AccessKeySecret": "synthetic-sk",
        "SecurityToken": "synthetic-token",
        "Expiration": "2026-09-15T13:00:00Z",
    })
}

#[test]
fn ecs_metadata_ready_for_complete_unexpired_credentials() {
    for expiration in ["2026-09-15T12:00:01Z", "2026-09-15T13:00:00Z"] {
        let mut body = ecs_metadata_credentials();
        body["Expiration"] = serde_json::json!(expiration);
        assert!(matches!(
            classify_credentials(200, body.to_string().as_bytes(), ecs_metadata_now()),
            Ok(CredentialStatus::Ready)
        ));
    }
}

#[test]
fn ecs_metadata_not_ready_for_missing_role() {
    for body in [
        ecs_metadata_credentials().to_string(),
        "not JSON".to_string(),
    ] {
        assert!(matches!(
            classify_credentials(404, body.as_bytes(), ecs_metadata_now()),
            Ok(CredentialStatus::NotReady(NotReadyReason::RoleMissing))
        ));
    }
}

#[test]
fn ecs_metadata_not_ready_at_or_after_expiration() {
    for expiration in ["2026-09-15T12:00:00Z", "2026-09-15T11:59:59Z"] {
        let mut body = ecs_metadata_credentials();
        body["Expiration"] = serde_json::json!(expiration);
        assert!(matches!(
            classify_credentials(200, body.to_string().as_bytes(), ecs_metadata_now()),
            Ok(CredentialStatus::NotReady(
                NotReadyReason::CredentialsExpired
            ))
        ));
    }
}

#[test]
fn ecs_metadata_classifies_http_errors_before_parsing_body() {
    for status in [401, 403, 500] {
        for body in [
            ecs_metadata_credentials().to_string(),
            "not JSON".to_string(),
        ] {
            assert!(
                matches!(
                    (
                        status,
                        classify_credentials(status, body.as_bytes(), ecs_metadata_now())
                    ),
                    (401 | 403, Err(ProbeError::AccessDenied)) | (500, Err(ProbeError::Http))
                ),
                "HTTP {status} must retain its error classification"
            );
        }
    }
}

#[test]
fn ecs_metadata_rejects_invalid_fields() {
    for field in [
        "Code",
        "AccessKeyId",
        "AccessKeySecret",
        "SecurityToken",
        "Expiration",
    ] {
        for (case, replacement) in [
            ("missing", None),
            ("null", Some(serde_json::json!(null))),
            ("empty", Some(serde_json::json!(""))),
            ("number", Some(serde_json::json!(42))),
            ("boolean", Some(serde_json::json!(true))),
            ("array", Some(serde_json::json!([]))),
            ("object", Some(serde_json::json!({}))),
        ] {
            let mut body = ecs_metadata_credentials();
            match replacement {
                Some(value) => body[field] = value,
                None => {
                    body.as_object_mut()
                        .expect("fixture is an object")
                        .remove(field);
                }
            }
            assert!(
                matches!(
                    classify_credentials(200, body.to_string().as_bytes(), ecs_metadata_now()),
                    Err(ProbeError::InvalidResponse)
                ),
                "{field}/{case} must be rejected"
            );
        }
    }
    for (field, value) in [
        ("Code", "SyntheticFailure"),
        ("Code", "success"),
        ("Expiration", "not-a-timestamp"),
    ] {
        let mut body = ecs_metadata_credentials();
        body[field] = serde_json::json!(value);
        assert!(
            matches!(
                classify_credentials(200, body.to_string().as_bytes(), ecs_metadata_now()),
                Err(ProbeError::InvalidResponse)
            ),
            "invalid {field} must be rejected"
        );
    }
}

#[test]
fn ecs_metadata_rejects_invalid_json_or_keyword_decoys() {
    for (case, body) in [
        ("empty body", ""),
        ("malformed JSON", "{\"Code\":\"Success\","),
        ("null document", "null"),
        ("array document", "[]"),
        ("empty object", "{}"),
        (
            "keyword text",
            "metadata error: AccessKeyId AccessKeySecret SecurityToken",
        ),
        (
            "keyword JSON string",
            r#""metadata error: AccessKeyId AccessKeySecret SecurityToken""#,
        ),
        (
            "keyword error object",
            r#"{"Code":"Success","Message":"AccessKeyId AccessKeySecret SecurityToken"}"#,
        ),
    ] {
        assert!(
            matches!(
                classify_credentials(200, body.as_bytes(), ecs_metadata_now()),
                Err(ProbeError::InvalidResponse)
            ),
            "{case} must be rejected"
        );
    }
}

#[test]
fn ecs_metadata_errors_do_not_display_response_body_or_credentials() {
    let mut body = ecs_metadata_credentials();
    body["Code"] = serde_json::json!("SyntheticFailure");
    let body = body.to_string();
    let malformed_body = format!("{body} invalid-json");

    for body in [&body, &malformed_body] {
        for status in [200, 401, 403, 500] {
            let error = match classify_credentials(status, body.as_bytes(), ecs_metadata_now()) {
                Err(error) => error,
                Ok(_) => panic!("error fixture must not produce a credential status"),
            };
            let message = error.to_string();
            // Do not print the message on failure: it may contain the leaked fixture.
            for forbidden in [
                body.as_str(),
                "synthetic-ak",
                "synthetic-sk",
                "synthetic-token",
            ] {
                assert!(
                    !message.contains(forbidden),
                    "HTTP {status} error Display must not expose response data"
                );
            }
        }
    }
    for error in [ProbeError::Unreachable, ProbeError::Timeout] {
        let message = error.to_string();
        for forbidden in [
            body.as_str(),
            "synthetic-ak",
            "synthetic-sk",
            "synthetic-token",
        ] {
            assert!(
                !message.contains(forbidden),
                "transport error Display must not expose response data"
            );
        }
    }
}

#[test]
fn ecs_metadata_error_codes_and_messages_are_fixed() {
    for (error, code, message) in [
        (
            ProbeError::AccessDenied,
            "metadata_access_denied",
            "Unable to access ECS instance metadata.",
        ),
        (
            ProbeError::InvalidResponse,
            "invalid_metadata_response",
            "ECS instance metadata returned an invalid response.",
        ),
        (
            ProbeError::Unreachable,
            "metadata_unreachable",
            "Unable to reach ECS instance metadata.",
        ),
        (
            ProbeError::Timeout,
            "metadata_timeout",
            "ECS instance metadata request timed out.",
        ),
        (
            ProbeError::Http,
            "metadata_http_error",
            "ECS instance metadata returned an HTTP error.",
        ),
    ] {
        assert_eq!(error.code(), code);
        assert_eq!(error.to_string(), message);
    }
}

async fn ecs_metadata_read_request(socket: &mut AsyncTcpStream) -> std::io::Result<String> {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        request.extend_from_slice(&buffer[..count]);
    }
    Ok(String::from_utf8(request).expect("ASCII HTTP request"))
}

#[tokio::test]
async fn ecs_metadata_token_required_service_accepts_probe() {
    let mut body = ecs_metadata_credentials();
    body["Expiration"] = serde_json::json!("2100-01-01T00:00:00Z");
    assert_eq!(
        ecs_metadata_fixture(
            ecs_metadata_http_response(200, &body.to_string()),
            MetadataBodyMode::Complete,
        )
        .await,
        Ok(CredentialStatus::Ready),
    );
}

#[derive(Clone, Copy)]
enum MetadataBodyMode {
    Complete,
    Stall,
    Trickle,
}

async fn ecs_metadata_fixture(
    response: String,
    mode: MetadataBodyMode,
) -> Result<CredentialStatus, ProbeError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind metadata fixture");
    let url = format!(
        "http://{}/latest/meta-data/ram/security-credentials/{ECS_RAM_ROLE_NAME}",
        listener.local_addr().expect("fixture address")
    );
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let request = ecs_metadata_read_request(&mut socket).await?;
        // The fixture deliberately rejects IMDSv1 instead of accepting a tokenless GET.
        if !request.starts_with("PUT /latest/api/token HTTP/1.1\r\n") {
            socket
                .write_all(ecs_metadata_http_response(401, "token required").as_bytes())
                .await?;
            return Ok(());
        }
        assert!(request.contains("x-aliyun-ecs-metadata-token-ttl-seconds: 60\r\n"));
        assert!(!request.contains("x-aliyun-ecs-metadata-token:"));
        socket
            .write_all(ecs_metadata_http_response(200, "synthetic-imds-token").as_bytes())
            .await?;
        socket.shutdown().await?;
        let (mut socket, _) = listener.accept().await?;
        let request = ecs_metadata_read_request(&mut socket).await?;
        assert!(request.starts_with(&format!(
            "GET /latest/meta-data/ram/security-credentials/{ECS_RAM_ROLE_NAME} HTTP/1.1\r\n"
        )));
        assert!(request.contains("x-aliyun-ecs-metadata-token: synthetic-imds-token\r\n"));
        socket.write_all(response.as_bytes()).await?;
        match mode {
            MetadataBodyMode::Complete => socket.shutdown().await?,
            MetadataBodyMode::Stall => std::future::pending::<()>().await,
            MetadataBodyMode::Trickle => loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                socket.write_all(b"1\r\n \r\n").await?;
            },
        }
        Ok::<_, std::io::Error>(())
    });
    let outcome = tokio::time::timeout(TEST_WATCHDOG, super::ecs_metadata::probe_url(&url)).await;
    server.abort();
    match server.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => assert!(matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        )),
        Err(error) => assert!(error.is_cancelled(), "metadata fixture failed"),
    }
    outcome.expect("metadata probe must finish within watchdog")
}

fn ecs_metadata_http_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn ecs_metadata_http_probe_preserves_classification() {
    let mut body = ecs_metadata_credentials();
    body["Expiration"] = serde_json::json!("2100-01-01T00:00:00Z");
    let body = body.to_string();
    for (status, expected) in [
        (200, Ok(CredentialStatus::Ready)),
        (201, Ok(CredentialStatus::Ready)),
        (
            404,
            Ok(CredentialStatus::NotReady(NotReadyReason::RoleMissing)),
        ),
        (401, Err(ProbeError::AccessDenied)),
        (403, Err(ProbeError::AccessDenied)),
        (500, Err(ProbeError::Http)),
        (429, Err(ProbeError::Http)),
    ] {
        assert_eq!(
            ecs_metadata_fixture(
                ecs_metadata_http_response(status, &body),
                MetadataBodyMode::Complete
            )
            .await,
            expected,
            "HTTP {status}"
        );
    }
}

#[tokio::test]
async fn ecs_metadata_http_body_limit_includes_chunked_responses() {
    let mut body = ecs_metadata_credentials();
    body["Expiration"] = serde_json::json!("2100-01-01T00:00:00Z");
    let mut body = body.to_string();
    body.push_str(&" ".repeat(64 * 1024 - body.len()));
    for extra in [0, 1] {
        let body = format!("{body}{}", " ".repeat(extra));
        let expected = if extra == 0 {
            Ok(CredentialStatus::Ready)
        } else {
            Err(ProbeError::InvalidResponse)
        };
        for response in [
            ecs_metadata_http_response(200, &body),
            format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                body.len()
            ),
        ] {
            assert_eq!(
                ecs_metadata_fixture(response, MetadataBodyMode::Complete).await,
                expected
            );
        }
    }
}

#[tokio::test]
async fn ecs_metadata_http_redirect_is_not_followed() {
    let target = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redirect target");
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{}/steal\r\nContent-Length: 0\r\n\r\n",
        target.local_addr().expect("redirect address")
    );
    let result = ecs_metadata_fixture(response, MetadataBodyMode::Complete).await;
    assert_eq!(result, Err(ProbeError::Http));
    assert!(
        target.accept().now_or_never().is_none(),
        "redirect target must not receive a connection"
    );
}

#[tokio::test]
async fn ecs_metadata_http_deadline_covers_headers_body_and_trickle() {
    for (response, mode) in [
        (String::new(), MetadataBodyMode::Stall),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_string(),
            MetadataBodyMode::Stall,
        ),
        (
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_string(),
            MetadataBodyMode::Trickle,
        ),
    ] {
        let started = std::time::Instant::now();
        let result = ecs_metadata_fixture(response, mode).await;
        assert_eq!(result, Err(ProbeError::Timeout));
        assert!(
            started.elapsed() < TEST_COMPLETION_BOUND,
            "metadata deadline must cover the whole response"
        );
    }
}

#[tokio::test]
async fn ecs_metadata_http_connection_failure_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("unused local port");
    let url = format!("http://{}", listener.local_addr().expect("local address"));
    drop(listener);
    assert_eq!(
        super::ecs_metadata::probe_url(&url).await,
        Err(ProbeError::Unreachable)
    );
}
