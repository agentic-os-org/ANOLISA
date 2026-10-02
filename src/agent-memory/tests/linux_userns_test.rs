//! Phase 2: Linux user-namespace mount strategy end-to-end test.
//!
//! These tests `unshare` the test process itself, which is destructive (a
//! whole-program one-shot operation), so we drive a child binary instead.

use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command as TokioCommand};
use tokio::time::timeout;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_agent-memory")
}

fn host_userns_supported() -> bool {
    // Most systems have unprivileged userns enabled in 5.x kernels, but some
    // distros (and Docker default seccomp) gate it. A 1-line probe with
    // /usr/bin/unshare avoids invoking our crate.
    Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn info_shows_userns_when_strategy_userns() {
    if !host_userns_supported() {
        eprintln!("skipping: unprivileged userns not available on this host");
        return;
    }

    let tmp = tempdir().unwrap();
    let out = Command::new(binary())
        .args(["info"])
        .env("MEMORY_BASE_DIR", tmp.path())
        .env("USER_ID", "alice")
        .env("MEMORY_MOUNT_STRATEGY", "userns")
        .env("MEMORY_SESSION_DIR", tmp.path().join("__sessions__"))
        .output()
        .expect("spawn info");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("mount strategy : linux-userns"),
        "got: {stdout}"
    );
    assert!(stdout.contains("entered userns : true"), "got: {stdout}");
    assert!(
        stdout.contains("/mnt/memory/user-alice"),
        "expected /mnt/memory/user-alice in:\n{stdout}"
    );
}

#[test]
fn info_falls_back_with_strategy_auto_if_userns_unavailable() {
    // We can't easily disable userns at test time. Just exercise the
    // command path with auto on Linux: depending on the host, it picks
    // userns (when available) or userland; both are acceptable.
    let tmp = tempdir().unwrap();
    let out = Command::new(binary())
        .args(["info"])
        .env("MEMORY_BASE_DIR", tmp.path())
        .env("USER_ID", "auto-tester")
        .env("MEMORY_MOUNT_STRATEGY", "auto")
        .env("MEMORY_SESSION_DIR", tmp.path().join("__sessions__"))
        .output()
        .expect("spawn info");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    // Verify the info output distinguishes configured intent from actual strategy.
    assert!(
        s.contains("mount strategy :"),
        "missing actual strategy in:\n{s}"
    );
    assert!(
        s.contains("(configured: auto)"),
        "missing configured intent in:\n{s}"
    );
    // entered userns should be true or false — just check the field exists.
    assert!(
        s.contains("entered userns :"),
        "missing userns field in:\n{s}"
    );
}

#[tokio::test]
async fn userns_data_roundtrip_through_mcp() {
    if !host_userns_supported() {
        eprintln!("skipping: unprivileged userns not available on this host");
        return;
    }

    let tmp = tempdir().unwrap();
    let mut child = TokioCommand::new(binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("MEMORY_BASE_DIR", tmp.path())
        .env("USER_ID", "bob")
        .env("MEMORY_MOUNT_STRATEGY", "userns")
        .env("MEMORY_SESSION_DIR", tmp.path().join("__sessions__"))
        .spawn()
        .expect("spawn server");
    let stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    handshake(&mut reader, &mut stdin).await;

    // Write through MCP, then verify the data physically exists at the
    // host-side backing path: <base>/user-bob/notes/from_userns.md
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "mem_write",
                "arguments": {
                    "path": "notes/from_userns.md",
                    "content": "user-namespace says hello"
                }
            }
        }),
    )
    .await;
    let _ = recv(&mut reader).await;

    drop(stdin);
    let _ = child.wait().await;

    let backing = tmp
        .path()
        .join("user-bob")
        .join("notes")
        .join("from_userns.md");
    let body = std::fs::read_to_string(&backing).expect("data should be on disk");
    assert_eq!(body, "user-namespace says hello");
}

// ---------- helpers (mini version of mcp_integration_test) ----------

async fn handshake(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: &mut ChildStdin,
) {
    send(
        stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "userns-test", "version": "1.0"}
            }
        }),
    )
    .await;
    let _ = recv(reader).await;
    send(
        stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;
}

async fn send(stdin: &mut ChildStdin, msg: &serde_json::Value) {
    let payload = serde_json::to_string(msg).unwrap();
    stdin.write_all(payload.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

async fn recv(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> serde_json::Value {
    let line = timeout(Duration::from_secs(10), reader.next_line())
        .await
        .expect("timeout")
        .expect("io")
        .expect("eof");
    serde_json::from_str(&line).unwrap_or(serde_json::json!({}))
}
