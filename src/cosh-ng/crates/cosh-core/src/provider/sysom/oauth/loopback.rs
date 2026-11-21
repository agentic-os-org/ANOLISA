//! Fixed loopback callback listener for the OAuth browser handoff.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::contract::{OAuthError, CALLBACK_PATH};

/// Per-connection read bound; half-open connections only close themselves.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Request head cap for the callback listener.
pub const MAX_REQUEST: usize = 8 * 1024;
const PAGE: &str = "Login result received. Return to the terminal to view it.";

/// State-matched browser result.
pub struct CallbackOutcome {
    /// Authorization code on success.
    pub code: Option<String>,
    /// OAuth error parameter on a matched denial.
    pub error: Option<String>,
}

/// Fixed loopback listener accepting exactly one valid callback.
pub struct CallbackListener {
    listener: TcpListener,
}

impl CallbackListener {
    /// Binds the loopback callback port; `PortInUse` surfaces immediately.
    pub async fn bind(port: u16) -> Result<Self, OAuthError> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    OAuthError::PortInUse
                } else {
                    OAuthError::Unavailable
                }
            })?;
        Ok(Self { listener })
    }

    /// Actual bound port (ephemeral in tests).
    pub fn port(&self) -> u16 {
        self.listener
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or(0)
    }

    /// Wait for one state-matched authorization result. Invalid requests only
    /// close their own connection; the listener keeps waiting until the deadline.
    pub async fn wait(
        self,
        expected_state: &str,
        overall: Duration,
        read_timeout: Duration,
    ) -> Result<CallbackOutcome, OAuthError> {
        let deadline = tokio::time::Instant::now() + overall;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(OAuthError::CallbackTimeout);
            }
            let accepted = tokio::time::timeout(remaining, self.listener.accept()).await;
            let (mut socket, _) = match accepted {
                Ok(Ok(pair)) => pair,
                Ok(Err(_)) => return Err(OAuthError::Unavailable),
                Err(_) => return Err(OAuthError::CallbackTimeout),
            };
            if let Some(outcome) =
                handle_connection(&mut socket, expected_state, read_timeout).await
            {
                return Ok(outcome);
            }
        }
    }
}

async fn handle_connection(
    socket: &mut TcpStream,
    expected_state: &str,
    read_timeout: Duration,
) -> Option<CallbackOutcome> {
    let request = read_request_head(socket, read_timeout).await?;
    let mut lines = request.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" || !target.starts_with('/') {
        respond(socket, "405 Method Not Allowed").await;
        return None;
    }
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != CALLBACK_PATH {
        respond(socket, "404 Not Found").await;
        return None;
    }
    let mut params: Vec<(String, String)> = Vec::new();
    for (key, value) in url.query_pairs() {
        if params.iter().any(|(existing, _)| existing == key.as_ref()) {
            respond(socket, "400 Bad Request").await;
            return None;
        }
        params.push((key.into_owned(), value.into_owned()));
    }
    let get = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if get("state") != Some(expected_state) {
        respond(socket, "400 Bad Request").await;
        return None;
    }
    if let Some(error) = get("error").filter(|value| !value.is_empty()) {
        respond(socket, "200 OK").await;
        return Some(CallbackOutcome {
            code: None,
            error: Some(error.to_owned()),
        });
    }
    if let Some(code) = get("code").filter(|value| !value.is_empty()) {
        respond(socket, "200 OK").await;
        return Some(CallbackOutcome {
            code: Some(code.to_owned()),
            error: None,
        });
    }
    respond(socket, "400 Bad Request").await;
    None
}

async fn read_request_head(socket: &mut TcpStream, read_timeout: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + read_timeout;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let read = tokio::time::timeout(remaining, socket.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() > MAX_REQUEST {
            return None;
        }
        if raw.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(raw).ok();
        }
    }
}

async fn respond(socket: &mut TcpStream, status: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{PAGE}",
        PAGE.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send(port: u16, request: &str) -> String {
        let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut response)).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    fn get(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
    }

    #[tokio::test]
    async fn valid_callback_returns_code_with_hardened_page() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let port = listener.port();
        let waiter = tokio::spawn(listener.wait("state-1", Duration::from_secs(5), READ_TIMEOUT));
        let response = send(port, &get("/callback?state=state-1&code=code-1")).await;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.to_lowercase().contains("cache-control: no-store"));
        assert!(response
            .to_lowercase()
            .contains("referrer-policy: no-referrer"));
        assert!(!response.contains("code-1"));
        let outcome = waiter.await.unwrap().unwrap();
        assert_eq!(outcome.code.as_deref(), Some("code-1"));
        assert!(outcome.error.is_none());
    }

    #[tokio::test]
    async fn wrong_state_wrong_path_and_method_keep_waiting() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let port = listener.port();
        let waiter = tokio::spawn(listener.wait("state-1", Duration::from_secs(5), READ_TIMEOUT));
        assert!(send(port, &get("/callback?state=other&code=x"))
            .await
            .starts_with("HTTP/1.1 400"));
        assert!(send(port, &get("/elsewhere?state=state-1&code=x"))
            .await
            .starts_with("HTTP/1.1 404"));
        assert!(send(
            port,
            "POST /callback?state=state-1&code=x HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n"
        )
        .await
        .starts_with("HTTP/1.1 405"));
        assert!(send(port, &get("/callback?state=state-1"))
            .await
            .starts_with("HTTP/1.1 400"));
        assert!(
            send(port, &get("/callback?state=state-1&state=state-1&code=x"))
                .await
                .starts_with("HTTP/1.1 400")
        );
        send(port, &get("/callback?state=state-1&code=final")).await;
        assert_eq!(
            waiter.await.unwrap().unwrap().code.as_deref(),
            Some("final")
        );
    }

    #[tokio::test]
    async fn matched_denial_terminates_the_listener() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let port = listener.port();
        let waiter = tokio::spawn(listener.wait("state-1", Duration::from_secs(5), READ_TIMEOUT));
        send(port, &get("/callback?state=state-1&error=access_denied")).await;
        let outcome = waiter.await.unwrap().unwrap();
        assert!(outcome.code.is_none());
        assert_eq!(outcome.error.as_deref(), Some("access_denied"));
    }

    #[tokio::test]
    async fn oversized_and_half_open_connections_only_close_themselves() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let port = listener.port();
        let waiter = tokio::spawn(listener.wait(
            "state-1",
            Duration::from_secs(5),
            Duration::from_millis(200),
        ));
        let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        socket
            .write_all(
                format!(
                    "GET /callback?state=state-1&code=x HTTP/1.1\r\nX-Pad: {}\r\n\r\n",
                    "a".repeat(MAX_REQUEST)
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut response)).await;
        let mut half_open = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        half_open
            .write_all(b"GET /callback?state=state-1")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        send(port, &get("/callback?state=state-1&code=after-abuse")).await;
        assert_eq!(
            waiter.await.unwrap().unwrap().code.as_deref(),
            Some("after-abuse")
        );
    }

    #[tokio::test]
    async fn port_in_use_and_overall_deadline_are_distinct_errors() {
        let first = CallbackListener::bind(0).await.unwrap();
        let port = first.port();
        assert!(matches!(
            CallbackListener::bind(port).await,
            Err(OAuthError::PortInUse)
        ));
        let outcome = first
            .wait("state-1", Duration::from_millis(100), READ_TIMEOUT)
            .await;
        assert!(matches!(outcome, Err(OAuthError::CallbackTimeout)));
    }

    #[tokio::test]
    async fn percent_encoded_code_is_decoded_once() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let port = listener.port();
        let waiter = tokio::spawn(listener.wait("state-1", Duration::from_secs(5), READ_TIMEOUT));
        send(port, &get("/callback?state=state-1&code=a%2Bb%2Fc%3D")).await;
        assert_eq!(
            waiter.await.unwrap().unwrap().code.as_deref(),
            Some("a+b/c=")
        );
    }
}
