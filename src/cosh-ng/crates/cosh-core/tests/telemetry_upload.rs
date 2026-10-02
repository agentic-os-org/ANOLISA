//! End-to-end self-upload telemetry test against the real cosh-core binary.
//!
//! Verifies that the standalone SLS self-upload path emits a POST with the
//! expected path and body when the unified cosh.jsonl channel is absent. All
//! other binary tests opt out of telemetry by default; this is the single
//! exception that intentionally exercises the full upload path against a
//! local mock server.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

mod common;

fn binary_path() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target profile directory")
        .to_path_buf();
    path.push("cosh-core");
    path
}

/// A mock HTTP server that records the first complete request it receives
/// and replies with 200 OK.
struct MockServer {
    address: std::net::SocketAddr,
    receiver: mpsc::Receiver<Vec<u8>>,
}

/// Read a complete HTTP/1.1 request from `stream`.
///
/// Parses the headers to find `Content-Length`, then reads the full body.
/// This mirrors the robust read loop in `sls::fetch_region_id_from_metadata`
/// and prevents the test from replying after seeing only a partial request.
fn read_http_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut temp = [0u8; 1024];

    // Read until the header terminator appears.
    loop {
        let n = stream.read(&mut temp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            break;
        }
    }

    // Parse Content-Length so we read the body even when it arrives in a
    // later TCP packet than the headers.
    let content_length = {
        let header_end = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(buf.len());
        let headers = String::from_utf8_lossy(&buf[..header_end]);
        headers
            .lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .next()
            .unwrap_or(0)
    };

    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let body_so_far = buf.len().saturating_sub(header_end);
    let mut remaining = content_length.saturating_sub(body_so_far);

    while remaining > 0 {
        let to_read = std::cmp::min(remaining, temp.len());
        let n = stream.read(&mut temp[..to_read]).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);
        remaining -= n;
    }

    buf
}

impl MockServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("mock server address");
        let (tx, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let request = read_http_request(&mut stream);
                let _ = tx.send(request);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        Self { address, receiver }
    }

    fn recv_timeout(&self, timeout: Duration) -> Vec<u8> {
        self.receiver
            .recv_timeout(timeout)
            .expect("receive mock server request")
    }
}

#[test]
fn standalone_self_upload_posts_to_local_server() {
    if common::system_telemetry_is_disabled() {
        eprintln!("skipping enabled-telemetry test because the host opted out system-wide");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");

    let config_dir = home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(
        config_dir.join("config.toml"),
        r#"
[ai]
active_provider = "test"

[ai.providers.test]
type = "mock"
model = "mock-history"
"#,
    )
    .expect("write config");

    // Mock metadata server returns a valid region.
    let metadata = TcpListener::bind("127.0.0.1:0").expect("bind metadata server");
    let metadata_addr = metadata.local_addr().expect("metadata address");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = metadata.accept() {
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\ncn-hangzhou");
        }
    });

    // Mock SLS server records the upload request.
    let sls = MockServer::start();

    // Keep the per-user sentinel absent. The system override points at a
    // present file and must be ignored by the production binary; otherwise a
    // user environment could redirect the administrator-controlled sentinel.
    let absent_user_sentinel = home
        .join(".copilot-shell")
        .join("telemetry_disabled_absent");
    let ignored_system_override = home
        .join(".copilot-shell")
        .join("ignored_system_telemetry_disabled_override");
    fs::write(&ignored_system_override, "").expect("write ignored system sentinel override");

    let mut command = Command::new(binary_path());
    command
        .env("HOME", &home)
        .env("COSH_TELEMETRY_DISABLED_PATH", &absent_user_sentinel)
        .env(
            "COSH_SYSTEM_TELEMETRY_DISABLED_PATH",
            &ignored_system_override,
        )
        .env(
            "COSH_METADATA_HOST",
            format!("127.0.0.1:{}", metadata_addr.port()),
        )
        .env(
            "COSH_SLS_TRACK_URL",
            format!(
                "http://127.0.0.1:{}/logstores/cosh/track",
                sls.address.port()
            ),
        )
        .args(["--headless", "--workspace"])
        .arg(&workspace)
        .arg("hello");
    // Do NOT call common::opt_out_telemetry: this test intentionally enables
    // telemetry, but points both the metadata probe and the upload target at
    // local servers so nothing reaches production.
    let output = command.output().expect("run cosh-core");

    assert!(
        output.status.success(),
        "cosh-core failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let request = sls.recv_timeout(Duration::from_secs(5));

    // Verify Content-Length was parsed case-insensitively and the captured
    // body length matches the header.
    let header_end = request
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(request.len());
    let content_length = String::from_utf8_lossy(&request[..header_end])
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .next()
        .unwrap_or(0);
    let body = &request[header_end..];
    assert_eq!(
        body.len(),
        content_length,
        "captured body length must match Content-Length"
    );

    let request_str = String::from_utf8_lossy(&request);
    assert!(
        request_str.contains("POST /logstores/cosh/track "),
        "expected POST to track endpoint, got: {request_str}"
    );
    assert!(
        request_str.contains("cosh_upload_source"),
        "expected cosh_upload_source marker in body, got: {request_str}"
    );
    assert!(
        request_str.contains("cosh-ng-direct"),
        "expected cosh-ng-direct source marker, got: {request_str}"
    );
}
