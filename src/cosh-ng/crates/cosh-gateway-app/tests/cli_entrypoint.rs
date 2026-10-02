//! Installed ACP entrypoint behavior over a deterministic local fake adapter.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn fake_adapter(directory: &tempfile::TempDir) -> std::path::PathBuf {
    let path = directory.path().join("codex-acp");
    fs::write(
        &path,
        r#"#!/bin/sh
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"@agentclientprotocol/codex-acp","title":"Codex","version":"1.6.2"},"_meta":{"jetbrains":{"air":{"version":1,"capabilities":["sessionFailure","agentFileChangeReport"]}}}}}'
            ;;
        2)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"entrypoint-session"}}'
            ;;
        3)
            printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"entrypoint-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"safe\u001b[2Jtext"}}}}'
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn"}}'
            ;;
    esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn permission_adapter(directory: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let path = directory.path().join("codex-acp");
    let response = directory.path().join("permission-response.json");
    let script = r#"#!/bin/sh
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"@agentclientprotocol/codex-acp","title":"Codex","version":"1.6.2"},"_meta":{"jetbrains":{"air":{"version":1,"capabilities":["sessionFailure","agentFileChangeReport"]}}}}}'
            ;;
        2)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"permission-session"}}'
            ;;
        3)
            printf '%s\n' '{"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"sessionId":"permission-session","toolCall":{"toolCallId":"private-tool-id","title":"Run private operation","rawInput":{"token":"credential-secret"}},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"},{"optionId":"always","name":"Allow always","kind":"allow_always"},{"optionId":"reject","name":"Reject once","kind":"reject_once"}]}}'
            ;;
        4)
            printf '%s\n' "$line" > '__RESPONSE__'
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn"}}'
            ;;
    esac
done
"#
    .replace("__RESPONSE__", response.to_str().unwrap());
    fs::write(&path, script).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    (path, response)
}

#[test]
fn doctor_initializes_installed_adapter_without_prompting() {
    let workspace = tempfile::tempdir().unwrap();
    let adapter = fake_adapter(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "doctor",
            "--adapter",
            adapter.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout
        .lines()
        .any(|line| line.contains("\"event\":\"initialized\"")));
    assert!(stdout
        .lines()
        .any(|line| line.contains("\"event\":\"session_opened\"")));
    assert!(stdout
        .lines()
        .any(|line| line.contains("\"event\":\"doctor_ok\"")));
    assert!(!stdout.contains("session_update"));
}

#[test]
fn task_rejects_a_relative_gateway_socket_from_the_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .env("COSH_GATEWAY_SOCKET", "relative/gateway.sock")
        .args(["task", "--output", "jsonl", "list"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(12));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"daemon_failed\""));
    assert!(stdout.contains("COSH_GATEWAY_SOCKET path must be absolute"));
}

#[test]
fn run_reads_prompt_from_stdin_and_escapes_terminal_controls() {
    let workspace = tempfile::tempdir().unwrap();
    let adapter = fake_adapter(&workspace);
    let mut child = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "run",
            "--adapter",
            adapter.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"inspect safely\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("safe\\u{1b}[2Jtext"));
    assert!(!stdout.as_bytes().contains(&0x1b));
    assert!(!stdout.contains("sessionUpdate"));
}

#[test]
fn missing_adapter_has_stable_profile_exit_and_jsonl_error() {
    let workspace = tempfile::tempdir().unwrap();
    let adapter = workspace.path().join("codex-acp");
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "doctor",
            "--adapter",
            adapter.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(11));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"event\":\"error\""));
    assert!(stdout.contains("\"code\":\"profile_invalid\""));
}

#[test]
fn noninteractive_permission_cancels_and_persists_only_digests() {
    let workspace = tempfile::tempdir().unwrap();
    fs::set_permissions(workspace.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let (adapter, response) = permission_adapter(&workspace);
    let evidence = workspace.path().join("permission-evidence.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "run",
            "--adapter",
            adapter.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--output",
            "jsonl",
            "--permission",
            "deny",
            "--permission-evidence",
            evidence.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"private prompt\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"event\":\"permission_decided\""));
    assert!(stdout.contains("\"decision\":\"cancelled\""));
    assert!(!stdout.contains("private-tool-id"));
    assert!(!stdout.contains("credential-secret"));

    let stored = fs::read_to_string(evidence).unwrap();
    assert!(stored.contains("\"decision\":\"cancelled\""));
    assert!(stored.contains("\"workspace_digest\""));
    assert!(!stored.contains("permission-session"));
    assert!(!stored.contains("private-tool-id"));
    assert!(!stored.contains("credential-secret"));
    let answer = fs::read_to_string(response).unwrap();
    assert!(answer.contains("\"id\":99"));
    assert!(answer.contains("cancelled"));
}

#[test]
fn relative_permission_evidence_path_fails_before_adapter_launch() {
    let workspace = tempfile::tempdir().unwrap();
    let adapter = workspace.path().join("codex-acp");
    fs::write(&adapter, "#!/bin/sh\ntouch launched\n").unwrap();
    let mut permissions = fs::metadata(&adapter).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&adapter, permissions).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .current_dir(workspace.path())
        .args([
            "run",
            "--adapter",
            adapter.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--permission-evidence",
            "relative.jsonl",
        ])
        .stdin(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(12));
    assert!(!workspace.path().join("launched").exists());
    assert!(!workspace.path().join("relative.jsonl").exists());
}

#[test]
fn task_cli_rejects_invalid_identity_before_socket_io() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("absent.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "task",
            "--socket",
            socket.to_str().unwrap(),
            "--output",
            "jsonl",
            "get",
            "run_00000000-0000-0000-0000-000000000000",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"invalid_request\""));
    assert!(stdout.contains("identifier prefix must be `tsk`"));
}

#[test]
fn task_snapshot_cli_rejects_inexact_snapshot_identity_before_socket_io() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("absent.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "task",
            "--socket",
            socket.to_str().unwrap(),
            "--output",
            "jsonl",
            "snapshot",
            "preview",
            "tsk_00000000-0000-0000-0000-000000000000",
            "snap-prefix",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"invalid_request\""));
    assert!(stdout.contains("identifier prefix must be `ckp`"));
}

#[test]
fn task_snapshot_switch_rejects_invalid_preview_digest_before_socket_io() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("absent.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "task",
            "--socket",
            socket.to_str().unwrap(),
            "--output",
            "jsonl",
            "snapshot",
            "switch",
            "tsk_00000000-0000-0000-0000-000000000000",
            "ckp_00000000-0000-0000-0000-000000000000",
            "--preview-digest",
            "not-a-digest",
            "--expected-revision",
            "7",
            "--idempotency-key",
            "stable-switch-key",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"invalid_request\""));
    assert!(stdout.contains("digest must contain exactly 64 lowercase hexadecimal characters"));
}

#[test]
fn task_retry_cli_rejects_a_non_run_previous_identity_before_socket_io() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("absent.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "task",
            "--socket",
            socket.to_str().unwrap(),
            "--output",
            "jsonl",
            "retry",
            "tsk_00000000-0000-0000-0000-000000000000",
            "--previous-run-id",
            "tsk_00000000-0000-0000-0000-000000000001",
            "--idempotency-key",
            "stable-retry-key",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"invalid_request\""));
    assert!(stdout.contains("identifier prefix must be `run`"));
}

#[test]
fn serve_rejects_an_invalid_provisioned_installation_identity() {
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args(["serve", "--installation-id", "invalid"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(12));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("identifier prefix must be `ins`"));
}

#[test]
fn serve_rejects_removed_runtime_and_acp_arguments_before_binding() {
    for removed in [
        vec!["--runtime-backend", "acp"],
        vec!["--runtime-backend", "core-brokered"],
        vec!["--profile", "codex"],
        vec!["--adapter", "/not/reached/codex-acp"],
    ] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("gateway.sock");
        let database = directory.path().join("gateway.db");
        let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
            .arg("serve")
            .args(&removed)
            .args(["--socket", socket.to_str().unwrap()])
            .args(["--database", database.to_str().unwrap()])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2), "removed args: {removed:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("unexpected argument"), "{stderr}");
        assert!(!socket.exists());
        assert!(!database.exists());
    }
}

#[test]
fn serve_help_exposes_unified_optional_runtime_inputs() {
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args(["serve", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--core-executable"), "{stdout}");
    for governed in ["--checkpoint-socket", "--security-audit", "--acp-adapter"] {
        assert!(stdout.contains(governed), "{stdout}");
    }
    for removed in [
        "--capability-profile",
        "--acp-profile",
        "--runtime-backend",
        "--profile",
        "--adapter",
    ] {
        assert!(!stdout.contains(removed), "{stdout}");
    }
}

#[test]
fn security_audit_requires_checkpoint_socket_before_binding() {
    let directory = tempfile::tempdir().unwrap();
    let core = directory.path().join("cosh-core");
    fs::write(&core, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&core).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&core, permissions).unwrap();
    let socket = directory.path().join("gateway.sock");
    let database = directory.path().join("gateway.db");
    let audit = directory.path().join("unused-audit.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "serve",
            "--security-audit",
            audit.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
            "--core-executable",
            core.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(11));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("profile_invalid"), "{stdout}");
    assert!(!socket.exists());
    assert!(!database.exists());
    assert!(!audit.exists());
}

#[test]
fn task_only_serve_requires_containment_for_the_brokered_core_runtime() {
    let workspace = tempfile::tempdir().unwrap();
    let core = workspace.path().join("cosh-core");
    fs::write(&core, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&core).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&core, permissions).unwrap();
    let socket = workspace.path().join("gateway.sock");
    let database = workspace.path().join("gateway.db");

    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "serve",
            "--socket",
            socket.to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--core-executable",
            core.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .env("PATH", "")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(12));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"runtime_containment_unverified\""));
    assert!(!stdout.contains("profile_invalid"));
    assert!(!socket.exists());
    assert!(!database.exists());
}

#[test]
fn packaged_service_requires_workspace_and_unifies_optional_runtimes() {
    let unit = include_str!("../../../packaging/systemd/cosh-gateway@.service.in");

    assert!(unit.contains("--core-executable=\"{libexecdir}/cosh-ng/cosh-core\""));
    assert!(unit.contains("--workspace=${COSH_GATEWAY_WORKSPACE}"));
    assert!(unit.contains(" $COSH_GATEWAY_ACP_ARG"));
    assert!(unit.contains(" $COSH_GATEWAY_CHECKPOINT_ARG"));
    assert!(unit.contains(" $COSH_GATEWAY_SECURITY_AUDIT_ARG"));
    assert!(!unit.contains("Environment=COSH_GATEWAY_WORKSPACE="));
    assert!(unit.contains("Environment=COSH_GATEWAY_ACP_ARG="));
    assert!(unit.contains("Environment=COSH_GATEWAY_CHECKPOINT_ARG="));
    assert!(unit.contains("Environment=COSH_GATEWAY_SECURITY_AUDIT_ARG="));
    assert!(unit.contains("EnvironmentFile=/etc/cosh/gateway-%i.env"));
    assert!(unit.contains("Environment=HOME=/var/lib/cosh-gateway-%i/core-home"));
    assert!(!unit.contains("WorkingDirectory=/var/lib/cosh-gateway-%i"));
    assert!(unit.contains("Conflicts=cosh-gateway-acp@%i.service"));
    let environment_file = unit
        .find("EnvironmentFile=/etc/cosh/gateway-%i.env")
        .unwrap();
    let exec_start = unit.find("ExecStart=").unwrap();
    for default in [
        "Environment=COSH_GATEWAY_ACP_ARG=",
        "Environment=COSH_GATEWAY_CHECKPOINT_ARG=",
        "Environment=COSH_GATEWAY_SECURITY_AUDIT_ARG=",
    ] {
        assert!(unit.find(default).unwrap() < environment_file, "{default}");
    }
    assert!(environment_file < exec_start);
    assert!(!unit.contains("ws-ckpt.service"));
    assert!(!unit.contains("--checkpoint-socket="));
    assert!(!unit.contains("--security-audit="));
    assert!(!unit.contains("--capability-profile="));
    assert!(!unit.contains("--acp-profile="));
    for property in [
        "Type=exec",
        "KillMode=control-group",
        "SendSIGKILL=yes",
        "FinalKillSignal=SIGKILL",
        "Delegate=no",
        "TimeoutStopSec=15",
        "Restart=on-failure",
        "NoNewPrivileges=true",
        "PrivateTmp=true",
        "ProtectSystem=full",
        "ProtectControlGroups=true",
        "InaccessiblePaths=/run/user",
        "RestrictSUIDSGID=false",
    ] {
        assert!(unit.lines().any(|line| line == property), "{property}");
    }
    assert!(!unit.lines().any(|line| line == "RestrictSUIDSGID=true"));
    assert!(!unit.contains("--adapter="));
    assert!(!unit.contains("--profile="));
    assert!(!unit.contains("--runtime-backend="));
}

#[test]
fn serve_rejects_a_spoofed_core_profile_before_binding_state() {
    let workspace = tempfile::tempdir().unwrap();
    let spoofed = workspace.path().join("not-cosh-core");
    fs::write(&spoofed, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&spoofed).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&spoofed, permissions).unwrap();
    let socket = workspace.path().join("gateway.sock");
    let database = workspace.path().join("gateway.db");

    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "serve",
            "--systemd-unit",
            "cosh-gateway@test.service",
            "--core-executable",
            spoofed.to_str().unwrap(),
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(11));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"profile_invalid\""));
    assert!(!socket.exists());
    assert!(!database.exists());
}

#[test]
fn serve_rejects_an_invalid_systemd_unit_before_binding() {
    let workspace = tempfile::tempdir().unwrap();
    let socket = workspace.path().join("gateway.sock");
    let database = workspace.path().join("gateway.db");

    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "serve",
            "--systemd-unit",
            "../cosh-gateway.service",
            "--core-executable",
            "/not/reached/cosh-core",
            "--socket",
            socket.to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(12));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"code\":\"runtime_containment_unverified\""));
    assert!(!socket.exists());
    assert!(!database.exists());
}
