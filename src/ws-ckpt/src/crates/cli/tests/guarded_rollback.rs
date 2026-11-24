//! Exercises the guarded rollback JSON boundary against a local mock peer.
//!
//! Identity-phase rejections must carry the daemon's code and message; only
//! genuine protocol anomalies and IPC failures keep the generic preflight mask.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ws_ckpt_common::{
    decode_payload, encode_frame, GuardedCheckpointRejectionCodeV2, Request, Response,
    WorkspaceGenerationTokenV2, GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
};

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ckpt-guarded-{}-{}",
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

fn rejection(code: GuardedCheckpointRejectionCodeV2, message: &str) -> Response {
    Response::GuardedCheckpointV2Rejected {
        code,
        message: message.to_owned(),
    }
}

/// Runs `ws-ckpt rollback --guarded --json` against a peer that answers the
/// first request with `response`, returning the CLI output and that request.
fn run_rollback(response: Response, args: &[&str], stdin: Option<&[u8]>) -> (Output, Request) {
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
        // Guarded check/preview/execute send exactly one request before the
        // identity rejection; no follow-up traffic needs a live daemon.
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
    let output = spawn_rollback(&socket, args, stdin);
    (output, peer.join().expect("test peer completed"))
}

/// Runs `ws-ckpt rollback --guarded --json` against `socket`, feeding the
/// optional confirmation JSON on stdin before closing it.
fn spawn_rollback(socket: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ws-ckpt"))
        .args(["rollback", "--guarded", "--json"])
        .args(args)
        .env("WS_CKPT_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ws-ckpt rollback");
    if let Some(input) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input)
            .expect("write guarded confirmation");
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("wait for ws-ckpt rollback")
}

fn stdout_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("rollback JSON on stdout")
}

fn confirmation() -> Vec<u8> {
    let zeros = serde_json::to_value([0u8; 32]).unwrap();
    serde_json::json!({
        "ws_id": "ws-123456",
        "generation": zeros,
        "diff_digest": zeros,
        "operation_id": "op-1",
        "operation_digest": zeros,
    })
    .to_string()
    .into_bytes()
}

#[test]
fn check_passes_through_workspace_not_found() {
    let (output, request) = run_rollback(
        rejection(
            GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
            "workspace is not registered: /tmp/ghost-ws",
        ),
        &["-w", "/tmp/ghost-ws", "--check"],
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        matches!(request, Request::WorkspaceIdentityV2 { registration_path }
        if registration_path == "/tmp/ghost-ws")
    );
    let value = stdout_json(&output);
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "WorkspaceNotFound");
    assert_eq!(
        value["message"],
        "workspace is not registered: /tmp/ghost-ws"
    );
    assert_eq!(value["schema"], "ws-ckpt.rollback.v1");
}

#[test]
fn check_passes_through_invalid_registration_path() {
    // Absolute paths reach the daemon verbatim; '.'/'..' components are the
    // InvalidRegistrationPath trigger actually reachable through the CLI
    // (relative paths are absolutized before sending).
    let (output, request) = run_rollback(
        rejection(
            GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
            "registration path must not contain '.' or '..' components",
        ),
        &["-w", "/tmp/../ghost-ws", "--check"],
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        matches!(request, Request::WorkspaceIdentityV2 { registration_path }
        if registration_path == "/tmp/../ghost-ws")
    );
    let value = stdout_json(&output);
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "InvalidRegistrationPath");
    assert_eq!(
        value["message"],
        "registration path must not contain '.' or '..' components"
    );
}

#[test]
fn preview_identity_rejection_is_not_masked() {
    let (output, _) = run_rollback(
        rejection(
            GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
            "workspace is not registered: /tmp/ghost-ws",
        ),
        &["-w", "/tmp/ghost-ws", "--preview", "-s", "snapshot-1"],
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "WorkspaceNotFound");
    assert_eq!(
        value["message"],
        "workspace is not registered: /tmp/ghost-ws"
    );
}

#[test]
fn execute_identity_rejection_is_known_no_effect() {
    let (output, _) = run_rollback(
        rejection(
            GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
            "workspace is not registered: /tmp/ghost-ws",
        ),
        &["-w", "/tmp/ghost-ws", "-s", "snapshot-1"],
        Some(&confirmation()),
    );
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["status"], "known_no_effect");
    assert_eq!(value["code"], "WorkspaceNotFound");
    assert_eq!(
        value["message"],
        "workspace is not registered: /tmp/ghost-ws"
    );
}

#[test]
fn check_compatible_still_succeeds() {
    let (output, _) = run_rollback(
        Response::WorkspaceIdentityV2Ok {
            protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
            ws_id: "ws-123456".into(),
            registered_path: "/tmp/ws".into(),
            generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
        },
        &["-w", "/tmp/ws", "--check"],
        None,
    );
    assert!(output.status.success(), "{:?}", output);
    let value = stdout_json(&output);
    assert_eq!(value["status"], "compatible");
    assert_eq!(value["ws_id"], "ws-123456");
    assert_eq!(value["registered_path"], "/tmp/ws");
    assert_eq!(value["diff_json"], true);
}

#[test]
fn unexpected_identity_response_keeps_preflight_failed() {
    let (output, _) = run_rollback(
        Response::DeleteOk {
            target: "other".into(),
        },
        &["-w", "/tmp/ghost-ws", "--check"],
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "PreflightFailed");
    assert_eq!(
        value["message"],
        "unexpected response while checking guarded rollback protocol"
    );
}

#[test]
fn connect_failure_keeps_preflight_failed() {
    let dir = SocketDir::new();
    let socket = dir.0.join("daemon.sock");
    let output = spawn_rollback(&socket, &["-w", "/tmp/ghost-ws", "--check"], None);
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "PreflightFailed");
    assert!(value["message"]
        .as_str()
        .unwrap()
        .contains("Failed to connect to ws-ckpt daemon"));
}

#[test]
fn execute_connect_failure_is_known_no_effect() {
    let dir = SocketDir::new();
    let socket = dir.0.join("daemon.sock");
    let output = spawn_rollback(
        &socket,
        &["-w", "/tmp/ghost-ws", "-s", "snapshot-1"],
        Some(&confirmation()),
    );
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["status"], "known_no_effect");
    assert_eq!(value["code"], "PreflightFailed");
    assert!(value["message"]
        .as_str()
        .unwrap()
        .contains("Failed to connect to ws-ckpt daemon"));
}

#[test]
fn execute_unexpected_identity_response_is_known_no_effect() {
    let (output, _) = run_rollback(
        Response::DeleteOk {
            target: "other".into(),
        },
        &["-w", "/tmp/ghost-ws", "-s", "snapshot-1"],
        Some(&confirmation()),
    );
    assert_eq!(output.status.code(), Some(1));
    let value = stdout_json(&output);
    assert_eq!(value["status"], "known_no_effect");
    assert_eq!(value["code"], "PreflightFailed");
    assert_eq!(
        value["message"],
        "unexpected response while checking guarded rollback protocol"
    );
}
