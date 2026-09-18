// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The the reference side of the comparison, driven over a long-lived worker.
//!
//! the comparison side is a Python package, so it runs out of process behind the
//! line-delimited JSON protocol in `assets/worker/headroom_pipeline_worker.py`.
//! The worker is started once and reused: importing importing it costs far more than
//! a single compression, so per-scenario spawning would bury the measurement in
//! interpreter start-up.
//!
//! A missing or broken worker degrades to a one-sided run. The report records
//! the degradation rather than aborting, because tokenless-only numbers still
//! answer part of the question and a silent abort would lose them.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::asset::{Message, Scenario};

/// Which pipeline the worker should run a scenario through.
///
/// Both are measured for every scenario: `PureStage` is how the reference's published
/// benchmark is configured, `Router` is what the reference can actually do. Reporting
/// either alone misrepresents it — on prose the two differ by ~70 points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    /// `CacheAligner -> SmartCrusher`, the reference's own fixture assembly.
    PureStage,
    /// The above plus `ContentRouter`, the reference's recommended entry point.
    Router,
}

impl Variant {
    /// Wire name understood by the worker.
    pub fn wire(self) -> &'static str {
        match self {
            Variant::PureStage => "pure_stage",
            Variant::Router => "router",
        }
    }

    /// Both variants, in report order.
    pub fn all() -> [Variant; 2] {
        [Variant::PureStage, Variant::Router]
    }
}

/// What the worker reported about the reference it imported.
///
/// `dirty` covers tracked modifications only; `untracked` is separate because an
/// untracked file inside the package changes what ran without moving the
/// revision or the tracked-dirty flag.
#[derive(Debug, Clone, Serialize)]
pub struct HeadroomProvenance {
    /// Revision of the imported reference checkout, when it could be read.
    pub revision: Option<String>,
    /// Whether that checkout had uncommitted tracked changes.
    pub dirty: Option<bool>,
    /// Untracked files in that checkout.
    pub untracked: Option<usize>,
    /// Which token estimator drove the reference's decisions.
    pub tokenizer: Option<String>,
}

/// Handshake line emitted by the worker on start.
#[derive(Debug, Deserialize)]
struct Handshake {
    ready: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    dirty: Option<bool>,
    #[serde(default)]
    untracked: Option<usize>,
    #[serde(default)]
    tokenizer: Option<String>,
    #[serde(default)]
    variants: Vec<String>,
}

/// Per-request response from the worker.
#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    compress_ms: f64,
    #[serde(default)]
    tokens_before: Option<i64>,
    #[serde(default)]
    tokens_after: Option<i64>,
    #[serde(default)]
    transforms_applied: Vec<String>,
    #[serde(default)]
    warnings: Vec<String>,
}

/// What the reference did to one scenario under one variant.
#[derive(Debug, Clone)]
pub struct HeadroomResult {
    /// Which pipeline produced this.
    pub variant: Variant,
    /// The conversation as the reference would hand it to the model.
    pub messages: Vec<Message>,
    /// Wall time inside `pipeline.apply`, excluding pipe and JSON framing.
    pub compress_ms: f64,
    /// The reference's own token count before compression.
    ///
    /// Its estimator, not the harness's — kept only as corroboration. Every
    /// published rate uses the authoritative tiktoken count instead, because
    /// the two bases disagree for the identical payload.
    pub self_tokens_before: Option<i64>,
    /// The reference's own token count after compression, same caveat.
    pub self_tokens_after: Option<i64>,
    /// Transform names the reference applied, e.g. `smart:lossless:table(...)`.
    ///
    /// The most direct evidence of *how* it compressed: a lossless table
    /// re-encoding and an item-dropping truncation can produce a similar rate
    /// while differing entirely in what survives.
    pub transforms_applied: Vec<String>,
    /// Advisory warnings the reference raised.
    pub warnings: Vec<String>,
}

/// A running worker.
///
/// `Debug` is derived so callers can put this in structs of their own without
/// tripping the missing-`Debug` lint.
#[derive(Debug)]
pub struct HeadroomWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    provenance: HeadroomProvenance,
    variants: Vec<String>,
}

/// Failure modes of the reference side.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The interpreter could not be started.
    #[error("failed to spawn the reference worker via {python}: {source}")]
    Spawn {
        /// Interpreter that was tried.
        python: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// The worker died, or its pipes could not be used.
    #[error("the reference worker i/o failed: {0}")]
    Io(#[from] std::io::Error),

    /// The worker reported it could not import or configure the reference.
    #[error("the reference worker refused to start: {0}")]
    NotReady(String),

    /// A protocol line was not the JSON the protocol specifies.
    #[error("malformed worker protocol line: {0}")]
    Protocol(#[from] serde_json::Error),

    /// The worker closed stdout before answering.
    #[error("the reference worker closed the connection before responding")]
    Closed,
}

impl HeadroomWorker {
    /// Start the worker and complete the handshake.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Spawn`] when the interpreter is missing and
    /// [`WorkerError::NotReady`] when the reference itself could not be imported or
    /// configured — both of which the caller should turn into a recorded
    /// degradation rather than a hard failure.
    pub fn start(python: &str, worker: &std::path::Path) -> Result<Self, WorkerError> {
        let mut child = Command::new(python)
            .arg(worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited: the reference's advisory notices go to stderr, and losing
            // them would make a degraded run harder to diagnose.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| WorkerError::Spawn {
                python: python.to_string(),
                source,
            })?;

        let stdin = child.stdin.take().ok_or(WorkerError::Closed)?;
        let stdout = child.stdout.take().ok_or(WorkerError::Closed)?;
        let mut stdout = BufReader::new(stdout);

        let mut line = String::new();
        if stdout.read_line(&mut line)? == 0 {
            return Err(WorkerError::Closed);
        }
        let handshake: Handshake = serde_json::from_str(line.trim())?;
        if !handshake.ready {
            return Err(WorkerError::NotReady(
                handshake
                    .error
                    .unwrap_or_else(|| "no reason given".to_string()),
            ));
        }

        Ok(Self {
            child,
            stdin,
            stdout,
            provenance: HeadroomProvenance {
                revision: handshake.revision,
                dirty: handshake.dirty,
                untracked: handshake.untracked,
                tokenizer: handshake.tokenizer,
            },
            variants: handshake.variants,
        })
    }

    /// Provenance of the reference the worker imported.
    pub fn provenance(&self) -> &HeadroomProvenance {
        &self.provenance
    }

    /// Whether the worker offers a variant, so an older worker degrades that
    /// variant instead of failing the whole run.
    pub fn supports(&self, variant: Variant) -> bool {
        self.variants.iter().any(|v| v == variant.wire())
    }

    /// Run one scenario through one variant.
    ///
    /// # Errors
    ///
    /// Propagates protocol and I/O failures. A scenario the reference itself failed
    /// on comes back as `Ok(None)` with the reason in `reason`, so one bad
    /// scenario does not end the run.
    pub fn run(
        &mut self,
        scenario: &Scenario,
        variant: Variant,
    ) -> Result<Result<HeadroomResult, String>, WorkerError> {
        let request = serde_json::json!({
            "messages": scenario.messages,
            "model": "benchmark-model",
            "model_limit": scenario.model_limit,
            "variant": variant.wire(),
        });
        let mut payload = serde_json::to_string(&request)?;
        payload.push('\n');

        // Round-trip wall time is not reported: the worker's own perf_counter
        // around pipeline.apply excludes pipe and framing cost, which this
        // would include.
        let _sent_at = Instant::now();
        self.stdin.write_all(payload.as_bytes())?;
        self.stdin.flush()?;

        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err(WorkerError::Closed);
        }
        let response: Response = serde_json::from_str(line.trim())?;
        if !response.ok {
            return Ok(Err(response
                .error
                .unwrap_or_else(|| "no reason given".to_string())));
        }

        Ok(Ok(HeadroomResult {
            variant,
            messages: response.messages,
            compress_ms: response.compress_ms,
            self_tokens_before: response.tokens_before,
            self_tokens_after: response.tokens_after,
            transforms_applied: response.transforms_applied,
            warnings: response.warnings,
        }))
    }
}

impl Drop for HeadroomWorker {
    fn drop(&mut self) {
        // Closing stdin ends the worker's read loop, so it exits on its own.
        // `kill` is the fallback for a worker wedged inside a compression.
        let _ = self.stdin.flush();
        if let Some(stdin) = self.child.stdin.take() {
            drop(stdin);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Absolute path of the worker script shipped with the crate.
pub fn worker_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/worker/headroom_pipeline_worker.py")
}

/// Interpreter to run the worker with, from `HEADROOM_PYTHON` or `python3`.
pub fn python_binary() -> String {
    std::env::var("HEADROOM_PYTHON").unwrap_or_else(|_| "python3".to_string())
}

/// Reduce a `Value` message list to the harness representation.
///
/// The worker returns whatever the reference produced, which may include fields this
/// harness does not model; non-object entries are dropped because a message
/// list must be objects to be counted.
pub fn as_messages(values: Vec<Value>) -> Vec<Message> {
    values
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_wire_names_match_the_worker_protocol() {
        assert_eq!(Variant::PureStage.wire(), "pure_stage");
        assert_eq!(Variant::Router.wire(), "router");
    }

    #[test]
    fn both_variants_are_measured() {
        assert_eq!(Variant::all().len(), 2);
    }

    #[test]
    fn handshake_failure_carries_the_reason() {
        let line = r#"{"ready": false, "error": "ModuleNotFoundError: the reference"}"#;
        let hs: Handshake = serde_json::from_str(line).expect("parses");
        assert!(!hs.ready);
        assert_eq!(
            hs.error.as_deref(),
            Some("ModuleNotFoundError: the reference")
        );
    }

    #[test]
    fn handshake_reports_provenance_and_variants() {
        let line = r#"{"ready": true, "revision": "abc", "dirty": true,
                       "untracked": 65, "tokenizer": "mock-chars4",
                       "variants": ["pure_stage", "router"]}"#;
        let hs: Handshake = serde_json::from_str(line).expect("parses");
        assert!(hs.ready);
        assert_eq!(hs.untracked, Some(65));
        assert_eq!(hs.tokenizer.as_deref(), Some("mock-chars4"));
        assert_eq!(hs.variants.len(), 2);
    }

    #[test]
    fn response_keeps_transform_names() {
        // Transform names distinguish a lossless table re-encoding from an
        // item-dropping truncation, which can share a compression rate while
        // differing entirely in what survives.
        let line = r#"{"ok": true, "messages": [], "compress_ms": 1.5,
                       "tokens_before": 100, "tokens_after": 40,
                       "transforms_applied": ["smart:lossless:table(1000->len=2)"],
                       "warnings": []}"#;
        let r: Response = serde_json::from_str(line).expect("parses");
        assert!(r.ok);
        assert_eq!(r.transforms_applied.len(), 1);
        assert!(r.transforms_applied[0].contains("lossless"));
    }

    #[test]
    fn failed_response_carries_the_reason() {
        let line = r#"{"ok": false, "error": "ValueError: boom"}"#;
        let r: Response = serde_json::from_str(line).expect("parses");
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("ValueError: boom"));
    }

    #[test]
    fn non_object_entries_are_dropped_from_message_lists() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            Value::Null,
        ];
        assert_eq!(as_messages(values).len(), 1);
    }

    #[test]
    fn worker_script_ships_with_the_crate() {
        assert!(
            worker_path().exists(),
            "worker script missing: {:?}",
            worker_path()
        );
    }
}
