//! Exercises the CLI's stdout, exit status and existing IPC against a local peer.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ws_ckpt_common::{
    decode_payload, encode_frame, ChangeType, DiffEntry, ErrorCode, Request, Response,
};

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ckpt-diff-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create isolated socket directory");
        Self(path)
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove isolated socket directory");
    }
}

fn run_diff(response: Response, args: &[&str]) -> (Output, Request) {
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
        .args(["diff", "-w", "ws-123456", "--from", "snapshot-1"])
        .args(args)
        .env("WS_CKPT_SOCKET", &socket)
        .output()
        .expect("run ws-ckpt diff");
    (output, peer.join().expect("test peer completed"))
}

#[test]
fn json_preserves_file_changes_and_snapshot_request() {
    let changes = vec![
        DiffEntry {
            path: "目录/added file.txt".into(),
            change_type: ChangeType::Added,
            detail: None,
        },
        DiffEntry {
            path: "modified\nfile\t\x1b.txt".into(),
            change_type: ChangeType::Modified,
            detail: Some("content modified".into()),
        },
        DiffEntry {
            path: "deleted.txt".into(),
            change_type: ChangeType::Deleted,
            detail: None,
        },
        DiffEntry {
            path: "new name.txt".into(),
            change_type: ChangeType::Renamed,
            detail: Some("old name.txt → new name.txt".into()),
        },
    ];
    let (output, request) = run_diff(
        Response::DiffOk {
            changes: changes.clone(),
        },
        &["--to", "snapshot-2", "--format", "json"],
    );
    assert!(output.status.success(), "{:?}", output);
    assert!(
        matches!(request, Request::Diff { workspace, from, to: Some(to) }
        if workspace == "ws-123456" && from == "snapshot-1" && to == "snapshot-2")
    );
    // Parsing the entire stdout also rejects timing text or ANSI outside JSON.
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["changes"], serde_json::to_value(changes).unwrap());
    assert!(value["elapsed_secs"].as_f64().unwrap() >= 0.0);
    assert!(output.stderr.is_empty(), "{:?}", output);
}

#[test]
fn json_empty_diff_is_an_empty_array_against_live() {
    let (output, request) = run_diff(Response::DiffOk { changes: vec![] }, &["--format", "json"]);
    assert!(output.status.success(), "{:?}", output);
    assert!(matches!(request, Request::Diff { to: None, .. }));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["changes"], serde_json::json!([]));
    assert!(value["elapsed_secs"].is_number());
}

#[test]
fn text_remains_the_default_and_can_be_selected_explicitly() {
    for args in [vec![], vec!["--format", "text"]] {
        let (output, _) = run_diff(
            Response::DiffOk {
                changes: vec![DiffEntry {
                    path: "dir/file with spaces.txt".into(),
                    change_type: ChangeType::Modified,
                    detail: None,
                }],
            },
            &args,
        );
        assert!(output.status.success(), "{:?}", output);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with("\x1b[33mM  dir/file with spaces.txt\x1b[0m\n\n1 change(s)\n"));
        assert!(stdout.contains("Completed in "));
    }
}

#[test]
fn empty_text_diff_keeps_the_existing_message() {
    let (output, _) = run_diff(Response::DiffOk { changes: vec![] }, &[]);
    assert!(output.status.success(), "{:?}", output);
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .starts_with("No differences found.\nCompleted in "));
}

#[test]
fn daemon_errors_and_unexpected_replies_never_report_success() {
    for args in [vec![], vec!["--format", "json"]] {
        let (output, _) = run_diff(
            Response::Error {
                code: ErrorCode::SnapshotNotFound,
                message: "snapshot-1 is unavailable".into(),
            },
            &args,
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty(), "{:?}", output);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("SnapshotNotFound"));
        assert!(stderr.contains("snapshot-1 is unavailable"));

        let (output, _) = run_diff(
            Response::DeleteOk {
                target: "other".into(),
            },
            &args,
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty(), "{:?}", output);
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("unexpected response type while reading diff"));
    }
}
