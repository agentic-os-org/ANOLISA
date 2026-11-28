use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use ws_ckpt_common::{
    decode_payload, encode_frame, ErrorCode, GuardedRollbackRejectionCodeV2, RecoveryPreview,
    Request, Response, StatusReport, UnavailableWorkspaceInfo, WorkspaceGenerationTokenV2,
    WorkspaceInfo, GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
};

struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ws-ckpt-output-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_cli(socket_dir: &SocketDir, args: &[&str], responses: Vec<Response>) -> Output {
    let output = run_cli_with_input(socket_dir, args, responses, None);
    assert!(output.status.success(), "{output:?}");
    output
}

fn run_cli_with_input(
    socket_dir: &SocketDir,
    args: &[&str],
    responses: Vec<Response>,
    input: Option<&Value>,
) -> Output {
    let replies = responses.into_iter().map(SocketReply::from);
    run_cli_with_replies(socket_dir, args, replies, input)
}

enum SocketReply {
    Response(Box<Response>),
    CloseAfter(&'static [u8]),
}

impl From<Response> for SocketReply {
    fn from(response: Response) -> Self {
        Self::Response(Box::new(response))
    }
}

fn run_cli_with_replies(
    socket_dir: &SocketDir,
    args: &[&str],
    replies: impl IntoIterator<Item = SocketReply>,
    input: Option<&Value>,
) -> Output {
    let socket = socket_dir.0.join("ckpt.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ws-ckpt"))
        .env("WS_CKPT_SOCKET", &socket)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        serde_json::to_writer(stdin, input).unwrap();
    } else {
        drop(stdin);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    for response in replies {
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if child.try_wait().unwrap().is_some() || std::time::Instant::now() >= deadline
                    {
                        let _ = child.kill();
                        let output = child.wait_with_output().unwrap();
                        panic!("CLI stopped before sending its request: {output:?}");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("IPC accept failed: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut len = [0; 4];
        stream.read_exact(&mut len).unwrap();
        let mut payload = vec![0; u32::from_le_bytes(len) as usize];
        stream.read_exact(&mut payload).unwrap();
        let request: Request = decode_payload(&payload).unwrap();
        let response = match response {
            SocketReply::Response(response) => *response,
            SocketReply::CloseAfter(bytes) => {
                assert!(matches!(request, Request::WorkspaceStatus { .. }));
                stream.write_all(bytes).unwrap();
                continue;
            }
        };
        match &response {
            Response::WorkspaceStatusOk { .. } => {
                assert!(matches!(request, Request::WorkspaceStatus { .. }));
            }
            Response::StatusOk { .. } => assert!(matches!(request, Request::Status { .. })),
            Response::RollbackOk { to, .. } => {
                assert!(matches!(request, Request::Rollback { to: Some(id), .. } if id == *to));
            }
            Response::DeleteOk { target } => {
                assert!(matches!(request, Request::Delete { snapshot, .. } if snapshot == *target));
            }
            Response::WorkspaceIdentityV2Ok {
                registered_path, ..
            } => {
                assert!(
                    matches!(request, Request::WorkspaceIdentityV2 { registration_path }
                    if registration_path == *registered_path)
                );
            }
            Response::GuardedRollbackV2Rejected { .. } => {
                assert!(matches!(request, Request::GuardedRollbackV2 {
                    target_snapshot_id, operation_id, ..
                } if target_snapshot_id == "s1" && operation_id == "op-123"));
            }
            Response::GuardedRollbackEvidenceV2Ok { .. } => {
                assert!(matches!(request, Request::GuardedRollbackEvidenceV2 {
                    ws_id, operation_id, ..
                } if ws_id == "ws-abc123" && operation_id == "op-123"));
            }
            _ => {}
        }
        stream.write_all(&encode_frame(&response).unwrap()).unwrap();
    }
    drop(listener);
    std::fs::remove_file(socket).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn rollback_json_reports_plain_success_preview_and_rejection() {
    let socket_dir = SocketDir::new();
    let args = ["rollback", "--json", "-w", "ws-abc123", "-s", "s1"];
    let output = run_cli(
        &socket_dir,
        &args,
        vec![Response::RollbackOk {
            from: "ws-abc123".into(),
            to: "s1".into(),
        }],
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["schema"], "ws-ckpt.rollback.v1");
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["snapshot_id"], "s1");
    assert_eq!(body["workspace"], "ws-abc123");

    let output = run_cli(
        &socket_dir,
        &[
            "rollback",
            "--json",
            "--preview",
            "-w",
            "/workspace",
            "-s",
            "s1",
        ],
        vec![Response::RollbackPreviewOk {
            to: "s1".into(),
            changes: vec![],
        }],
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["status"], "preview");
    assert_eq!(body["changes"], json!([]));

    for (code, expected) in [
        (ErrorCode::CwdOccupied, "known_no_effect"),
        (ErrorCode::DiskSpaceInsufficient, "known_no_effect"),
        (ErrorCode::InternalError, "unknown"),
    ] {
        let code_name = format!("{code:?}");
        let output = run_cli_with_input(
            &socket_dir,
            &args,
            vec![Response::Error {
                code,
                message: code_name.clone(),
            }],
            None,
        );
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!output.status.success());
        assert_eq!(body["status"], expected);
        assert_eq!(body["code"], code_name);
    }
}

#[test]
fn rollback_guarded_checks_protocol_and_uses_operation_evidence() {
    let socket_dir = SocketDir::new();
    let identity = |version| Response::WorkspaceIdentityV2Ok {
        protocol_version: version,
        ws_id: "ws-abc123".into(),
        registered_path: "/workspace".into(),
        generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
    };
    let check = [
        "rollback",
        "--guarded",
        "--json",
        "--check",
        "-w",
        "/workspace",
    ];
    let output = run_cli(
        &socket_dir,
        &check,
        vec![identity(GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2)],
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["status"], "compatible");
    assert_eq!(body["ws_id"], "ws-abc123");
    assert_eq!(body["diff_json"], true);

    let output = run_cli_with_input(&socket_dir, &check, vec![identity(99)], None);
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success());
    assert_eq!(body["status"], "error");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("protocol version 99"));

    let confirmation = json!({
        "ws_id": "ws-abc123",
        "generation": ([7_u8; 32]),
        "diff_digest": ([8_u8; 32]),
        "operation_id": "op-123",
        "operation_digest": ([9_u8; 32]),
    });
    let output = run_cli_with_input(
        &socket_dir,
        &[
            "rollback",
            "--guarded",
            "--json",
            "-w",
            "/workspace",
            "-s",
            "s1",
        ],
        vec![
            identity(GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2),
            Response::GuardedRollbackV2Rejected {
                code: GuardedRollbackRejectionCodeV2::DiffMismatch,
                message: "preview is stale".into(),
            },
        ],
        Some(&confirmation),
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success());
    assert_eq!(body["status"], "known_no_effect");
    assert_eq!(body["code"], "DiffMismatch");

    let output = run_cli_with_input(
        &socket_dir,
        &[
            "rollback",
            "--guarded",
            "--json",
            "--result",
            "-w",
            "ws-abc123",
        ],
        vec![Response::GuardedRollbackEvidenceV2Ok { evidence: None }],
        Some(&confirmation),
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success());
    assert_eq!(body["status"], "result");
    assert!(body["evidence"].is_null());
}

#[test]
fn status_reports_unavailable_workspaces_with_unknown_snapshot_count() {
    let socket_dir = SocketDir::new();
    let reason = "parse snapshot index\nline 1";
    for scoped in [false, true] {
        let replies = || {
            vec![
                Response::WorkspaceStatusOk {
                    report: StatusReport {
                        uptime_secs: 1,
                        workspaces: (!scoped)
                            .then(|| WorkspaceInfo {
                                ws_id: "ws-b".into(),
                                path: "/ready".into(),
                                snapshot_count: 2,
                            })
                            .into_iter()
                            .collect(),
                        fs_total_bytes: 0,
                        fs_used_bytes: 0,
                    },
                    unavailable_workspaces: vec![UnavailableWorkspaceInfo {
                        ws_id: "ws-a".into(),
                        path: "/unavailable".into(),
                        reason: reason.into(),
                    }],
                },
                Response::WatcherHealthOk {
                    workspaces_without_watcher: vec![],
                    watcher_queue_overflows: 0,
                },
            ]
        };
        let mut args = vec!["status"];
        if scoped {
            args.extend(["-w", "ws-a"]);
        }
        let output = run_cli(&socket_dir, &args, replies());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("ws-a") && stdout.contains("/unavailable"),
            "{stdout}"
        );
        assert!(
            stdout.contains(r"unknown (unavailable: parse snapshot index\nline 1)"),
            "{stdout}"
        );
        assert_eq!(stdout.contains("ws-b"), !scoped);
        assert!(!stdout.contains("No workspaces registered"));

        args.extend(["--format", "json"]);
        let output = run_cli(&socket_dir, &args, replies());
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            body["workspaces"].as_array().unwrap().len(),
            usize::from(!scoped)
        );
        assert_eq!(
            body["unavailable_workspaces"],
            json!([{
                "ws_id": "ws-a", "path": "/unavailable", "reason": reason,
            }])
        );
    }
}

#[test]
fn status_falls_back_when_legacy_daemon_closes_unknown_request() {
    let socket_dir = SocketDir::new();
    let output = run_cli_with_replies(
        &socket_dir,
        &["status", "-w", "ws-old", "--format", "json"],
        [
            SocketReply::CloseAfter(&[]),
            SocketReply::from(Response::StatusOk {
                report: StatusReport {
                    uptime_secs: 1,
                    workspaces: vec![WorkspaceInfo {
                        ws_id: "ws-old".into(),
                        path: "/old".into(),
                        snapshot_count: 3,
                    }],
                    fs_total_bytes: 0,
                    fs_used_bytes: 0,
                },
            }),
            SocketReply::from(Response::WatcherHealthOk {
                workspaces_without_watcher: vec![],
                watcher_queue_overflows: 0,
            }),
        ],
        None,
    );
    assert!(output.status.success(), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["workspaces"][0]["snapshot_count"], 3);
    assert!(body.get("unavailable_workspaces").is_none());
}

#[test]
fn status_does_not_retry_after_receiving_a_truncated_frame() {
    let socket_dir = SocketDir::new();
    for (bytes, error) in [
        (&[16, 0][..], "failed to read response length"),
        (&[16, 0, 0, 0, 1, 0][..], "failed to read response payload"),
    ] {
        let output = run_cli_with_replies(
            &socket_dir,
            &["status"],
            [SocketReply::CloseAfter(bytes)],
            None,
        );
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(error), "{stderr}");
    }
}

#[test]
fn status_and_snapshot_receipts_escape_controls() {
    let socket_dir = SocketDir::new();
    let id = "old\nname\t\u{1b}";
    let path = "/tmp/工作区\nnext\t\\tail";
    let report = || StatusReport {
        uptime_secs: 1,
        workspaces: vec![WorkspaceInfo {
            ws_id: "ws".into(),
            path: path.into(),
            snapshot_count: 1,
        }],
        fs_total_bytes: 0,
        fs_used_bytes: 0,
    };
    let status = || {
        vec![
            Response::WorkspaceStatusOk {
                unavailable_workspaces: vec![],
                report: report(),
            },
            Response::WatcherHealthOk {
                workspaces_without_watcher: vec![],
                watcher_queue_overflows: 0,
            },
        ]
    };
    let output = run_cli(&socket_dir, &["status"], status());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.lines().filter(|line| line.contains("/tmp/")).count(),
        1
    );
    assert!(stdout.contains(r"/tmp/工作区\nnext\t\\tail"), "{stdout:?}");
    let output = run_cli(&socket_dir, &["status", "--format", "json"], status());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["workspaces"][0]["path"], path);

    let receipts = [
        (
            vec!["rollback", "-w", "ws", "-s", id],
            Response::RollbackOk {
                from: "ws".into(),
                to: id.into(),
            },
            r"✓ Rolled back workspace ws to snapshot: old\nname\t\u{1b}",
        ),
        (
            vec!["delete", "-s", id],
            Response::DeleteOk { target: id.into() },
            r"✓ Deleted: old\nname\t\u{1b}",
        ),
    ];
    for (args, response, expected) in receipts {
        let output = run_cli(&socket_dir, &args, vec![response]);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 2, "{stdout:?}");
        assert_eq!(
            stdout.lines().next().unwrap(),
            format!("\x1b[32m{expected}\x1b[0m")
        );
    }

    let preview = Response::RecoverPreviewOk {
        preview: RecoveryPreview {
            ws_id: Some("ws".into()),
            registration_path: path.into(),
            snapshot_count: 1,
            confirmation_digest: [0; 32],
        },
    };
    let warning = Response::RecoverWithWarning {
        workspace: path.into(),
        warning: "\nLeftover paths:\n- first\\npath\n- second\n\nInspect them before removal."
            .into(),
    };
    let expected =
        b"WARNING: Leftover paths:\n- first\\npath\n- second\n\nInspect them before removal.\n";
    let output = run_cli(
        &socket_dir,
        &["recover", "-w", "ws", "--force"],
        vec![preview.clone(), warning.clone()],
    );
    assert_eq!(output.stderr, expected);
    let output = run_cli(
        &socket_dir,
        &["recover", "--all", "--force"],
        vec![
            Response::StatusOk { report: report() },
            preview.clone(),
            warning,
        ],
    );
    assert_eq!(output.stderr, expected);

    let legacy_warning = Response::RecoverWithWarning {
        workspace: path.into(),
        warning: "Leftover path: first\npath".into(),
    };
    let output = run_cli(
        &socket_dir,
        &["recover", "-w", "ws", "--force"],
        vec![preview, legacy_warning],
    );
    assert_eq!(output.stderr, b"WARNING: Leftover path: first\\npath\n");
}
