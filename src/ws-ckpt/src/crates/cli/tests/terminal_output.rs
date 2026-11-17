use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

use ws_ckpt_common::{
    decode_payload, encode_frame, RecoveryPreview, Request, Response, StatusReport, WorkspaceInfo,
};

struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ws-ckpt-output-{}", std::process::id()));
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
    let socket = socket_dir.0.join("ckpt.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ws-ckpt"))
        .env("WS_CKPT_SOCKET", &socket)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    for response in responses {
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
        match &response {
            Response::RollbackOk { to, .. } => {
                assert!(matches!(request, Request::Rollback { to: Some(id), .. } if id == *to));
            }
            Response::DeleteOk { target } => {
                assert!(matches!(request, Request::Delete { snapshot, .. } if snapshot == *target));
            }
            _ => {}
        }
        stream.write_all(&encode_frame(&response).unwrap()).unwrap();
    }
    drop(listener);
    std::fs::remove_file(socket).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

#[test]
fn status_and_snapshot_receipts_escape_controls() {
    let socket_dir = SocketDir::new();
    let id = "old\nname\t\u{1b}";
    let path = "/tmp/工作区\nnext\t\\tail";
    let status = || {
        vec![
            Response::StatusOk {
                report: StatusReport {
                    uptime_secs: 1,
                    workspaces: vec![WorkspaceInfo {
                        ws_id: "ws".into(),
                        path: path.into(),
                        snapshot_count: 1,
                    }],
                    fs_total_bytes: 0,
                    fs_used_bytes: 0,
                },
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
        vec![status().remove(0), preview.clone(), warning],
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
