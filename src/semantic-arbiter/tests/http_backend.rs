//! Real loopback HTTP exercises both provider adapters without remote credentials.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use semantic_arbiter::config::{BackendConfig, ModelConfig, Thresholds, TransportMode};
use semantic_arbiter::model::http::HttpBackend;
use semantic_arbiter::model::NoulQuestion;
use semantic_arbiter::model::{BackendKind, BackendMode, ModelBackend, ModelError, ModelRequest};
use semantic_arbiter::protocol::MAX_MESSAGE_BYTES;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Instant};

const TEST_KEY: &str = "synthetic-credential-not-a-secret";

#[derive(Clone)]
struct ResponsePlan {
    chunks: Vec<(Duration, Vec<u8>)>,
}

impl ResponsePlan {
    fn fixed(status: u16, headers: &str, body: &str) -> Self {
        Self { chunks: vec![(Duration::ZERO, format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()
        ).into_bytes())] }
    }
}

struct ObservedRequest {
    headers: String,
    body: Value,
}

struct Server {
    base: String,
    hits: Arc<AtomicUsize>,
    received: mpsc::UnboundedReceiver<ObservedRequest>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(plan: ResponsePlan) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let accepted = hits.clone();
    let (sender, received) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::SeqCst);
            let sender = sender.clone();
            let plan = plan.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, plan, sender).await;
            });
        }
    });
    Server {
        base,
        hits,
        received,
        task,
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    plan: ResponsePlan,
    sender: mpsc::UnboundedSender<ObservedRequest>,
) -> std::io::Result<()> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(bytes.len() < MAX_MESSAGE_BYTES);
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
    let _ = sender.send(ObservedRequest { headers, body });
    for (delay, bytes) in plan.chunks {
        if !delay.is_zero() {
            sleep(delay).await;
        }
        stream.write_all(&bytes).await?;
    }
    stream.shutdown().await
}

fn config(server: &Server, typesafe: bool) -> ModelConfig {
    let backend = if typesafe {
        BackendConfig::Typesafe {
            base_url: format!("{}/gateway///", server.base),
            model: "jev-latest".into(),
            key_env: "TYPESAFE_API_KEY".into(),
        }
    } else {
        BackendConfig::Bailian {
            endpoint_url: format!("{}/full/configured%20path/", server.base),
            model: "decision-model-preview".into(),
            key_env: "DASHSCOPE_API_KEY".into(),
        }
    };
    ModelConfig {
        config_revision: "policy-v1".into(),
        backend,
        timeout_ms: 1000,
        max_response_bytes: 4096,
        thresholds: Thresholds::default(),
        transport: TransportMode::LoopbackTest,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        state: json!({"task": "synthetic task", "candidate": {"resource_id": "r1"}}),
        questions: BTreeMap::from([
            (
                "preservation".into(),
                NoulQuestion::new("Must this be preserved?".into()),
            ),
            (
                "task_scope".into(),
                NoulQuestion::new("Is this in scope?".into()),
            ),
        ]),
    }
}

fn valid_body() -> &'static str {
    r#"{"model":"resolved-alias-2026","answers":{"preservation":{"type":"noul","noul":0.01},"task_scope":{"type":"noul","noul":0.99}},"usage":{"input_tokens":10,"output_tokens":2},"request_id":"synthetic-id","latency_ms":58.6}"#
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

#[tokio::test]
async fn both_adapters_send_exact_paths_auth_and_closed_body() {
    for typesafe in [false, true] {
        let mut server = server(ResponsePlan::fixed(200, "", valid_body())).await;
        let config = config(&server, typesafe);
        let backend = HttpBackend::new(&config, TEST_KEY.into()).unwrap();
        assert_eq!(
            backend.identity().kind,
            if typesafe {
                BackendKind::Typesafe
            } else {
                BackendKind::Bailian
            }
        );
        assert_eq!(backend.identity().mode, BackendMode::LoopbackTest);
        let reply = backend.infer(&request(), deadline()).await.unwrap();
        assert!(reply.answers["preservation"].supports_no(900_000));
        assert!(reply.answers["task_scope"].supports_yes(900_000));
        let observed = server.received.recv().await.unwrap();
        let path = if typesafe {
            "/gateway/v1/systemone"
        } else {
            "/full/configured%20path/"
        };
        assert!(observed
            .headers
            .starts_with(&format!("POST {path} HTTP/1.1\r\n")));
        let lower = observed.headers.to_ascii_lowercase();
        assert!(lower.contains(&format!("authorization: bearer {TEST_KEY}\r\n")));
        assert!(lower.contains("content-type: application/json\r\n"));
        assert!(!lower.contains("retry"));
        assert_eq!(
            observed.body,
            json!({
                "model": backend.identity().model,
                "state": request().state,
                "questions": {
                    "preservation": {"type":"noul","instructions":"Must this be preserved?"},
                    "task_scope": {"type":"noul","instructions":"Is this in scope?"}
                }
            })
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn both_adapters_retain_exact_tokens_and_never_round_support_into_permission() {
    for typesafe in [false, true] {
        for (token, yes, no) in [
            ("0.89999999999999999999", 899_999, 100_000),
            ("8.9999999999999999999e-1", 899_999, 100_000),
            ("0.10000000000000000001", 100_000, 899_999),
            ("1.0000000000000000001E-1", 100_000, 899_999),
            ("9.000000E-1", 900_000, 100_000),
            ("0.1", 100_000, 900_000),
            ("0.90000000000000000001", 900_000, 99_999),
            ("0.09999999999999999999", 99_999, 900_000),
            ("0.99999999999999999999", 999_999, 0),
            ("1e-999", 0, 999_999),
            ("1e-999999999999999999999", 0, 999_999),
            ("1e-6", 1, 999_999),
            ("0", 0, 1_000_000),
            ("-0.000e+9999", 0, 1_000_000),
            ("1.00000000000000000000", 1_000_000, 0),
            ("10e-1", 1_000_000, 0),
        ] {
            // Literal numbers must reach the adapter without a test-side f64 conversion.
            let body = format!(
                r#"{{"answers":{{"preservation":{{"type":"noul","noul":{token}}},"task_scope":{{"type":"noul","noul":{token}}}}}}}"#
            );
            let server = server(ResponsePlan::fixed(200, "", &body)).await;
            let backend = HttpBackend::new(&config(&server, typesafe), TEST_KEY.into()).unwrap();
            let reply = backend.infer(&request(), deadline()).await.unwrap();
            // Both questions retain their own support, even when their yes sum exceeds one.
            for probability in reply.answers.values() {
                assert_eq!(probability.raw_json_number(), token);
                assert_eq!(probability.yes_ppm(), yes, "{token}");
                assert_eq!(probability.no_ppm(), no, "{token}");
                for threshold in [0, 1, 900_000, 1_000_000, u32::MAX] {
                    assert_eq!(
                        probability.supports_yes(threshold),
                        yes >= threshold,
                        "{token}"
                    );
                    assert_eq!(
                        probability.supports_no(threshold),
                        no >= threshold,
                        "{token}"
                    );
                }
            }
            let encoded = serde_json::to_string(&reply).unwrap();
            assert!(encoded.contains(&format!(r#""raw":{token},"#)));
            assert_eq!(
                serde_json::from_str::<semantic_arbiter::model::ModelReply>(&encoded).unwrap(),
                reply
            );
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn both_adapters_reject_exact_out_of_range_numbers_before_float_conversion() {
    for typesafe in [false, true] {
        for token in [
            "1.00000000000000000001",
            "100000000000000000001e-20",
            "1.00000000000000000001e+0",
            "-0.00000000000000000001",
            "-1e-999",
            "-1e-999999999999999999999",
            "[]",
            "{}",
        ] {
            for invalid_first in [true, false] {
                let (first, second) = if invalid_first {
                    (token, "1")
                } else {
                    ("0", token)
                };
                let body = format!(
                    r#"{{"answers":{{"preservation":{{"type":"noul","noul":{first}}},"task_scope":{{"type":"noul","noul":{second}}}}}}}"#
                );
                let server = server(ResponsePlan::fixed(200, "", &body)).await;
                let backend =
                    HttpBackend::new(&config(&server, typesafe), TEST_KEY.into()).unwrap();
                assert_eq!(
                    backend.infer(&request(), deadline()).await,
                    Err(ModelError::InvalidResponse),
                    "{body}"
                );
                assert_eq!(server.hits.load(Ordering::SeqCst), 1);
            }
        }
    }
}

#[tokio::test]
async fn only_bailian_accepts_an_absent_answer_type() {
    let body = r#"{"answers":{"preservation":{"noul":0},"task_scope":{"noul":1}}}"#;
    for typesafe in [false, true] {
        let server = server(ResponsePlan::fixed(200, "", body)).await;
        let backend = HttpBackend::new(&config(&server, typesafe), TEST_KEY.into()).unwrap();
        let result = backend.infer(&request(), deadline()).await;
        if typesafe {
            assert_eq!(result, Err(ModelError::InvalidResponse));
        } else {
            assert!(result.is_ok());
        }
    }
}

#[tokio::test]
async fn rejects_invalid_answer_shapes_numbers_keys_and_actions() {
    let invalid = [
        r#"{"answers":{"preservation":{"type":"score","noul":0},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":null,"noul":0},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":"0"},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":true},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":null},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":-0.001},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1.000001}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":NaN},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":1e999},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0,"action":"delete"},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1},"extra":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}},"actions":[]}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}},"unknown":1}"#,
        r#"{"answers":{},"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0,"noul":1},"task_scope":{"type":"noul","noul":1}}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}},"usage":{"input_tokens":1,"input_tokens":2}}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}},"model":"a","model":"b"}"#,
        r#"{"answers":{"preservation":{"type":"noul","noul":0},"task_scope":{"type":"noul","noul":1}},"latency_ms":58.6,"latency_ms":60}"#,
        "not JSON",
    ];
    for typesafe in [false, true] {
        for body in invalid {
            let server = server(ResponsePlan::fixed(200, "", body)).await;
            let backend = HttpBackend::new(&config(&server, typesafe), TEST_KEY.into()).unwrap();
            assert_eq!(
                backend.infer(&request(), deadline()).await,
                Err(ModelError::InvalidResponse),
                "{body}"
            );
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn errors_are_sanitized_and_http_statuses_never_retry_or_read_error_bodies() {
    for (status, expected) in [
        (401, ModelError::Authentication),
        (403, ModelError::Authentication),
        (429, ModelError::RateLimited),
        (500, ModelError::Http),
        (503, ModelError::Http),
        (400, ModelError::Http),
        (302, ModelError::Http),
    ] {
        // An incomplete error body must not turn a known status into a read timeout.
        let response =
            format!("HTTP/1.1 {status} Test\r\nContent-Length: 999999\r\nRetry-After: 0\r\n\r\n");
        let server = server(ResponsePlan {
            chunks: vec![
                (Duration::ZERO, response.into_bytes()),
                (
                    Duration::from_secs(1),
                    format!("{TEST_KEY} synthetic task internal-url").into_bytes(),
                ),
            ],
        })
        .await;
        let mut config = config(&server, false);
        config.timeout_ms = 100;
        config.max_response_bytes = 1;
        let backend = HttpBackend::new(&config, TEST_KEY.into()).unwrap();
        let error = backend.infer(&request(), deadline()).await.unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        for text in [
            error.to_string(),
            format!("{error:?}"),
            serde_json::to_string(&error).unwrap(),
        ] {
            assert!(!text.contains(TEST_KEY));
            assert!(!text.contains("synthetic task"));
            assert!(!text.contains(&server.base));
        }
    }
}

#[tokio::test]
async fn redirects_never_send_credentials_to_the_target() {
    let target = server(ResponsePlan::fixed(200, "", valid_body())).await;
    for status in [301, 302, 303, 307, 308] {
        let source = server(ResponsePlan::fixed(
            status,
            &format!("Location: {}/capture\r\n", target.base),
            "",
        ))
        .await;
        let backend = HttpBackend::new(&config(&source, false), TEST_KEY.into()).unwrap();
        assert_eq!(
            backend.infer(&request(), deadline()).await,
            Err(ModelError::Http)
        );
        assert_eq!(source.hits.load(Ordering::SeqCst), 1);
        assert_eq!(target.hits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn response_cap_is_streamed_with_and_without_content_length() {
    let body = valid_body();
    let plans = [
        ResponsePlan::fixed(200, "", body),
        ResponsePlan {
            chunks: vec![(
                Duration::ZERO,
                format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}").into_bytes(),
            )],
        },
        ResponsePlan {
            chunks: vec![
                (
                    Duration::ZERO,
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
                ),
                (
                    Duration::ZERO,
                    format!("{:x}\r\n{}\r\n", body.len() / 2, &body[..body.len() / 2]).into_bytes(),
                ),
                (
                    Duration::ZERO,
                    format!(
                        "{:x}\r\n{}\r\n0\r\n\r\n",
                        body.len() - body.len() / 2,
                        &body[body.len() / 2..]
                    )
                    .into_bytes(),
                ),
            ],
        },
    ];
    for plan in plans {
        for too_small in [false, true] {
            let server = server(plan.clone()).await;
            let mut config = config(&server, false);
            config.max_response_bytes = body.len() - usize::from(too_small);
            let backend = HttpBackend::new(&config, TEST_KEY.into()).unwrap();
            let result = backend.infer(&request(), deadline()).await;
            if too_small {
                assert_eq!(result, Err(ModelError::ResponseTooLarge));
            } else {
                assert!(result.is_ok());
            }
        }
    }
}

#[tokio::test]
async fn body_cap_aborts_before_the_response_finishes() {
    for headers in ["Content-Length: 999999", "Transfer-Encoding: chunked"] {
        let prefix = if headers.starts_with("Transfer") {
            "41\r\n"
        } else {
            ""
        };
        let server = server(ResponsePlan {
            chunks: vec![
                (
                    Duration::ZERO,
                    format!(
                        "HTTP/1.1 200 OK\r\n{headers}\r\n\r\n{prefix}{}",
                        "x".repeat(65)
                    )
                    .into_bytes(),
                ),
                (Duration::from_secs(1), b"\r\n0\r\n\r\n".to_vec()),
            ],
        })
        .await;
        let mut config = config(&server, false);
        config.max_response_bytes = 64;
        config.timeout_ms = 100;
        let backend = HttpBackend::new(&config, TEST_KEY.into()).unwrap();
        assert_eq!(
            backend.infer(&request(), deadline()).await,
            Err(ModelError::ResponseTooLarge)
        );
    }
}

#[tokio::test]
async fn delayed_polling_does_not_renew_the_configured_timeout() {
    let server = server(ResponsePlan::fixed(200, "", valid_body())).await;
    let mut config = config(&server, false);
    config.timeout_ms = 1;
    let backend = HttpBackend::new(&config, TEST_KEY.into()).unwrap();
    let request = request();
    let pending = backend.infer(&request, deadline());
    sleep(Duration::from_millis(10)).await;
    assert_eq!(pending.await, Err(ModelError::Timeout));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn header_wait_and_dribbling_body_share_one_absolute_deadline() {
    let body = valid_body();
    let plans = [
        ResponsePlan {
            chunks: vec![(
                Duration::from_millis(250),
                ResponsePlan::fixed(200, "", body).chunks.remove(0).1,
            )],
        },
        ResponsePlan {
            chunks: vec![
                (
                    Duration::ZERO,
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
                ),
                (Duration::from_millis(30), b"1\r\n{\r\n".to_vec()),
                (Duration::from_millis(30), b"1\r\n \r\n".to_vec()),
                (Duration::from_millis(30), b"1\r\n \r\n".to_vec()),
                (Duration::from_millis(250), b"1\r\n}\r\n0\r\n\r\n".to_vec()),
            ],
        },
    ];
    for plan in plans {
        for use_config_limit in [false, true] {
            let server = server(plan.clone()).await;
            let mut config = config(&server, false);
            config.timeout_ms = if use_config_limit { 75 } else { 1000 };
            let backend = HttpBackend::new(&config, TEST_KEY.into()).unwrap();
            let deadline =
                Instant::now() + Duration::from_millis(if use_config_limit { 1000 } else { 75 });
            let result = timeout(
                Duration::from_millis(500),
                backend.infer(&request(), deadline),
            )
            .await
            .unwrap();
            assert_eq!(result, Err(ModelError::Timeout));
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn expired_or_oversized_requests_never_reach_the_server() {
    let server = server(ResponsePlan::fixed(200, "", valid_body())).await;
    let backend = HttpBackend::new(&config(&server, false), TEST_KEY.into()).unwrap();
    assert_eq!(
        backend.infer(&request(), Instant::now()).await,
        Err(ModelError::Timeout)
    );
    let mut request = request();
    request.state = json!("x".repeat(MAX_MESSAGE_BYTES));
    assert_eq!(
        backend.infer(&request, deadline()).await,
        Err(ModelError::InvalidResponse)
    );
    request.questions.clear();
    assert_eq!(
        backend.infer(&request, deadline()).await,
        Err(ModelError::InvalidResponse)
    );
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disconnected_and_truncated_responses_are_transport_failures_without_retry() {
    for plan in [
        ResponsePlan { chunks: vec![] },
        ResponsePlan {
            chunks: vec![(
                Duration::ZERO,
                b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n{".to_vec(),
            )],
        },
    ] {
        let server = server(plan).await;
        let backend = HttpBackend::new(&config(&server, false), TEST_KEY.into()).unwrap();
        assert_eq!(
            backend.infer(&request(), deadline()).await,
            Err(ModelError::Transport)
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn invalid_credentials_are_sanitized_before_any_connection() {
    let server = server(ResponsePlan::fixed(200, "", valid_body())).await;
    let config = config(&server, false);
    for key in ["", "\r\nsynthetic-injection", "has space", "non-ascii-密钥"] {
        assert!(matches!(
            HttpBackend::new(&config, key.into()),
            Err(ModelError::MissingCredentials)
        ));
    }
    let mut config = config;
    if let BackendConfig::Bailian {
        ref mut key_env, ..
    } = config.backend
    {
        *key_env = "SEMANTIC_ARBITER_TEST_NO_SUCH_CREDENTIAL_7592_001".into();
    }
    assert!(matches!(
        HttpBackend::from_env(&config),
        Err(ModelError::MissingCredentials)
    ));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn environment_proxies_are_ignored() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "proxy_environment_child", "--nocapture"])
        .env("SEMANTIC_ARBITER_PROXY_TEST_CHILD", "1")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("http_proxy", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("all_proxy", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn proxy_environment_child() {
    if std::env::var_os("SEMANTIC_ARBITER_PROXY_TEST_CHILD").is_none() {
        return;
    }
    let server = server(ResponsePlan::fixed(200, "", valid_body())).await;
    let backend = HttpBackend::new(&config(&server, false), TEST_KEY.into()).unwrap();
    assert!(backend.infer(&request(), deadline()).await.is_ok());
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}
