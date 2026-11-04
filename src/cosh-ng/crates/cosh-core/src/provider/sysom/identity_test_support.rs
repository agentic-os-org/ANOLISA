use super::ecs_identity::{IdentityClient, TEST_ENDPOINT};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Semaphore};

pub(super) struct Fixture {
    pub(super) endpoint: String,
    pub(super) model_response: Arc<Mutex<String>>,
    pub(super) metadata_response: Arc<Mutex<Option<String>>>,
    pub(super) requests: Arc<Mutex<Vec<String>>>,
    pub(super) exchange_gate: Arc<Semaphore>,
    pub(super) challenge_gate: Arc<Semaphore>,
    pub(super) proof_gate: Arc<Semaphore>,
    pub(super) model_gate: Arc<Semaphore>,
    pub(super) credential_ttl: Arc<std::sync::atomic::AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    pub(super) async fn start(exchange_code: &str, gated: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let exchange_gate = Arc::new(Semaphore::new(if gated { 0 } else { 100 }));
        let model_response=Arc::new(Mutex::<String>::new("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()));
        let model_reply = model_response.clone();
        let metadata_response = Arc::new(Mutex::new(None::<String>));
        let metadata_reply = metadata_response.clone();
        let model_gate = Arc::new(Semaphore::new(100));
        let model_permits = model_gate.clone();
        let records = requests.clone();
        let gate = exchange_gate.clone();
        let challenge_gate = Arc::new(Semaphore::new(100));
        let challenge_permits = challenge_gate.clone();
        let proof_gate = Arc::new(Semaphore::new(100));
        let proof_permits = proof_gate.clone();
        let credential_ttl = Arc::new(std::sync::atomic::AtomicU64::new(900));
        let issued_ttl = credential_ttl.clone();
        let code = exchange_code.to_owned();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let model_reply = model_reply.clone();
                let metadata_reply = metadata_reply.clone();
                let model_permits = model_permits.clone();
                let records = records.clone();
                let gate = gate.clone();
                let challenge_permits = challenge_permits.clone();
                let proof_permits = proof_permits.clone();
                let code = code.clone();
                let issued_ttl = issued_ttl.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..n]);
                        if let Some(end) = raw.windows(4).position(|x| x == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length:")
                                        .and_then(|s| s.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if raw.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let request = String::from_utf8(raw).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    records.lock().await.push(request);
                    if path.starts_with("/latest/") {
                        if let Some(response) = metadata_reply.lock().await.as_ref() {
                            let _ = socket.write_all(response.as_bytes()).await;
                            return;
                        }
                    }
                    if path == super::API_PATH {
                        let response = model_reply.lock().await.clone();
                        if let Some((headers, body)) = response.split_once("\r\n\r\n") {
                            let _ = socket
                                .write_all(format!("{headers}\r\n\r\n").as_bytes())
                                .await;
                            model_permits.acquire().await.unwrap().forget();
                            let _ = socket.write_all(body.as_bytes()).await;
                        }
                        return;
                    }
                    let body = if path.ends_with("/challenge") {
                        challenge_permits.acquire().await.unwrap().forget();
                        serde_json::json!({"code":"Success","success":true,"data":{"challengeId":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","audience":"sysom:ecs-model:prod:cn-hangzhou:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","serverTime":"2026-01-01T00:00:00Z","expiresAt":"2026-01-01T00:02:00Z"}}).to_string()
                    } else if path.ends_with("/credentials") {
                        gate.acquire().await.unwrap().forget();
                        if code == "Success" {
                            let expiry =
                                chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                                    .unwrap()
                                    + chrono::Duration::seconds(
                                        issued_ttl.load(std::sync::atomic::Ordering::SeqCst) as i64,
                                    );
                            serde_json::json!({"code":"Success","success":true,"data":{"accessKeyId":"STS.test","accessKeySecret":"secret-canary","securityToken":"token-canary","serverTime":"2026-01-01T00:00:00Z","expiration":expiry.to_rfc3339()}}).to_string()
                        } else {
                            serde_json::json!({"code":code,"success":false,"message":"secret-canary"}).to_string()
                        }
                    } else if path.ends_with("/api/token") {
                        "imds-token-canary".into()
                    } else if path.ends_with("/document") {
                        "{\n \"instance-id\": \"i-test\"\n}\n".into()
                    } else if path.starts_with("/latest/dynamic/instance-identity/pkcs7?") {
                        proof_permits.acquire().await.unwrap().forget();
                        " Zm9v\n".into()
                    } else {
                        panic!("unexpected path {path}")
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            endpoint,
            model_response,
            metadata_response,
            requests,
            exchange_gate,
            challenge_gate,
            proof_gate,
            model_gate,
            credential_ttl,
            task,
        }
    }
    pub(super) async fn client(&self) -> IdentityClient {
        super::ecs_metadata::TEST_ENDPOINT
            .scope(
                self.endpoint.clone(),
                TEST_ENDPOINT.scope(self.endpoint.clone(), async {
                    IdentityClient::new(super::endpoint::PUBLIC_HOST.into())
                }),
            )
            .await
    }
    pub(super) async fn provider(&self) -> super::SysomProvider {
        super::ecs_metadata::TEST_ENDPOINT
            .scope(
                self.endpoint.clone(),
                TEST_ENDPOINT.scope(self.endpoint.clone(), async {
                    let provider = super::SysomProvider::from_ecs_identity(&self.endpoint);
                    let _ = provider.instance_id.set(Some("i-test".into()));
                    provider
                }),
            )
            .await
    }
    pub(super) async fn provider_with_admission(
        &self,
        check: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> super::SysomProvider {
        super::ecs_metadata::TEST_ENDPOINT
            .scope(
                self.endpoint.clone(),
                TEST_ENDPOINT.scope(self.endpoint.clone(), async {
                    let provider = super::SysomProvider::from_ecs_identity_with_admission(
                        &self.endpoint,
                        check,
                    );
                    let _ = provider.instance_id.set(Some("i-test".into()));
                    provider
                }),
            )
            .await
    }
    pub(super) async fn model(&self, status: u16, content_type: &str, body: &str) {
        *self.model_response.lock().await=format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
    }
    pub(super) async fn wait_for_exchange(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if self
                    .requests
                    .lock()
                    .await
                    .iter()
                    .any(|r| r.starts_with("POST /api/v1/identity/ecs-model/credentials"))
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
