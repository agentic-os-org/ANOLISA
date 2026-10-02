//! Interactive MCP harness for manual testing of the agent-memory server.
//!
//! Spawns the server, performs the JSON-RPC handshake, then provides an
//! interactive prompt for calling any of the 19 MCP tools. Can also run
//! preset test scenarios with verbose output for human verification.
//!
//! Usage:
//!   cargo run --example mcp-harness -- /tmp/mem-test            (interactive REPL)
//!   cargo run --example mcp-harness -- /tmp/mem-test --scenario full  (preset scenario)
//!   cargo run --example mcp-harness -- /tmp/mem-test --scenario git   (git governance)
//!   cargo run --example mcp-harness -- /tmp/mem-test --scenario promote (session promote)

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use clap::Parser;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::time::timeout;

#[derive(Parser)]
#[command(name = "mcp-harness")]
#[command(about = "Interactive MCP harness for manual testing of agent-memory")]
struct Cli {
    /// Data directory for memory storage (created if missing)
    data_dir: PathBuf,

    /// Scenario mode: interactive | full | git | promote
    #[arg(long, default_value = "interactive")]
    scenario: String,

    /// Server binary path (default: agent-memory from PATH)
    #[arg(long, default_value = "agent-memory")]
    binary: String,

    /// Enable git versioning (used by git scenario)
    #[arg(long)]
    git: bool,

    /// Verbose output: show raw JSON-RPC messages
    #[arg(long)]
    verbose: bool,
}

// ---- MCP Client ----

struct McpClient {
    child: tokio::process::Child,
    reader: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: Option<ChildStdin>,
    next_id: u64,
    verbose: bool,
}

impl McpClient {
    async fn spawn(data_dir: &std::path::Path, binary: &str, git: bool, verbose: bool) -> Self {
        let session_dir = data_dir.join("__sessions__");
        let mut cmd = Command::new(binary);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("MEMORY_BASE_DIR", data_dir)
            .env("MEMORY_SESSION_DIR", &session_dir)
            .env("MEMORY_MOUNT_STRATEGY", "userland")
            .env("USER_ID", "tester");
        if git {
            cmd.env("MEMORY_GIT_ENABLED", "true");
            cmd.env("MEMORY_GIT_AUTO_COMMIT", "true");
        }

        let mut child = cmd.spawn().expect("failed to spawn MCP server");
        let stdout = child.stdout.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut reader = BufReader::new(stdout).lines();

        // Handshake
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "mcp-harness", "version": "1.0.0"}
            }
        });
        rpc_send(&mut stdin, &init, verbose).await;
        let resp = rpc_recv(&mut reader, verbose).await;
        if verbose {
            println!("<<< handshake response: {}", resp);
        }

        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        rpc_send(&mut stdin, &initialized, verbose).await;

        println!("MCP handshake complete. Server ready.");
        Self {
            child,
            reader,
            stdin: Some(stdin),
            next_id: 2,
            verbose,
        }
    }

    async fn call(&mut self, tool: &str, args: Value) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        });
        rpc_send(self.stdin.as_mut().unwrap(), &req, self.verbose).await;
        let resp = rpc_recv(&mut self.reader, self.verbose).await;
        extract_text(&resp)
    }

    async fn call_json(&mut self, tool: &str, args: Value) -> Value {
        let text = self.call(tool, args).await;
        serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("call_json({tool}): parse error: {e}\nraw: {text}");
            json!(null)
        })
    }

    async fn shutdown(&mut self) {
        self.stdin.take();
        let _ = self.child.kill().await;
    }
}

// ---- JSON-RPC transport ----

async fn rpc_send(stdin: &mut ChildStdin, msg: &Value, verbose: bool) {
    let payload = serde_json::to_string(msg).unwrap();
    if verbose {
        println!(">>> {}", payload);
    }
    stdin.write_all(payload.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

async fn rpc_recv(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    verbose: bool,
) -> Value {
    let line = timeout(Duration::from_secs(15), reader.next_line())
        .await
        .expect("timeout waiting for MCP response")
        .expect("io error reading MCP stream")
        .expect("MCP stream ended unexpectedly");
    if verbose {
        println!("<<< {}", line);
    }
    serde_json::from_str(&line).expect("invalid JSON from MCP server")
}

fn extract_text(resp: &Value) -> String {
    resp["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|i| i["text"].as_str())
        .unwrap_or("")
        .to_string()
}

// ---- Interactive REPL ----

fn print_help() {
    println!("\nAvailable commands:");
    println!("  call <tool> <json_args>  — call an MCP tool");
    println!("  list                     — list available tools");
    println!("  help                     — show this help");
    println!("  quit                     — shutdown and exit");
    println!("\nTool names:");
    println!("  Tier A: mem_read mem_write mem_append mem_edit mem_list");
    println!("          mem_grep mem_diff mem_mkdir mem_remove mem_promote mem_session_log");
    println!("  Tier B: memory_search memory_observe memory_get_context");
    println!("  Tier C: mem_snapshot mem_snapshot_list mem_snapshot_restore mem_log mem_revert");
    println!("\nExample:");
    println!("  call mem_write {{\"path\": \"test.md\", \"content\": \"hello\"}}");
    println!();
}

async fn interactive_repl(client: &mut McpClient) {
    println!("Entering interactive mode. Type 'help' for commands, 'quit' to exit.");
    let stdin = io::stdin();
    loop {
        print!("mcp> ");
        io::stdout().flush().unwrap();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap() == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "quit" || line == "exit" {
            break;
        }
        if line == "help" {
            print_help();
            continue;
        }
        if line == "list" {
            let text = client.call("tools/list", json!({})).await;
            println!("{}", text);
            continue;
        }
        if let Some(rest) = line.strip_prefix("call ") {
            // Parse: call <tool> <json_args>
            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
            if parts.len() < 2 {
                println!("Usage: call <tool> <json_args>");
                continue;
            }
            let tool = parts[0];
            let args_str = parts[1];
            let args: Value = match serde_json::from_str(args_str) {
                Ok(v) => v,
                Err(e) => {
                    println!("Invalid JSON args: {e}");
                    continue;
                }
            };
            println!("Calling {}...", tool);
            let text = client.call(tool, args).await;
            println!("Result: {text}");
            continue;
        }
        println!("Unknown command: {line}. Type 'help' for available commands.");
    }
}

// ---- Preset scenarios ----

async fn scenario_full(client: &mut McpClient) {
    println!("\n=== Phase 1: Tier A file ops ===\n");

    let r = client.call("mem_mkdir", json!({"path": "notes"})).await;
    println!("mem_mkdir notes: {r}");

    let r = client
        .call("mem_mkdir", json!({"path": "strategies"}))
        .await;
    println!("mem_mkdir strategies: {r}");

    let r = client
        .call(
            "mem_write",
            json!({"path": "notes/day1.md", "content": "Day 1: learned rust ownership model\n"}),
        )
        .await;
    println!("mem_write day1: {r}");

    let r = client
        .call(
            "mem_write",
            json!({"path": "strategies/rust-plan.md", "content": "# Rust Plan\nGoal: master ownership\n"}),
        )
        .await;
    println!("mem_write rust-plan: {r}");

    let r = client
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    println!("mem_read day1: {r}");

    let r = client
        .call(
            "mem_append",
            json!({"path": "notes/day1.md", "content": "Day 2: practiced borrowing rules"}),
        )
        .await;
    println!("mem_append day1: {r}");

    let r = client
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    println!("mem_read day1 (after append): {r}");

    let r = client
        .call(
            "mem_edit",
            json!({"path": "strategies/rust-plan.md", "old_str": "Goal: master ownership", "new_str": "Goal: master lifetimes"}),
        )
        .await;
    println!("mem_edit rust-plan: {r}");

    let r = client
        .call("mem_read", json!({"path": "strategies/rust-plan.md"}))
        .await;
    println!("mem_read rust-plan (after edit): {r}");

    let r = client
        .call_json("mem_list", json!({"recursive": true}))
        .await;
    println!("mem_list: {r}");

    let r = client
        .call_json("mem_grep", json!({"pattern": "ownership"}))
        .await;
    println!("mem_grep 'ownership': {r}");

    let r = client
        .call(
            "mem_diff",
            json!({"path1": "notes/day1.md", "path2": "strategies/rust-plan.md"}),
        )
        .await;
    println!("mem_diff: {r}");

    println!("\n=== Phase 2: Tier B structured search ===\n");

    let r = client
        .call(
            "memory_observe",
            json!({"content": "noticed that lifetimes prevent dangling pointers", "hint": "rust"}),
        )
        .await;
    println!("memory_observe: {r}");

    println!("Waiting 500ms for index worker...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let r = client
        .call_json("memory_search", json!({"query": "ownership", "top_k": 5}))
        .await;
    println!("memory_search 'ownership': {r}");

    let r = client
        .call("memory_get_context", json!({"max_tokens": 500}))
        .await;
    println!("memory_get_context: {r}");

    println!("\n=== Phase 3: Tier C snapshots ===\n");

    let r = client
        .call_json("mem_snapshot", json!({"name": "day2-checkpoint"}))
        .await;
    let snap_id = r["id"].as_str().unwrap_or("?");
    println!("mem_snapshot: id={snap_id}");

    let r = client.call_json("mem_snapshot_list", json!({})).await;
    println!("mem_snapshot_list: {r}");

    let r = client
        .call(
            "mem_write",
            json!({"path": "notes/day1.md", "content": "OVERWRITTEN", "overwrite": true}),
        )
        .await;
    println!("mem_write (overwrite day1): {r}");

    let r = client
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    println!("mem_read (after overwrite): {r}");

    let r = client
        .call("mem_snapshot_restore", json!({"id": snap_id}))
        .await;
    println!("mem_snapshot_restore: {r}");

    let r = client
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    println!("mem_read (after restore): {r}");

    println!("\n=== Phase 4: Sandbox & auxiliary ===\n");

    let r = client
        .call("mem_remove", json!({"path": "strategies/rust-plan.md"}))
        .await;
    println!("mem_remove: {r}");

    let r = client
        .call("mem_read", json!({"path": "strategies/rust-plan.md"}))
        .await;
    println!("mem_read (removed file): {r}");

    let r = client.call("mem_session_log", json!({})).await;
    println!("mem_session_log: {r}");

    let r = client
        .call("mem_read", json!({"path": "../../etc/passwd"}))
        .await;
    println!("sandbox escape (../../etc/passwd): {r}");

    let r = client
        .call(
            "mem_write",
            json!({"path": ".anolisa/audit.log", "content": "x"}),
        )
        .await;
    println!("sandbox meta-dir (.anolisa): {r}");

    println!("\n=== Full scenario complete ===\n");
}

async fn scenario_git(client: &mut McpClient) {
    println!("\n=== Git governance scenario ===\n");

    let r = client
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-1"}),
        )
        .await;
    println!("mem_write v1: {r}");

    println!("Waiting 200ms for auto-commit...");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let r = client
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-2", "overwrite": true}),
        )
        .await;
    println!("mem_write v2: {r}");

    println!("Waiting 200ms for auto-commit...");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let r = client
        .call_json("mem_log", json!({"limit": 10, "path": "page.md"}))
        .await;
    println!("mem_log: {r}");

    let r = client
        .call_json("mem_snapshot", json!({"name": "v2-snap"}))
        .await;
    let snap_id = r["id"].as_str().unwrap_or("?");
    println!("mem_snapshot: id={snap_id}");

    let r = client.call_json("mem_snapshot_list", json!({})).await;
    println!("mem_snapshot_list: {r}");

    let r = client
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-3", "overwrite": true}),
        )
        .await;
    println!("mem_write v3: {r}");

    let r = client
        .call("mem_snapshot_restore", json!({"id": snap_id}))
        .await;
    println!("mem_snapshot_restore: {r}");

    let r = client.call("mem_read", json!({"path": "page.md"})).await;
    println!("mem_read (after restore): {r}");

    let r = client
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-3", "overwrite": true}),
        )
        .await;
    println!("mem_write v3 again: {r}");

    println!("Waiting 200ms for auto-commit...");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let r = client.call("mem_revert", json!({"path": "page.md"})).await;
    println!("mem_revert: {r}");

    let r = client.call("mem_read", json!({"path": "page.md"})).await;
    println!("mem_read (after revert): {r}");

    println!("\n=== Git scenario complete ===\n");
}

async fn scenario_promote(_client: &mut McpClient, data_dir: &std::path::Path) {
    println!("\n=== Promote scenario ===\n");

    let sessions_root = data_dir.join("__sessions__");
    let scratch = sessions_root.join("ses_manual_test").join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("draft.md"), "promoted from scratch").unwrap();
    println!("Pre-created session scratch: {}", scratch.display());

    // Note: for promote to work, the server needs MEMORY_SESSION_ID.
    // Since we can't re-spawn with new env, we'll show what would happen.
    println!("\nNOTE: mem_promote requires MEMORY_SESSION_ID env var.");
    println!("For full promote testing, re-run with:");
    println!("  MEMORY_SESSION_ID=ses_manual_test MEMORY_SESSION_DIR=<path> agent-memory");
    println!("Then connect via mcp-harness and call:");
    println!(
        "  call mem_promote {{\"session_path\": \"draft.md\", \"store_path\": \"imported.md\"}}"
    );

    println!("\n=== Promote scenario notes complete ===\n");
}

// ---- Main ----

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Create data dir if it doesn't exist
    std::fs::create_dir_all(&cli.data_dir).unwrap();
    println!("Data directory: {}", cli.data_dir.display());

    let git = cli.git || cli.scenario == "git";
    let mut client = McpClient::spawn(&cli.data_dir, &cli.binary, git, cli.verbose).await;

    match cli.scenario.as_str() {
        "interactive" => interactive_repl(&mut client).await,
        "full" => scenario_full(&mut client).await,
        "git" => scenario_git(&mut client).await,
        "promote" => scenario_promote(&mut client, &cli.data_dir).await,
        other => {
            println!("Unknown scenario: {other}. Use: interactive | full | git | promote");
            interactive_repl(&mut client).await;
        }
    }

    client.shutdown().await;
    println!("Harness shut down.");
}
