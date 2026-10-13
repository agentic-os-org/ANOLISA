// SPDX-License-Identifier: Apache-2.0
//! UDS HTTP client for the `blazed` CLI subcommands.
//!
//! One connection per call: the CLI fires exactly one request per
//! invocation, so a pooled client would only add construction cost.
//! Requests go to the same hand-rolled HTTP/1.1 API the daemon serves
//! (`api.rs`), over the UDS socket named by `--socket`.

use std::path::Path;

use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::Value;

use crate::error::{BlazeDaemonError, Result};

/// Connects to the daemon bound to `socket` and performs one JSON request.
async fn request_json(socket: &Path, method: Method, path: &str) -> Result<Value> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|source| BlazeDaemonError::SocketConnect {
            socket: socket.to_path_buf(),
            source,
        })?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await?;
    // The connection driver must be polled for the request to complete; it
    // ends by itself once the single request/response pair is done.
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::debug!(?err, "client connection closed with error");
        }
    });
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::<Bytes>::default())?;
    let response = sender.send_request(request).await?;
    let status = response.status().as_u16();
    let body = response.into_body().collect().await?.to_bytes();
    if !(200..300).contains(&status) {
        return Err(BlazeDaemonError::HttpStatus {
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&body)?)
}

/// Requests `POST /v1/admin/reload` on the daemon bound to `socket`.
pub(crate) async fn admin_reload(socket: &Path) -> Result<Value> {
    request_json(socket, Method::POST, "/v1/admin/reload").await
}

/// Probes `GET /v1/health` on the daemon bound to `socket`.
pub(crate) async fn health(socket: &Path) -> Result<Value> {
    request_json(socket, Method::GET, "/v1/health").await
}

/// One doctor line for socket reachability. Never fails: doctor reports,
/// it does not gate — an unreachable daemon is a finding, not an error.
pub(crate) async fn doctor_socket_line(socket: &Path) -> String {
    match health(socket).await {
        Ok(value) => {
            let status = value.get("status").and_then(Value::as_str).unwrap_or("ok");
            format!("socket reachability : ok ({status})")
        }
        Err(error) => format!("socket reachability : unreachable ({error})"),
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper::{Request, Response, StatusCode};
    use serde_json::json;

    use super::*;

    /// Stub handler: (method, path) -> (status, JSON body).
    type Stub = Arc<dyn Fn(&str, &str) -> (StatusCode, String) + Send + Sync>;

    /// Spawns a UDS HTTP server answering every request with the handler's
    /// result and records the requests it saw. Returns the tempdir (keep it
    /// alive) and the socket path.
    async fn spawn_stub(
        handler: Stub,
        seen: Arc<Mutex<Vec<(String, String)>>>,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _peer)) = listener.accept().await else {
                    break;
                };
                let handler = Arc::clone(&handler);
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let handler = Arc::clone(&handler);
                        let seen = Arc::clone(&seen);
                        async move {
                            // Drain the request body so the connection ends cleanly.
                            let (parts, body) = req.into_parts();
                            let _ = body.collect().await;
                            let method = parts.method.as_str().to_string();
                            let path = parts.uri.path().to_string();
                            seen.lock().unwrap().push((method.clone(), path.clone()));
                            let (status, body) = handler(&method, &path);
                            let response = Response::builder()
                                .status(status)
                                .header(CONTENT_TYPE, "application/json")
                                .body(Full::new(Bytes::from(body)))
                                .expect("valid stub response");
                            Ok::<_, Infallible>(response)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        (dir, socket)
    }

    #[tokio::test]
    async fn admin_reload_returns_the_reload_response() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(|_method: &str, path: &str| {
            assert_eq!(path, "/v1/admin/reload");
            (
                StatusCode::OK,
                r#"{"reloaded":true,"policies":3}"#.to_string(),
            )
        });
        let (_dir, socket) = spawn_stub(handler, Arc::clone(&seen)).await;
        let value = admin_reload(&socket).await.unwrap();
        assert_eq!(value, json!({"reloaded": true, "policies": 3}));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![("POST".to_string(), "/v1/admin/reload".to_string())],
            "reload must call the admin endpoint exactly once"
        );
    }

    #[tokio::test]
    async fn health_reports_reachability() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(|method: &str, path: &str| {
            assert_eq!((method, path), ("GET", "/v1/health"));
            (StatusCode::OK, r#"{"status":"ok"}"#.to_string())
        });
        let (_dir, socket) = spawn_stub(handler, Arc::clone(&seen)).await;
        let value = health(&socket).await.unwrap();
        assert_eq!(value["status"], "ok");
    }

    #[tokio::test]
    async fn admin_reload_on_a_missing_socket_reports_socket_connect() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.sock");
        let error = admin_reload(&missing).await.unwrap_err();
        match &error {
            BlazeDaemonError::SocketConnect { socket, .. } => assert_eq!(socket, &missing),
            other => panic!("expected SocketConnect, got {other:?}"),
        }
        assert!(
            error.to_string().contains("Is the daemon running?"),
            "the error must carry the operator hint: {error}"
        );
    }

    #[tokio::test]
    async fn admin_reload_surfaces_http_error_status() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(|_method: &str, _path: &str| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "policy reload failed".to_string(),
            )
        });
        let (_dir, socket) = spawn_stub(handler, Arc::clone(&seen)).await;
        let error = admin_reload(&socket).await.unwrap_err();
        match &error {
            BlazeDaemonError::HttpStatus { status, body } => {
                assert_eq!(*status, 500);
                assert_eq!(body, "policy reload failed");
            }
            other => panic!("expected HttpStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn doctor_socket_line_distinguishes_live_and_dead_sockets() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(|_method: &str, _path: &str| {
            (StatusCode::OK, r#"{"status":"ok"}"#.to_string())
        });
        let (_dir, socket) = spawn_stub(handler, Arc::clone(&seen)).await;
        let live = doctor_socket_line(&socket).await;
        assert!(live.starts_with("socket reachability : ok"), "{live}");

        let dir = tempfile::tempdir().unwrap();
        let dead = dir.path().join("dead.sock");
        let report = doctor_socket_line(&dead).await;
        assert!(
            report.starts_with("socket reachability : unreachable"),
            "{report}"
        );
        assert!(
            report.contains("Is the daemon running?"),
            "the unreachable line must carry the operator hint: {report}"
        );
    }
}
