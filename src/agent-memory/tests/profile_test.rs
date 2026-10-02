//! Phase 6.1: Profile真分档 — list_tools 按 profile 过滤。

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::time::timeout;

const TIER_B: &[&str] = &["memory_search", "memory_observe", "memory_get_context"];

async fn list_tools_for_profile(profile: &str) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("__sessions__");
    let binary = env!("CARGO_BIN_EXE_agent-memory");
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("MEMORY_BASE_DIR", tmp.path())
        .env("MEMORY_SESSION_DIR", &session_dir)
        .env("MEMORY_MOUNT_STRATEGY", "userland")
        .env("USER_ID", "tester")
        .env("MEMORY_PROFILE", profile)
        .spawn()
        .expect("spawn");
    let stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    handshake(&mut reader, &mut stdin).await;
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    let resp = recv(&mut reader).await;
    let names: Vec<String> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    drop(stdin);
    let _ = child.kill().await;
    names
}

async fn handshake(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: &mut ChildStdin,
) {
    send(
        stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"profile-test","version":"1.0"}
        }}),
    )
    .await;
    let _ = recv(reader).await;
    send(
        stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
}

async fn send(stdin: &mut ChildStdin, msg: &Value) {
    let payload = serde_json::to_string(msg).unwrap();
    stdin.write_all(payload.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

async fn recv(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Value {
    let line = timeout(Duration::from_secs(10), reader.next_line())
        .await
        .expect("timeout")
        .expect("io")
        .expect("eof");
    serde_json::from_str(&line).unwrap()
}

// 11 Tier A + 6 Tier B + 3 snapshot + 2 git + 2 consolidation + 4 task + 2 export/import + 2 sovereignty (read-only) + 2 session-history = 33
const TOTAL_TOOLS: usize = 37;
const TIER_B_COUNT: usize = 6;

#[tokio::test]
async fn basic_profile_exposes_all_tools() {
    let names = list_tools_for_profile("basic").await;
    assert_eq!(names.len(), TOTAL_TOOLS, "got: {names:?}");
}

#[tokio::test]
async fn advanced_profile_exposes_all_tools() {
    let names = list_tools_for_profile("advanced").await;
    assert_eq!(names.len(), TOTAL_TOOLS, "got: {names:?}");
}

#[tokio::test]
async fn expert_profile_hides_tier_b() {
    let names = list_tools_for_profile("expert").await;
    assert_eq!(
        names.len(),
        TOTAL_TOOLS - TIER_B_COUNT,
        "expected {}; got: {names:?}",
        TOTAL_TOOLS - TIER_B_COUNT
    );
    for hidden in TIER_B {
        assert!(
            !names.contains(&hidden.to_string()),
            "{hidden} should be hidden"
        );
    }
    // Tier A + Tier C still listed
    assert!(names.contains(&"mem_read".to_string()));
    assert!(names.contains(&"mem_write".to_string()));
    assert!(names.contains(&"mem_session_log".to_string()));
    assert!(names.contains(&"mem_snapshot".to_string()));
}
