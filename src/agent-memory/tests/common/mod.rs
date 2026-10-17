//! Shared MCP client harness for agent-memory test binaries.
//!
//! McpAgent wraps the JSON-RPC handshake and tool-call protocol over stdio,
//! providing a reusable client for both automated (`cargo test`) and
//! interactive (`mcp-harness`) testing.

// Included by several test binaries that each use a different subset of
// the harness; an item unused by one binary is not dead code for the suite.
#![allow(dead_code)]

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

// ---- Shared embedding mock + config harness for index-worker tests ----
//
// `KeywordEmbedding` started life duplicated between tier_b_test.rs and
// index_vector_dim_migration_test.rs; the model-migration tests need it to
// also model the provider quirks the migration has to survive, so it lives
// here now and both binaries import it.

use std::sync::atomic::{AtomicUsize, Ordering};

use agent_memory::config::AppConfig;
use agent_memory::embedding::{Embedding, EmbeddingProvider};
use agent_memory::service::MemoryService;

/// Deterministic embedding for tests: maps a keyword to a basis vector so
/// cosine similarity cleanly separates "rust" docs from "python" docs
/// (dims used here are >= 3 so every basis index exists).
///
/// Beyond the honest mode it can model the provider quirks the
/// model-migration path has to survive:
///
/// - `lying(declared, real)` — `dimensions()` reports `declared` while
///   `embed` produces `real`-wide vectors, the constructor-time estimate
///   that never self-corrects (unknown Ollama models, pre-first-response
///   OpenAI models);
/// - blank input short-circuits to a zero vector sized from the
///   *estimate*, exactly like both real providers;
/// - `empty_payload(declared)` — a broken backend answering with no
///   components at all, which carries no dimensionality and must not be
///   treated as authoritative;
/// - `embed_calls` counts every `embed` invocation so tests can pin
///   steady-state costs.
pub struct KeywordEmbedding {
    pub declared_dim: usize,
    pub real_dim: usize,
    pub empty_payload: bool,
    pub embed_calls: AtomicUsize,
}

impl KeywordEmbedding {
    /// `dimensions()` tells the truth and embeddings are `dim`-wide.
    pub fn honest(dim: usize) -> Self {
        Self {
            declared_dim: dim,
            real_dim: dim,
            empty_payload: false,
            embed_calls: AtomicUsize::new(0),
        }
    }

    /// `dimensions()` reports `declared` while `embed` produces `real`
    /// vectors — the estimate that never self-corrects.
    pub fn lying(declared: usize, real: usize) -> Self {
        Self {
            declared_dim: declared,
            real_dim: real,
            empty_payload: false,
            embed_calls: AtomicUsize::new(0),
        }
    }

    /// A backend answering with an empty vector: the answer carries no
    /// dimensionality at all.
    pub fn empty_payload(declared: usize) -> Self {
        Self {
            declared_dim: declared,
            real_dim: 0,
            empty_payload: true,
            embed_calls: AtomicUsize::new(0),
        }
    }

    pub fn embed_calls(&self) -> usize {
        self.embed_calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for KeywordEmbedding {
    async fn embed(&self, text: &str) -> agent_memory::Result<Embedding> {
        self.embed_calls.fetch_add(1, Ordering::SeqCst);
        if self.empty_payload {
            return Ok(Embedding { vector: Vec::new() });
        }
        if text.trim().is_empty() {
            // Both real providers short-circuit blank input before any
            // request and answer with a zero vector sized from the
            // estimate, not from the model.
            return Ok(Embedding {
                vector: vec![0.0_f32; self.declared_dim],
            });
        }
        let mut v = vec![0.0_f32; self.real_dim];
        let t = text.to_lowercase();
        if t.contains("rust") {
            v[0] = 1.0;
        } else if t.contains("python") {
            v[1] = 1.0;
        } else {
            v[2] = 1.0;
        }
        Ok(Embedding { vector: v })
    }

    fn dimensions(&self) -> usize {
        self.declared_dim
    }
}

/// Hermetic `AppConfig` for index-worker tests: userland mount, sessions
/// in a sub-temp of the same temp dir, no /run dependency.
pub fn base_config(tmp: &tempfile::TempDir) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg
}

/// Wait until the BM25 index holds at least `expected_min` rows.
pub fn wait_for_index(svc: &MemoryService, expected_min: usize) -> bool {
    svc.index
        .as_ref()
        .map(|h| h.wait_until_at_least(expected_min, 4000))
        .unwrap_or(false)
}
