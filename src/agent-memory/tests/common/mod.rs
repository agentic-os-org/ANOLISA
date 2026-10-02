//! Shared MCP client harness for agent-memory test binaries.
//!
//! McpAgent wraps the JSON-RPC handshake and tool-call protocol over stdio,
//! providing a reusable client for both automated (`cargo test`) and
//! interactive (`mcp-harness`) testing.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;

// ---- McpAgent ----

/// Lightweight MCP client that drives agent-memory over stdio JSON-RPC.
pub struct McpAgent {
    child: Child,
    reader: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: Option<ChildStdin>,
    next_id: u64,
}

impl McpAgent {
    /// Spawn the server, perform MCP handshake, return a ready client.
    ///
    /// `data_dir` is the base directory; `extra_env` are additional env vars
    /// (e.g. `MEMORY_GIT_ENABLED=true`).
    pub async fn spawn(data_dir: &std::path::Path, extra_env: &[(&str, &str)]) -> Self {
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
            // Hermeticity: the server otherwise reads ~/.anolisa/memory.toml
            // (default config path), so a developer's global config leaks into
            // the test. Disable the two subsystems whose leak breaks the BM25
            // e2e flow:
            //  - embedding: since 0.2.1 the index worker actually drives
            //    embeddings (runtime handle is now captured at spawn), so a
            //    configured provider makes per-file HTTP embed calls that
            //    delay BM25 upsert past the test's 500 ms wait, making
            //    memory_search spuriously return empty. Vector/hybrid search
            //    is covered separately in tier_b_test.rs with a mock provider.
            //  - git: auto-commit writes .git/logs/HEAD whose commit messages
            //    echo file paths (e.g. "notes/hello.md"), so mem_grep for a
            //    pattern that also appears in a path picks up spurious hits.
            // Tests that need either subsystem set the matching MEMORY_*_ENABLED
            // / MEMORY_EMBEDDING_BACKEND env via extra_env, which overrides
            // these defaults (extra_env is applied last).
            .env("MEMORY_EMBEDDING_BACKEND", "none")
            .env("MEMORY_GIT_ENABLED", "false");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("failed to spawn MCP server");
        let stdout = child.stdout.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut reader = BufReader::new(stdout).lines();

        // MCP handshake: initialize + initialized notification.
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
        send(&mut stdin, &init).await;
        let _ = recv(&mut reader).await;
        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        send(&mut stdin, &initialized).await;

        Self {
            child,
            reader,
            stdin: Some(stdin),
            next_id: 2,
        }
    }

    /// Call a tool and return the text content from the response.
    pub async fn call(&mut self, tool: &str, args: Value) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        });
        send(self.stdin.as_mut().unwrap(), &req).await;
        let resp = recv(&mut self.reader).await;
        extract_text(&resp)
    }

    /// Call a tool and parse the text content as a JSON Value.
    pub async fn call_json(&mut self, tool: &str, args: Value) -> Value {
        let text = self.call(tool, args).await;
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("call_json({tool}): failed to parse response as JSON: {e}\nraw text: {text}")
        })
    }

    /// Drop stdin and kill the child process.
    pub async fn cleanup(&mut self) {
        self.stdin.take();
        let _ = self.child.kill().await;
    }
}

// ---- JSON-RPC helpers ----

/// Send a JSON-RPC message over stdin.
pub async fn send(stdin: &mut ChildStdin, msg: &Value) {
    let payload = serde_json::to_string(msg).unwrap();
    stdin.write_all(payload.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

/// Receive a single JSON-RPC response line from stdout.
pub async fn recv(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Value {
    let line = timeout(Duration::from_secs(10), reader.next_line())
        .await
        .expect("timeout waiting for MCP response")
        .expect("io error reading MCP stream")
        .expect("MCP stream ended unexpectedly");
    serde_json::from_str(&line).expect("invalid JSON from MCP server")
}

/// Extract the text field from a tool-call response content array.
pub fn extract_text(resp: &Value) -> String {
    resp["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|i| i["text"].as_str())
        .unwrap_or("")
        .to_string()
}
