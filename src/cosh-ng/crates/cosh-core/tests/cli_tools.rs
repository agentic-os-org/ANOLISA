use std::fs;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

fn run_with_tools(selection: &str) -> std::process::Output {
    let home = tempfile::tempdir().expect("temp home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-core"));
    command
        .env("HOME", home.path())
        .args(["--headless", "--bare", "--tools", selection])
        .stdin(Stdio::null());
    common::opt_out_telemetry(&mut command, home.path());
    command.output().expect("run cosh-core")
}

fn wait_for_output(mut child: Child, timeout: Duration) -> std::process::Output {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("poll cosh-core").is_some() {
            return child.wait_with_output().expect("collect cosh-core output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect timed out output");
            panic!(
                "cosh-core did not exit before auth timeout\nstdout={}\nstderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_invalid_tools_without_credentials(selection: &str) -> std::process::Output {
    let home = tempfile::tempdir().expect("temp home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-core"));
    command
        .args(["--headless", "--bare", "--tools", selection])
        .env("HOME", home.path())
        .env("COSH_AI_PROVIDER", "cli-tools-no-auth")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("DASHSCOPE_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ALIBABA_CLOUD_ACCESS_KEY_ID")
        .env_remove("ALIBABA_CLOUD_ACCESS_KEY_SECRET")
        .env_remove("ALIBABA_CLOUD_SECURITY_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::opt_out_telemetry(&mut command, home.path());
    let child = command.spawn().expect("spawn cosh-core");

    wait_for_output(child, Duration::from_secs(5))
}

#[test]
fn unknown_tools_fail_before_authentication() {
    let output = run_invalid_tools_without_credentials("shell,unknown_tool");

    assert_eq!(output.status.code(), Some(2), "status={:?}", output.status);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let messages = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("valid JSONL output"))
        .collect::<Vec<_>>();
    assert!(!messages.is_empty(), "stdout must contain JSONL output");
    assert!(
        messages
            .iter()
            .all(|message| message["type"] != "control_request"),
        "tool selection must fail before authentication: {stdout}"
    );

    let error = messages
        .iter()
        .find(|message| {
            message["type"] == "result"
                && message["subtype"] == "error"
                && message["is_error"] == true
        })
        .expect("tool selection error result");
    assert_eq!(error["errors"][0], "unknown tools: unknown_tool");
    assert_eq!(error["error_code"], "InvalidToolSelection");
    assert!(error["session_id"].is_string());

    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown tools: unknown_tool"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn empty_tools_remain_valid() {
    let output = run_with_tools("");

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn configured_mcp_tool_selection_remains_valid() {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");

    let server = home.path().join("fake-mcp.sh");
    fs::write(
        &server,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
  esac
done
"#,
    )
    .expect("write fake MCP server");

    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"

[mcp.servers.fake]
command = "sh"
args = ["{}"]
startup_timeout_ms = 1000
"#,
            server.display()
        ),
    )
    .expect("write cosh-core config");

    let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-core"));
    command
        .args(["--headless", "--bare", "--tools", "mcp__fake__echo"])
        .env("HOME", home.path())
        .env("COSH_STATES_DIR", home.path().join("states"))
        .env_remove("COSH_AI_PROVIDER")
        .stdin(Stdio::null());
    common::opt_out_telemetry(&mut command, home.path());
    let output = command
        .output()
        .expect("run cosh-core with configured MCP tool");

    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("unknown tools"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
