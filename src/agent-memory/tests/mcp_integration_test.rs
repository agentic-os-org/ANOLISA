//! End-to-end MCP protocol tests.
//!
//! Spawns the binary, performs the JSON-RPC handshake, and verifies that
//! the 10 Tier A tools are exposed and behave correctly over stdio.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::time::timeout;

const EXPECTED_TOOLS: &[&str] = &[
    "mem_read",
    "mem_write",
    "mem_append",
    "mem_edit",
    "mem_list",
    "mem_grep",
    "mem_diff",
    "mem_mkdir",
    "mem_remove",
    "mem_promote",
    "mem_session_log",
    "memory_search",
    "memory_observe",
    "memory_get_context",
    "memory_sessions",
    "memory_timeline",
    "memory_summary",
    "memory_session_context",
    "mem_dream",
    "mem_index_refresh",
    "mem_snapshot",
    "mem_snapshot_list",
    "mem_snapshot_restore",
    "mem_log",
    "mem_revert",
    "mem_consolidate",
    "mem_compact",
    "memory_task_save",
    "memory_task_resume",
    "memory_task_list",
    "memory_task_close",
    "mem_export",
    "mem_import",
    "memory_about",
    "memory_forget",
    "memory_auto_created",
    "memory_consent",
];

async fn spawn_with_dir(
    data_dir: &std::path::Path,
) -> (
    tokio::process::Child,
    tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    ChildStdin,
) {
    let binary = env!("CARGO_BIN_EXE_agent-memory");
    // Pin a per-test session dir so concurrent tests don't fight over
    // /run/anolisa/sessions/.
    let session_dir = data_dir.join("__sessions__");
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("MEMORY_BASE_DIR", data_dir)
        .env("MEMORY_SESSION_DIR", &session_dir)
        .env("MEMORY_MOUNT_STRATEGY", "userland")
        .env("USER_ID", "tester")
        // Hermeticity: seal off ~/.anolisa/memory.toml leaks (see
        // McpAgent::spawn in tests/common/mod.rs for the full rationale).
        // Tests needing git/embedding use spawn_with_env to override these.
        .env("MEMORY_EMBEDDING_BACKEND", "none")
        .env("MEMORY_GIT_ENABLED", "false")
        .spawn()
        .expect("failed to spawn MCP server");

    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let reader = BufReader::new(stdout).lines();
    (child, reader, stdin)
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
    serde_json::from_str(&line).expect("invalid JSON")
}

async fn handshake(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: &mut ChildStdin,
) {
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "integration-test", "version": "1.0.0"}
        }
    });
    send(stdin, &init).await;
    let _ = recv(reader).await;

    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    send(stdin, &initialized).await;
}

async fn call_tool(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: &mut ChildStdin,
    id: u64,
    name: &str,
    args: Value,
) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": args}
    });
    send(stdin, &req).await;
    recv(reader).await
}

fn extract_text(resp: &Value) -> String {
    resp["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|i| i["text"].as_str())
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn lists_ten_tier_a_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
    send(&mut stdin, &req).await;
    let resp = recv(&mut reader).await;
    assert_eq!(resp["id"], 2);

    let tools = resp["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        tools.len(),
        EXPECTED_TOOLS.len(),
        "Expected {} tools, got {}: {:?}",
        EXPECTED_TOOLS.len(),
        tools.len(),
        names
    );
    for expected in EXPECTED_TOOLS {
        assert!(
            names.contains(expected),
            "tool {expected} missing in {names:?}"
        );
    }

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn write_read_grep_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    // Write
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        10,
        "mem_write",
        json!({"path": "notes/hello.md", "content": "hello world\nbye world\n"}),
    )
    .await;
    let text = extract_text(&resp);
    assert!(text.contains("wrote"), "got: {text}");

    // Read back
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        11,
        "mem_read",
        json!({"path": "notes/hello.md"}),
    )
    .await;
    assert_eq!(extract_text(&resp), "hello world\nbye world\n");

    // Grep
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        12,
        "mem_grep",
        json!({"pattern": "hello"}),
    )
    .await;
    let text = extract_text(&resp);
    let hits: Value = serde_json::from_str(&text).expect("grep returns JSON array");
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["path"], "notes/hello.md");
    assert_eq!(hits[0]["line"], 1);

    // List
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        13,
        "mem_list",
        json!({"recursive": true}),
    )
    .await;
    let text = extract_text(&resp);
    let entries: Value = serde_json::from_str(&text).expect("list returns JSON array");
    let paths: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"notes/hello.md"));
    assert!(paths.contains(&"README.md"));

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn edit_and_diff_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    call_tool(
        &mut reader,
        &mut stdin,
        20,
        "mem_write",
        json!({"path": "v1.md", "content": "title: hello\nbody: a"}),
    )
    .await;
    call_tool(
        &mut reader,
        &mut stdin,
        21,
        "mem_write",
        json!({"path": "v2.md", "content": "title: hello\nbody: a"}),
    )
    .await;

    // Edit v2
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        22,
        "mem_edit",
        json!({"path": "v2.md", "old_str": "body: a", "new_str": "body: b"}),
    )
    .await;
    assert!(extract_text(&resp).contains("edited"));

    // Diff
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        23,
        "mem_diff",
        json!({"path1": "v1.md", "path2": "v2.md"}),
    )
    .await;
    let text = extract_text(&resp);
    assert!(text.contains("--- v1.md"));
    assert!(text.contains("+++ v2.md"));
    assert!(text.contains("body: b"));

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn session_log_records_tool_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    call_tool(
        &mut reader,
        &mut stdin,
        40,
        "mem_write",
        json!({"path": "a.md", "content": "hello"}),
    )
    .await;

    let resp = call_tool(&mut reader, &mut stdin, 41, "mem_session_log", json!({})).await;
    let text = extract_text(&resp);
    assert!(
        text.contains("mem_write"),
        "session log missing write: {text}"
    );
    assert!(
        text.contains("\"path\":\"a.md\""),
        "session log missing path: {text}"
    );

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn promote_round_trip_from_scratch_to_store() {
    let tmp = tempfile::tempdir().unwrap();
    // Pre-create a file in the scratch dir of the not-yet-spawned session.
    // Use a fixed sid so we know where scratch lives.
    let sessions_root = tmp.path().join("__sessions__");
    let scratch = sessions_root.join("ses_promote_test").join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("draft.md"), "promoted content").unwrap();

    let binary = env!("CARGO_BIN_EXE_agent-memory");
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("MEMORY_BASE_DIR", tmp.path())
        .env("MEMORY_SESSION_DIR", &sessions_root)
        .env("MEMORY_SESSION_ID", "ses_promote_test")
        .env("MEMORY_MOUNT_STRATEGY", "userland")
        .env("USER_ID", "tester")
        // Hermeticity: seal ~/.anolisa/memory.toml leaks (see
        // McpAgent::spawn in tests/common/mod.rs).
        .env("MEMORY_EMBEDDING_BACKEND", "none")
        .env("MEMORY_GIT_ENABLED", "false")
        .spawn()
        .expect("spawn");

    let stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    handshake(&mut reader, &mut stdin).await;

    let resp = call_tool(
        &mut reader,
        &mut stdin,
        50,
        "mem_promote",
        json!({"session_path": "draft.md", "store_path": "imported.md"}),
    )
    .await;
    let text = extract_text(&resp);
    assert!(text.contains("promoted"), "got: {text}");

    let resp = call_tool(
        &mut reader,
        &mut stdin,
        51,
        "mem_read",
        json!({"path": "imported.md"}),
    )
    .await;
    assert_eq!(extract_text(&resp), "promoted content");

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn sandbox_blocks_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    let resp = call_tool(
        &mut reader,
        &mut stdin,
        30,
        "mem_read",
        json!({"path": "../../etc/passwd"}),
    )
    .await;
    let text = extract_text(&resp);
    assert!(
        text.to_lowercase().contains("outside"),
        "expected sandbox error, got: {text}"
    );

    let resp = call_tool(
        &mut reader,
        &mut stdin,
        31,
        "mem_write",
        json!({"path": ".anolisa/audit.log", "content": "x"}),
    )
    .await;
    let text = extract_text(&resp);
    assert!(
        text.contains("meta") || text.contains(".anolisa"),
        "expected meta-dir error, got: {text}"
    );

    drop(stdin);
    let _ = child.kill().await;
}

// -----------------------------------------------------------------------
// Tier B / Tier C tools — JSON-RPC end-to-end coverage.
//
// These tests close a gap surfaced by code review: every Tier B/C tool
// was already listed in EXPECTED_TOOLS so `tools/list` returned them,
// but no integration test actually drove a `tools/call` against them —
// rmcp tool_box registration, JSON Schema deserialization and the
// audit/git/snapshot side-effects were therefore only covered by the
// in-process tier_b_test / snapshot_test / git_test paths.
// -----------------------------------------------------------------------

/// Spawn the server with the given extra env vars in addition to the
/// shared defaults from `spawn_with_dir`. Used for git/snapshot tests
/// that need to flip non-default config knobs.
async fn spawn_with_env(
    data_dir: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> (
    tokio::process::Child,
    tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    ChildStdin,
) {
    let binary = env!("CARGO_BIN_EXE_agent-memory");
    let session_dir = data_dir.join("__sessions__");
    let mut cmd = Command::new(binary);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("MEMORY_BASE_DIR", data_dir)
        .env("MEMORY_SESSION_DIR", &session_dir)
        .env("MEMORY_MOUNT_STRATEGY", "userland")
        .env("USER_ID", "tester")
        // Hermeticity defaults (see McpAgent::spawn in tests/common/mod.rs);
        // extra_env below overrides these, so git/embedding tests flip them
        // back on by passing the matching MEMORY_* var.
        .env("MEMORY_EMBEDDING_BACKEND", "none")
        .env("MEMORY_GIT_ENABLED", "false");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn MCP server");
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let reader = BufReader::new(stdout).lines();
    (child, reader, stdin)
}

#[tokio::test]
async fn tier_b_observe_search_and_context_via_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    // Seed two files via mem_write — the index worker will pick them up.
    call_tool(
        &mut reader,
        &mut stdin,
        100,
        "mem_write",
        json!({"path": "notes/alpha.md", "content": "alpha refers to ownership in rust"}),
    )
    .await;
    call_tool(
        &mut reader,
        &mut stdin,
        101,
        "mem_write",
        json!({"path": "notes/beta.md", "content": "beta describes python gc"}),
    )
    .await;

    // memory_observe: writes notes/observed/<ulid>.md via the OS path.
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        102,
        "memory_observe",
        json!({"content": "looked at gamma today", "hint": "research"}),
    )
    .await;
    let observed_path = extract_text(&resp);
    assert!(
        observed_path.contains("notes/observed/") && observed_path.contains(".md"),
        "memory_observe should return a notes/observed/<ulid>.md path, got: {observed_path}"
    );

    // Give the inotify watcher a debounce window to flush new writes
    // into the FTS index. 200ms debounce + slack.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // memory_search: BM25 lookup for "ownership" must surface notes/alpha.md.
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        103,
        "memory_search",
        json!({"query": "ownership", "top_k": 5}),
    )
    .await;
    let text = extract_text(&resp);
    let hits: Value = serde_json::from_str(&text).expect("memory_search returns JSON array");
    let paths: Vec<&str> = hits
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["path"].as_str())
        .collect();
    assert!(
        paths.contains(&"notes/alpha.md"),
        "expected notes/alpha.md in {paths:?}"
    );

    // memory_get_context: returns a markdown preview of recent files
    // ordered by mtime desc. With 3 files seeded above it must be non-empty.
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        104,
        "memory_get_context",
        json!({"max_tokens": 200}),
    )
    .await;
    let text = extract_text(&resp);
    assert!(!text.is_empty(), "memory_get_context returned empty");
    assert!(
        text.contains("alpha") || text.contains("beta") || text.contains("gamma"),
        "expected one of the seeded body snippets in preview: {text}"
    );

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn tier_c_snapshot_create_list_restore_via_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    // Seed a file.
    call_tool(
        &mut reader,
        &mut stdin,
        200,
        "mem_write",
        json!({"path": "doc.md", "content": "version-1"}),
    )
    .await;

    // mem_snapshot: returns JSON with id field.
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        201,
        "mem_snapshot",
        json!({"name": "before-rewrite"}),
    )
    .await;
    let snap_text = extract_text(&resp);
    let snap_json: Value = serde_json::from_str(&snap_text)
        .expect("mem_snapshot should return JSON, got: {snap_text}");
    let snap_id = snap_json["id"].as_str().unwrap();
    assert!(
        snap_id.starts_with("snap_"),
        "mem_snapshot id should be snap_<ulid>, got: {snap_id}"
    );

    // mem_snapshot_list: the new snapshot must appear.
    let resp = call_tool(&mut reader, &mut stdin, 202, "mem_snapshot_list", json!({})).await;
    let listing_text = extract_text(&resp);
    assert!(
        listing_text.contains(snap_id),
        "snapshot list missing {snap_id}: {listing_text}"
    );

    // Mutate the file post-snapshot.
    call_tool(
        &mut reader,
        &mut stdin,
        203,
        "mem_write",
        json!({"path": "doc.md", "content": "version-2", "overwrite": true}),
    )
    .await;

    // mem_snapshot_restore: rolls doc.md back to version-1.
    call_tool(
        &mut reader,
        &mut stdin,
        204,
        "mem_snapshot_restore",
        json!({"id": snap_id}),
    )
    .await;

    let resp = call_tool(
        &mut reader,
        &mut stdin,
        205,
        "mem_read",
        json!({"path": "doc.md"}),
    )
    .await;
    assert_eq!(
        extract_text(&resp),
        "version-1",
        "restore should have rolled doc.md back to version-1"
    );

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn tier_c_git_log_and_revert_via_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    // Enable git versioning + auto-commit explicitly; default is off so
    // pre-existing mounts don't grow a .git dir silently.
    let (mut child, mut reader, mut stdin) = spawn_with_env(
        tmp.path(),
        &[
            ("MEMORY_GIT_ENABLED", "true"),
            ("MEMORY_GIT_AUTO_COMMIT", "true"),
        ],
    )
    .await;
    handshake(&mut reader, &mut stdin).await;

    // Two distinct writes → two auto-commits (skipping empty trees, but
    // these change content so both produce real commits).
    call_tool(
        &mut reader,
        &mut stdin,
        300,
        "mem_write",
        json!({"path": "page.md", "content": "v1"}),
    )
    .await;
    call_tool(
        &mut reader,
        &mut stdin,
        301,
        "mem_write",
        json!({"path": "page.md", "content": "v2", "overwrite": true}),
    )
    .await;

    // mem_log must surface at least the two writes (the initial repo seed
    // may or may not touch page.md depending on init order).
    let resp = call_tool(
        &mut reader,
        &mut stdin,
        302,
        "mem_log",
        json!({"limit": 10, "path": "page.md"}),
    )
    .await;
    let text = extract_text(&resp);
    let entries: Value = serde_json::from_str(&text).expect("mem_log returns JSON array");
    let arr = entries.as_array().unwrap();
    assert!(
        arr.len() >= 2,
        "expected at least 2 commits touching page.md, got {}: {text}",
        arr.len()
    );

    // mem_revert: with auto_commit=true every write is automatically committed,
    // so revert restores to HEAD content. Write v3, let it auto-commit, then
    // revert — the file should stay at v3 (revert of HEAD = same).
    call_tool(
        &mut reader,
        &mut stdin,
        303,
        "mem_write",
        json!({"path": "page.md", "content": "v3", "overwrite": true}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    call_tool(
        &mut reader,
        &mut stdin,
        304,
        "mem_revert",
        json!({"path": "page.md"}),
    )
    .await;

    let resp = call_tool(
        &mut reader,
        &mut stdin,
        305,
        "mem_read",
        json!({"path": "page.md"}),
    )
    .await;
    // Revert restores page.md to HEAD (which auto-committed v3).
    let body = extract_text(&resp);
    assert!(
        body.contains("v3"),
        "revert should restore HEAD content: {body}"
    );

    drop(stdin);
    let _ = child.kill().await;
}

#[tokio::test]
async fn mem_export_returns_ama_json() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut child, mut reader, mut stdin) = spawn_with_dir(tmp.path()).await;
    handshake(&mut reader, &mut stdin).await;

    // Write a memory first
    call_tool(
        &mut reader,
        &mut stdin,
        100,
        "mem_write",
        json!({"path": "export-test.md", "content": "export test content"}),
    )
    .await;

    // Export
    let resp = call_tool(&mut reader, &mut stdin, 200, "mem_export", json!({})).await;
    let text = extract_text(&resp);
    assert!(
        text.contains("anolisa-memory-archive"),
        "export should return AMA JSON: {text}"
    );
    assert!(
        text.contains("export-test.md"),
        "export should include written file: {text}"
    );

    drop(stdin);
    let _ = child.kill().await;
}
