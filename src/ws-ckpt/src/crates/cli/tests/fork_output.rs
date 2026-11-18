//! Exercises `fork --format json` against a local peer daemon.
//!
//! The fork result is buffered like every other command's, so `main` injects
//! `elapsed_secs` into it instead of printing a separate timing line. These
//! tests parse the whole of stdout as one JSON document, which is the property
//! that broke when the fork arm printed its object directly.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ws_ckpt_common::{decode_payload, encode_frame, Request, Response};

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ckpt-fork-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create isolated socket directory");
        Self(path)
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_fork(response: Response, args: &[&str]) -> (Output, Request) {
    let dir = SocketDir::new();
    let socket = dir.0.join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind test socket");
    listener.set_nonblocking(true).unwrap();
    let peer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI never connected");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept CLI connection: {error}"),
            }
        };
        // Reject the later best-effort health query without using a real daemon.
        drop(listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut length = [0; 4];
        stream.read_exact(&mut length).unwrap();
        let length = u32::from_le_bytes(length);
        assert!(length <= ws_ckpt_common::MAX_FRAME_SIZE);
        let mut payload = vec![0; length as usize];
        stream.read_exact(&mut payload).unwrap();
        let request = decode_payload(&payload).expect("decode CLI request");
        stream.write_all(&encode_frame(&response).unwrap()).unwrap();
        request
    });
    let output = Command::new(env!("CARGO_BIN_EXE_ws-ckpt"))
        .args(["fork", "-w", "/work/source", "-s", "snap-1"])
        .args(args)
        .env("WS_CKPT_SOCKET", &socket)
        .output()
        .expect("run ws-ckpt fork");
    (output, peer.join().expect("test peer completed"))
}

fn fork_ok(warnings: Vec<String>) -> Response {
    Response::ForkOk {
        ws_id: "ws-forked123456".to_string(),
        path: "/work/forked".to_string(),
        source_ws_id: "ws-source123456".to_string(),
        source_snapshot_id: "snap-1".to_string(),
        initial_snapshot_id: "fork-initial".to_string(),
        warnings,
    }
}

#[test]
fn json_fork_is_one_document_with_timing_inside_it() {
    let (output, request) = run_fork(
        fork_ok(Vec::new()),
        &["--destination", "/work/forked", "--format", "json"],
    );
    assert!(output.status.success(), "{:?}", output);
    assert!(matches!(
        request,
        Request::Fork { destination, snapshot, .. }
            if destination == "/work/forked" && snapshot == "snap-1"
    ));
    // Parsing the entire stdout also rejects a trailing timing line.
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "ws-ckpt/fork/v1");
    assert_eq!(value["ws_id"], "ws-forked123456");
    assert_eq!(value["path"], "/work/forked");
    assert_eq!(value["source"]["ws_id"], "ws-source123456");
    assert_eq!(value["source"]["snapshot_id"], "snap-1");
    assert_eq!(value["initial_snapshot_id"], "fork-initial");
    assert_eq!(value["warnings"], serde_json::json!([]));
    assert!(value["elapsed_secs"].is_number());
    assert!(output.stderr.is_empty(), "{:?}", output);
}

#[test]
fn json_fork_carries_persistence_warnings_in_the_same_document() {
    let warning = "the workspace works, but its registration could not be written";
    let (output, _) = run_fork(
        fork_ok(vec![warning.to_string()]),
        &["--destination", "/work/forked", "--format", "json"],
    );
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["warnings"], serde_json::json!([warning]));
    assert!(value["elapsed_secs"].is_number());
}

#[test]
fn text_fork_reports_warnings_on_stderr() {
    let warning = "the workspace works, but its registration could not be written";
    let (output, _) = run_fork(
        fork_ok(vec![warning.to_string()]),
        &["--destination", "/work/forked"],
    );
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Workspace forked: ws-forked123456"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Forked from: ws-source123456/snap-1"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(warning), "{stderr}");
}
