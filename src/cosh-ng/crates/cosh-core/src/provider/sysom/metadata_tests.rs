//! IMDSv2 transport and end-to-end core auth regressions; all services are loopback.

use super::ecs_metadata::TEST_ENDPOINT;
use super::*;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN_PATH: &str = "/latest/api/token";
const ROLE_PATH: &str = "/latest/meta-data/ram/security-credentials/AliyunECSInstanceForSysomRole";
const INSTANCE_PATH: &str = "/latest/meta-data/instance-id";
const ZONE_PATH: &str = "/latest/meta-data/zone-id";
const IMDS_TOKEN: &str = "synthetic-imds-secret";

tokio::task_local! {
    pub(super) static TEST_CACHE_PATH: PathBuf;
}

struct Reply {
    path: &'static str,
    response: String,
    delay: Duration,
    close_listener: bool,
}

impl Reply {
    fn new(path: &'static str, status: u16, body: &str) -> Self {
        Self {
            path,
            response: format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            delay: Duration::ZERO,
            close_listener: false,
        }
    }

    fn token() -> Self {
        Self::new(TOKEN_PATH, 200, IMDS_TOKEN)
    }
}

struct Fixture {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Fixture {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut replies = replies.into_iter();
            loop {
                let (mut socket, _) = listener.accept().await?;
                let request = read_request(&mut socket).await?;
                captured.lock().unwrap().push(request.clone());
                let Some(reply) = replies.next() else {
                    socket
                        .write_all(Reply::new("", 401, "no fallback").response.as_bytes())
                        .await?;
                    continue;
                };
                let method = match reply.path {
                    TOKEN_PATH => "PUT",
                    API_PATH => "POST",
                    _ => "GET",
                };
                assert!(request.starts_with(&format!("{method} {} HTTP/1.1\r\n", reply.path)));
                if reply.path == TOKEN_PATH {
                    assert!(request.contains("x-aliyun-ecs-metadata-token-ttl-seconds: 60\r\n"));
                    assert!(!request.contains("x-aliyun-ecs-metadata-token:"));
                } else if reply.path == API_PATH {
                    assert!(!request.contains(IMDS_TOKEN));
                } else {
                    assert!(
                        request.contains(&format!("x-aliyun-ecs-metadata-token: {IMDS_TOKEN}\r\n"))
                    );
                }
                if !reply.delay.is_zero() {
                    tokio::time::sleep(reply.delay).await;
                }
                if reply.close_listener {
                    // Close before replying so the next request fails to connect.
                    drop(listener);
                    return socket.write_all(reply.response.as_bytes()).await;
                }
                socket.write_all(reply.response.as_bytes()).await?;
                socket.shutdown().await?;
            }
        });
        Self {
            base_url,
            requests,
            task,
        }
    }

    async fn finish(self, count: usize) -> Vec<String> {
        self.task.abort();
        match self.task.await {
            Err(error) => assert!(error.is_cancelled(), "fixture task panicked"),
            Ok(Err(error)) => assert!(matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            )),
            Ok(Ok(())) => {}
        }
        let requests = self.requests.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            count,
            "unexpected retry, fallback or missing metadata request"
        );
        requests
    }
}

async fn read_request(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return Ok(String::from_utf8(bytes).unwrap());
            }
        }
    }
}

fn credentials() -> Value {
    serde_json::json!({
        "Code": "Success", "AccessKeyId": "synthetic-ak", "AccessKeySecret": "synthetic-sk",
        "SecurityToken": "synthetic-sts", "Expiration": "2100-01-01T00:00:00Z"
    })
}

#[test]
fn ecs_metadata_constructors_do_not_resolve_instance_id() {
    for provider in [
        SysomProvider::new("ak", "sk", None, ""),
        SysomProvider::from_ecs_ram_role(""),
    ] {
        assert!(provider.instance_id.get().is_none());
    }
}

#[tokio::test]
async fn ecs_metadata_prepare_probe_and_sts_share_transport() {
    let body = credentials().to_string();
    let fixture = Fixture::start(vec![
        Reply::token(),
        Reply::new(INSTANCE_PATH, 200, "i-test123\n"),
        Reply::new(ZONE_PATH, 200, "cn-shanghai-g\n"),
        Reply::token(),
        Reply::new(ROLE_PATH, 200, &body),
        Reply::token(),
        Reply::new(ROLE_PATH, 200, &body),
    ])
    .await;
    TEST_ENDPOINT
        .scope(fixture.base_url.clone(), async {
            let prepared = detect_ecs_auth_challenge().await.unwrap().unwrap();
            assert_eq!(prepared.instance_id, "i-test123");
            assert_eq!(
                prepared.console_url,
                "https://alinux.console.aliyun.com/cn-shanghai/guide/cosh?instance=i-test123"
            );
            assert_eq!(probe_ecs_ram_role().await, Ok(CredentialStatus::Ready));
            let provider = SysomProvider::from_ecs_ram_role("");
            assert!(provider.refresh_sts_credentials().await);
            let creds = provider.credentials.read().unwrap();
            assert_eq!(creds.access_key_id, "synthetic-ak");
            assert_eq!(creds.security_token.as_deref(), Some("synthetic-sts"));
        })
        .await;
    fixture.finish(7).await;
}

#[tokio::test]
async fn ecs_metadata_malformed_http_is_not_manual_auth() {
    let mut reply = Reply::token();
    reply.response = "not-an-http-response\r\n\r\n".to_string();
    let fixture = Fixture::start(vec![reply]).await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
        .await;
    assert_eq!(result, Err(ProbeError::InvalidResponse));
    fixture.finish(1).await;
}

#[tokio::test]
async fn ecs_metadata_token_redirect_does_not_forward_or_downgrade() {
    use futures::FutureExt;
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut reply = Reply::token();
    reply.response = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{}/steal\r\nContent-Length: 0\r\n\r\n",
        target.local_addr().unwrap()
    );
    let fixture = Fixture::start(vec![reply]).await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), probe_ecs_ram_role())
        .await;
    assert_eq!(result, Err(ProbeError::Http));
    fixture.finish(1).await;
    assert!(target.accept().now_or_never().is_none());
}

#[tokio::test]
async fn ecs_metadata_prepare_timeout_is_manual_auth() {
    let mut reply = Reply::token();
    reply.delay = Duration::from_secs(4);
    let fixture = Fixture::start(vec![reply]).await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
        .await;
    assert_eq!(result, Ok(None));
    fixture.finish(1).await;
}

#[tokio::test]
async fn ecs_metadata_prepare_instance_timeout_is_not_manual() {
    let mut instance = Reply::new(INSTANCE_PATH, 200, "i-test");
    instance.delay = Duration::from_secs(4);
    let fixture = Fixture::start(vec![Reply::token(), instance]).await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
        .await;
    fixture.finish(2).await;
    assert_eq!(result, Err(ProbeError::Timeout));
}

#[tokio::test]
async fn ecs_metadata_prepare_zone_timeout_is_not_manual() {
    let mut zone = Reply::new(ZONE_PATH, 200, "cn-shanghai-g");
    zone.delay = Duration::from_secs(4);
    let fixture = Fixture::start(vec![
        Reply::token(),
        Reply::new(INSTANCE_PATH, 200, "i-test"),
        zone,
    ])
    .await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
        .await;
    fixture.finish(3).await;
    assert_eq!(result, Err(ProbeError::Timeout));
}

#[tokio::test]
async fn ecs_metadata_prepare_instance_unreachable_is_not_manual() {
    let mut token = Reply::token();
    token.close_listener = true;
    let fixture = Fixture::start(vec![token]).await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
        .await;
    fixture.finish(1).await;
    assert_eq!(result, Err(ProbeError::Unreachable));
}

#[tokio::test]
async fn ecs_metadata_prepare_zone_unreachable_is_not_manual() {
    let mut instance = Reply::new(INSTANCE_PATH, 200, "i-test");
    instance.close_listener = true;
    let fixture = Fixture::start(vec![Reply::token(), instance]).await;
    let result = TEST_ENDPOINT
        .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
        .await;
    fixture.finish(2).await;
    assert_eq!(result, Err(ProbeError::Unreachable));
}

#[tokio::test]
async fn ecs_metadata_token_failures_never_fall_back_or_leak() {
    for (status, body, expected) in [
        (401, IMDS_TOKEN.to_string(), ProbeError::AccessDenied),
        (403, IMDS_TOKEN.to_string(), ProbeError::AccessDenied),
        (404, IMDS_TOKEN.to_string(), ProbeError::Http),
        (405, IMDS_TOKEN.to_string(), ProbeError::Http),
        (500, IMDS_TOKEN.to_string(), ProbeError::Http),
        (200, String::new(), ProbeError::InvalidResponse),
        (200, "  ".to_string(), ProbeError::InvalidResponse),
        (
            200,
            "injected\r\nheader".to_string(),
            ProbeError::InvalidResponse,
        ),
        (200, "x".repeat(4097), ProbeError::InvalidResponse),
    ] {
        let fixture = Fixture::start(vec![Reply::new(TOKEN_PATH, status, &body)]).await;
        let result = ecs_metadata::TEST_ENDPOINT
            .scope(fixture.base_url.clone(), probe_ecs_ram_role())
            .await;
        assert_eq!(result, Err(expected));
        assert!(!format!("{expected:?} {expected}").contains(IMDS_TOKEN));
        fixture.finish(1).await;
    }
}

#[tokio::test]
async fn ecs_metadata_prepare_rejects_denied_or_invalid_identity() {
    for (path, status, body, code) in [
        (TOKEN_PATH, 401, IMDS_TOKEN, "metadata_access_denied"),
        (TOKEN_PATH, 403, IMDS_TOKEN, "metadata_access_denied"),
        (TOKEN_PATH, 200, "", "invalid_metadata_response"),
        (INSTANCE_PATH, 401, IMDS_TOKEN, "metadata_access_denied"),
        (
            INSTANCE_PATH,
            200,
            "not-an-instance",
            "invalid_metadata_response",
        ),
        (
            INSTANCE_PATH,
            200,
            "i-test?token=secret",
            "invalid_metadata_response",
        ),
        (ZONE_PATH, 403, IMDS_TOKEN, "metadata_access_denied"),
        (ZONE_PATH, 200, "", "invalid_metadata_response"),
    ] {
        let mut replies = Vec::new();
        if path != TOKEN_PATH {
            replies.push(Reply::token());
        }
        if path == ZONE_PATH {
            replies.push(Reply::new(INSTANCE_PATH, 200, "i-test"));
        }
        replies.push(Reply::new(path, status, body));
        let count = replies.len();
        let fixture = Fixture::start(replies).await;
        let response = TEST_ENDPOINT
            .scope(fixture.base_url.clone(), detect_ecs_auth_challenge())
            .await;
        assert_eq!(response.unwrap_err().code(), code);
        fixture.finish(count).await;
    }
}

#[tokio::test]
async fn ecs_metadata_prepare_unreachable_is_manual() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let response = TEST_ENDPOINT
        .scope(base_url, detect_ecs_auth_challenge())
        .await;
    assert_eq!(response, Ok(None));
}

#[tokio::test]
async fn ecs_metadata_runtime_rejects_every_nonready_probe_result() {
    let mut invalid = credentials();
    invalid["Code"] = serde_json::json!("Failure");
    let mut expired = credentials();
    expired["Expiration"] = serde_json::json!("2000-01-01T00:00:00Z");
    let mut empty = credentials();
    empty["SecurityToken"] = serde_json::json!(" ");
    for (status, body, code) in [
        (200, invalid, "invalid_metadata_response"),
        (200, expired, "credential_source_unavailable"),
        (200, empty, "invalid_metadata_response"),
        (404, credentials(), "credential_source_unavailable"),
        (401, credentials(), "metadata_access_denied"),
    ] {
        let fixture = Fixture::start(
            (0..2)
                .flat_map(|_| {
                    [
                        Reply::token(),
                        Reply::new(ROLE_PATH, status, &body.to_string()),
                    ]
                })
                .collect(),
        )
        .await;
        ecs_metadata::TEST_ENDPOINT
            .scope(fixture.base_url.clone(), async {
                match probe_ecs_ram_role().await {
                    Err(error) => assert_eq!(error.code(), code),
                    Ok(CredentialStatus::NotReady(_)) => {
                        assert_eq!(code, "credential_source_unavailable")
                    }
                    Ok(CredentialStatus::Ready) => panic!("invalid credentials accepted"),
                }
                let provider = SysomProvider::new("old-ak", "old-sk", Some("old-sts"), "");
                assert!(!provider.refresh_sts_credentials().await);
                assert_eq!(
                    provider
                        .credentials
                        .read()
                        .unwrap()
                        .security_token
                        .as_deref(),
                    Some("old-sts")
                );
            })
            .await;
        fixture.finish(4).await;
    }
}

#[tokio::test]
async fn ecs_metadata_token_and_get_share_total_deadline() {
    let mut token = Reply::token();
    token.delay = Duration::from_millis(1800);
    let mut role = Reply::new(ROLE_PATH, 200, &credentials().to_string());
    role.delay = Duration::from_millis(1800);
    let fixture = Fixture::start(vec![token, role]).await;
    let result = ecs_metadata::TEST_ENDPOINT
        .scope(fixture.base_url.clone(), probe_ecs_ram_role())
        .await;
    assert_eq!(result, Err(ProbeError::Timeout));
    fixture.finish(2).await;
}

#[tokio::test]
async fn ecs_metadata_generate_initializes_body_and_refreshes_sts_once() {
    let mut refreshed = credentials();
    refreshed["AccessKeyId"] = serde_json::json!("refreshed-ak");
    let fixture = Fixture::start(vec![
        Reply::token(),
        Reply::new(INSTANCE_PATH, 200, "i-live"),
        Reply::token(),
        Reply::new(ROLE_PATH, 200, &credentials().to_string()),
        Reply::new(API_PATH, 403, "InvalidSecurityToken"),
        Reply::token(),
        Reply::new(ROLE_PATH, 200, &refreshed.to_string()),
        Reply::new(API_PATH, 403, "SecurityTokenExpired"),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("instance_id");
    let provider = SysomProvider::from_ecs_ram_role(&fixture.base_url);
    let result = TEST_CACHE_PATH
        .scope(
            cache_path.clone(),
            ecs_metadata::TEST_ENDPOINT.scope(
                fixture.base_url.clone(),
                provider.generate(&[], &[], &GenerateConfig::default()),
            ),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        provider.instance_id.get(),
        Some(&Some("i-live".to_string()))
    );
    assert_eq!(std::fs::read_to_string(cache_path).unwrap(), "i-live");
    let requests = fixture.finish(8).await;
    for index in [4, 7] {
        let body: Value =
            serde_json::from_str(requests[index].split_once("\r\n\r\n").unwrap().1).unwrap();
        let inner: Value = serde_json::from_str(body["llmParamString"].as_str().unwrap()).unwrap();
        assert_eq!(inner["instance_id"], "i-live");
    }
    assert!(requests[4].contains("Credential=synthetic-ak,"));
    assert!(requests[7].contains("Credential=refreshed-ak,"));
}

#[tokio::test]
async fn ecs_metadata_disk_cache_preserves_positive_negative_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("instance_id");
    let fixture = Fixture::start(vec![
        Reply::token(),
        Reply::new(INSTANCE_PATH, 200, "i-new"),
    ])
    .await;
    ecs_metadata::TEST_ENDPOINT
        .scope(fixture.base_url.clone(), async {
            for (content, expected) in [("i-cached\n", Some("i-cached".to_string())), ("", None)] {
                std::fs::write(&cache, content).unwrap();
                assert_eq!(resolve_instance_id_cached(&cache).await, expected);
            }
            assert!(fixture.requests.lock().unwrap().is_empty());
            let file = std::fs::File::options().write(true).open(&cache).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::now() - Duration::from_secs(INSTANCE_ID_CACHE_TTL_SECS + 1),
            ))
            .unwrap();
            assert_eq!(
                resolve_instance_id_cached(&cache).await,
                Some("i-new".to_string())
            );
            assert_eq!(std::fs::read_to_string(&cache).unwrap(), "i-new");
        })
        .await;
    fixture.finish(2).await;
}
