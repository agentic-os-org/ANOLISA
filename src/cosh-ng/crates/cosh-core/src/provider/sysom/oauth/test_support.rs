//! Loopback protocol double for the fixed OAuth contract (test only).

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Semaphore};

/// Tests that bind the fixed production callback port must serialize across modules.
pub static OAUTH_PORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct OAuthFixture {
    pub endpoint: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    pub challenge_response: Arc<Mutex<String>>,
    pub credentials_response: Arc<Mutex<String>>,
    pub revoke_response: Arc<Mutex<String>>,
    pub model_response: Arc<Mutex<String>>,
    pub client_id: Arc<Mutex<String>>,
    pub credentials_gate: Arc<Semaphore>,
    task: tokio::task::JoinHandle<()>,
}

pub fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn success_credentials(ttl_secs: i64, with_refresh: bool) -> String {
    let expiry = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap()
        + chrono::Duration::seconds(ttl_secs);
    let mut data = serde_json::json!({
        "accessKeyId": "STS.test",
        "accessKeySecret": "secret-canary",
        "securityToken": "token-canary",
        "serverTime": "2026-01-01T00:00:00Z",
        "expiration": expiry.to_rfc3339(),
    });
    if with_refresh {
        data["refreshToken"] = serde_json::json!("refresh-canary");
    }
    serde_json::json!({"code":"Success","success":true,"data":data}).to_string()
}

pub fn error_envelope(code: &str) -> String {
    serde_json::json!({"code":code,"success":false,"message":"secret-canary"}).to_string()
}

impl OAuthFixture {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let challenge_response = Arc::new(Mutex::new("AUTO".to_string()));
        let credentials_response = Arc::new(Mutex::new(http_response(
            "200 OK",
            &success_credentials(900, true),
        )));
        let revoke_response = Arc::new(Mutex::new(http_response("200 OK", "{}")));
        let model_response = Arc::new(Mutex::new(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"oauth-answer\"}}]}\n\n".to_string(),
        ));
        let client_id = Arc::new(Mutex::new("4038181954510891234".to_string()));
        let credentials_gate = Arc::new(Semaphore::new(100));
        let records = requests.clone();
        let challenge_reply = challenge_response.clone();
        let credentials_reply = credentials_response.clone();
        let revoke_reply = revoke_response.clone();
        let model_reply = model_response.clone();
        let app_id = client_id.clone();
        let gate = credentials_gate.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let records = records.clone();
                let challenge_reply = challenge_reply.clone();
                let credentials_reply = credentials_reply.clone();
                let revoke_reply = revoke_reply.clone();
                let model_reply = model_reply.clone();
                let app_id = app_id.clone();
                let gate = gate.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let Ok(read) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..read]);
                        if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length:")
                                        .and_then(|value| value.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if raw.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let request = String::from_utf8_lossy(&raw).into_owned();
                    let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                    records.lock().await.push(request.clone());
                    let response = if path.ends_with("/cosh-oauth/challenge") {
                        let stored = challenge_reply.lock().await.clone();
                        if stored == "AUTO" {
                            let body: serde_json::Value = request
                                .split_once("\r\n\r\n")
                                .and_then(|(_, body)| serde_json::from_str(body).ok())
                                .unwrap_or(serde_json::json!({}));
                            let app = app_id.lock().await.clone();
                            let mut url =
                                reqwest::Url::parse("https://signin.aliyun.com/oauth2/v1/auth")
                                    .unwrap();
                            url.query_pairs_mut()
                                .append_pair("response_type", "code")
                                .append_pair("client_id", &app)
                                .append_pair("redirect_uri", super::contract::REDIRECT_URI)
                                .append_pair("state", body["state"].as_str().unwrap_or(""))
                                .append_pair(
                                    "code_challenge",
                                    body["codeChallenge"].as_str().unwrap_or(""),
                                )
                                .append_pair("code_challenge_method", "S256")
                                .append_pair("scope", "openid aliuid profile")
                                .append_pair(
                                    "nonce",
                                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                                );
                            http_response(
                                "200 OK",
                                &serde_json::json!({"code":"Success","success":true,"data":{
                                    "nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                                    "authorizationUrl":url.to_string()}})
                                .to_string(),
                            )
                        } else {
                            stored
                        }
                    } else if path.ends_with("/cosh-oauth/credentials") {
                        gate.acquire().await.unwrap().forget();
                        credentials_reply.lock().await.clone()
                    } else if path.ends_with("/v1/revoke") {
                        revoke_reply.lock().await.clone()
                    } else if path == super::super::API_PATH {
                        model_reply.lock().await.clone()
                    } else {
                        http_response("404 Not Found", "{}")
                    };
                    if response.is_empty() {
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            endpoint,
            requests,
            challenge_response,
            credentials_response,
            revoke_response,
            model_response,
            client_id,
            credentials_gate,
            task,
        }
    }

    pub async fn wait_for_credentials(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !self
                .requests
                .lock()
                .await
                .iter()
                .any(|request| request.contains("/cosh-oauth/credentials"))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    pub async fn bodies(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .await
            .iter()
            .filter_map(|request| {
                request
                    .split_once("\r\n\r\n")
                    .and_then(|(_, body)| serde_json::from_str(body).ok())
            })
            .collect()
    }
}

impl Drop for OAuthFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
