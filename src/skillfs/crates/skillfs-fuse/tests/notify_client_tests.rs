//! Integration tests for the N2 Unix socket notify client.
//!
//! Each test spawns a fake daemon listening on a temporary Unix socket,
//! exercises `UnixSocketNotifyClient::send`, and validates both the
//! request JSON and the client's response handling.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use skillfs_fuse::security::{
    NotifyChangeEvent, NotifyClient, NotifyError, NotifyEventKind, UnixSocketNotifyClient,
};

/// Spawn a fake daemon that reads one NDJSON request, runs `handler` to
/// produce a response, and writes the response back. Returns the socket
/// path. Blocks until the socket file exists so the caller can connect
/// immediately.
fn spawn_fake_daemon(
    handler: impl Fn(&str) -> String + Send + 'static,
) -> (PathBuf, std::thread::JoinHandle<Option<String>>) {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.keep().join("daemon.sock");
    let sock_path_clone = sock_path.clone();

    let ready = Arc::new(std::sync::Barrier::new(2));
    let ready_clone = ready.clone();

    let handle = std::thread::spawn(move || {
        let listener = UnixListener::bind(&sock_path_clone).unwrap();
        ready_clone.wait();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response = handler(line.trim());
        let mut writer = std::io::BufWriter::new(&stream);
        writer.write_all(response.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
        Some(line)
    });

    ready.wait();
    (sock_path, handle)
}

fn make_event() -> NotifyChangeEvent {
    NotifyChangeEvent::new(
        "/srv/skills/category/weather",
        "category/weather",
        NotifyEventKind::Write,
        vec!["SKILL.md".to_string()],
        5000,
    )
}

#[test]
fn notify_client_accepted() {
    let (sock_path, daemon) = spawn_fake_daemon(|req_json| {
        let parsed: serde_json::Value = serde_json::from_str(req_json).unwrap();
        assert_eq!(parsed["method"], "skill_ledger.skillfs_notify_change");
        let params = parsed["params"].as_object().unwrap();
        assert_eq!(params.len(), 5);
        assert_eq!(parsed["params"]["schemaVersion"], 2);
        assert_eq!(
            parsed["params"]["canonicalSkillDir"],
            "/srv/skills/category/weather"
        );
        assert_eq!(parsed["params"]["skillId"], "category/weather");
        assert_eq!(parsed["params"]["eventKind"], "write");
        assert_eq!(parsed["params"]["paths"], serde_json::json!(["SKILL.md"]));
        for forbidden in [
            "skillDir",
            "skillName",
            "mountId",
            "generation",
            "resolverSocket",
            "sourceId",
        ] {
            assert!(
                !params.contains_key(forbidden),
                "unexpected field: {forbidden}"
            );
        }
        assert!(!parsed["id"].as_str().unwrap().is_empty());
        assert_eq!(parsed["trace_context"], serde_json::json!({}));
        assert_eq!(parsed["timeout_ms"], 5000);

        r#"{"id":"resp-1","ok":true,"data":{"schemaVersion":2,"accepted":true},"stdout":"","stderr":"","exit_code":0}"#.to_string()
    });

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
    let result = client.send(&make_event());
    assert!(result.is_ok(), "accepted response must succeed: {result:?}");

    daemon.join().unwrap();
    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
}

#[test]
fn notify_client_ok_false() {
    let (sock_path, daemon) = spawn_fake_daemon(|_| {
        r#"{"ok":false,"error":{"code":"method_not_found","message":"unknown method"}}"#.to_string()
    });

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
    let result = client.send(&make_event());
    assert!(
        matches!(result, Err(NotifyError::Rejected { .. })),
        "ok=false must be Rejected, got {result:?}"
    );

    daemon.join().unwrap();
    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
}

#[test]
fn notify_client_invalid_response() {
    let (sock_path, daemon) = spawn_fake_daemon(|_| "this is not json at all".to_string());

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
    let result = client.send(&make_event());
    assert!(
        matches!(result, Err(NotifyError::InvalidResponse { .. })),
        "garbled response must be InvalidResponse, got {result:?}"
    );

    daemon.join().unwrap();
    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
}

#[test]
fn notify_client_daemon_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("no-daemon.sock");

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(1));
    let result = client.send(&make_event());
    assert!(
        matches!(result, Err(NotifyError::Connect(_))),
        "no listener must be Connect error, got {result:?}"
    );
}

#[test]
fn notify_client_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.keep().join("slow-daemon.sock");
    let sock_path_clone = sock_path.clone();

    let ready = Arc::new(std::sync::Barrier::new(2));
    let ready_clone = ready.clone();

    let daemon = std::thread::spawn(move || {
        let listener = UnixListener::bind(&sock_path_clone).unwrap();
        ready_clone.wait();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        std::thread::sleep(Duration::from_secs(5));
        drop(stream);
    });

    ready.wait();

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_millis(200));
    let result = client.send(&make_event());
    assert!(
        matches!(result, Err(NotifyError::Timeout | NotifyError::Read(_))),
        "slow daemon must timeout, got {result:?}"
    );

    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
    drop(daemon);
}

#[test]
fn notify_client_accepted_false_is_rejected() {
    let (sock_path, daemon) = spawn_fake_daemon(|_| {
        r#"{"ok":true,"data":{"schemaVersion":2,"accepted":false}}"#.to_string()
    });

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
    let result = client.send(&make_event());
    assert!(
        matches!(result, Err(NotifyError::Rejected { .. })),
        "accepted=false must be Rejected, got {result:?}"
    );

    daemon.join().unwrap();
    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
}

#[test]
fn notify_client_v1_response_is_rejected() {
    let (sock_path, daemon) = spawn_fake_daemon(|_| {
        r#"{"ok":true,"data":{"schemaVersion":1,"accepted":true}}"#.to_string()
    });

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
    let result = client.send(&make_event());
    assert!(
        matches!(result, Err(NotifyError::InvalidResponse { .. })),
        "v1 response must be rejected, got {result:?}"
    );

    daemon.join().unwrap();
    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
}
