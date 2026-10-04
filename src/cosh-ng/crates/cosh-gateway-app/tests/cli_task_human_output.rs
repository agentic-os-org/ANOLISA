//! End-to-end human-output checks for mutating `task` subcommands against a
//! fake daemon that speaks the real length-prefixed Gateway protocol.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::thread;

use cosh_gateway::daemon::{GatewayResult, TaskView, GATEWAY_API_VERSION};
use cosh_gateway_contracts::common::{BoundedName, BoundedOpaque, TargetRef};
use cosh_gateway_contracts::ids::{RunId, TaskId};
use cosh_gateway_contracts::task::TaskState;
use serde_json::{json, Value};

fn view(task_id: &TaskId, run_id: &RunId) -> TaskView {
    TaskView {
        task_id: task_id.clone(),
        revision: 9,
        state: TaskState::Suspended,
        active_run_id: Some(run_id.clone()),
        target: TargetRef {
            kind: BoundedName::new("local").unwrap(),
            authority: BoundedName::new("matrix").unwrap(),
            identifier: BoundedOpaque::new("target").unwrap(),
        },
        launch: None,
        baseline: None,
    }
}

/// Serves exactly one framed Gateway request, then answers with `result`.
fn serve_one_result(socket: &Path, result: GatewayResult) {
    let listener = UnixListener::bind(socket).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).unwrap();
        let length = usize::try_from(u32::from_be_bytes(header)).unwrap();
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).unwrap();
        let request: Value = serde_json::from_slice(&payload).unwrap();
        let response = json!({
            "api_version": GATEWAY_API_VERSION,
            "request_id": request["request_id"],
            "status": "ok",
            "result": serde_json::to_value(&result).unwrap(),
        });
        let body = serde_json::to_vec(&response).unwrap();
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
    });
}

fn human_task_output(arguments: &[&str], result: GatewayResult) -> (bool, String) {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("gateway.sock");
    serve_one_result(&socket, result);
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args(["task", "--output", "human", "--socket"])
        .arg(&socket)
        .args(arguments)
        .output()
        .expect("the installed CLI binary must run");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn task_retry_prints_the_resulting_projection_in_human_mode() {
    let task_id = TaskId::new();
    let run_id = RunId::new();
    let (succeeded, stdout) = human_task_output(
        &[
            "retry",
            "tsk_00000000-0000-0000-0000-000000000000",
            "--previous-run-id",
            "run_00000000-0000-0000-0000-000000000001",
            "--idempotency-key",
            "stable-retry-key",
        ],
        GatewayResult::Retried(view(&task_id, &run_id)),
    );

    assert!(succeeded, "task retry must exit successfully");
    assert!(
        !stdout.is_empty(),
        "human output must report the retried Task projection, got stdout={stdout:?}"
    );
    assert!(stdout.contains(task_id.as_str()));
}

#[test]
fn task_append_prints_the_resulting_projection_in_human_mode() {
    let task_id = TaskId::new();
    let run_id = RunId::new();
    let (succeeded, stdout) = human_task_output(
        &[
            "append",
            "tsk_00000000-0000-0000-0000-000000000000",
            "--input-request-id",
            "inp_00000000-0000-0000-0000-000000000001",
            "--select",
            "0",
            "--idempotency-key",
            "stable-input-key",
        ],
        GatewayResult::InputAppended(view(&task_id, &run_id)),
    );

    assert!(succeeded, "task append must exit successfully");
    assert!(
        !stdout.is_empty(),
        "human output must report the appended Task projection, got stdout={stdout:?}"
    );
    assert!(stdout.contains(task_id.as_str()));
}

#[test]
fn task_resolve_approval_prints_the_resulting_projection_in_human_mode() {
    let task_id = TaskId::new();
    let run_id = RunId::new();
    let (succeeded, stdout) = human_task_output(
        &[
            "resolve-approval",
            "apr_00000000-0000-0000-0000-000000000000",
            "--decision",
            "approve",
            "--idempotency-key",
            "stable-approval-key",
        ],
        GatewayResult::ApprovalResolved(view(&task_id, &run_id)),
    );

    assert!(succeeded, "task resolve-approval must exit successfully");
    assert!(
        !stdout.is_empty(),
        "human output must report the resolved Task projection, got stdout={stdout:?}"
    );
    assert!(stdout.contains(task_id.as_str()));
}
