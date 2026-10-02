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

//! L2 module-level comparison harness: tokenless vs headroom on identical
//! one-round tool outputs.
//!
//! Unlike the L0/L1 suites (which measure tokenless in isolation with a
//! bytes/4 heuristic), L2 runs PAIRED comparisons — both compressors see the
//! same bytes in the same round — and counts tokens with a real tokenizer
//! (tiktoken-rs, `o200k_base` headline / `cl100k_base` side report), so the
//! resulting deltas are attributable to compressor behaviour rather than
//! sampling or counting differences.
//!
//! Latency is reported under three explicitly distinct bases (in-process for
//! tokenless, worker-internal for headroom, wrapped-minus-raw wall clock for
//! rtk); the report labels each so numbers are never cross-compared silently.

pub mod headroom_side;
pub mod probe;
pub mod report;
pub mod retention;
pub mod rtk_side;
pub mod samples;
pub mod stats;
pub mod task_sim;
pub mod tokenizer;
pub mod tokenless_side;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors surfaced by the L2 harness library.
///
/// `HeadroomUnavailable` / `RtkUnavailable` are *expected* on machines
/// missing those toolchains: callers degrade to one-sided runs and record the
/// degradation in the report instead of aborting.
#[derive(Debug, Error)]
pub enum L2Error {
    /// The headroom worker failed to start or refused the handshake.
    #[error("headroom worker unavailable: {0}")]
    HeadroomUnavailable(String),
    /// No runnable rtk binary was found ($RTK_BIN, vendored build, PATH).
    #[error("rtk unavailable: {0}")]
    RtkUnavailable(String),
    /// The worker replied with something outside the line-JSON protocol.
    #[error("worker protocol violation: {0}")]
    Protocol(String),
    /// A sample command (raw or rtk-wrapped) could not be executed.
    #[error("command execution failed: {0}")]
    Command(String),
    /// Sample/probe asset files are malformed or reference missing data.
    #[error("invalid sample data: {0}")]
    InvalidSample(String),
    /// The semantic probe endpoint returned an unusable response.
    #[error("probe failure: {0}")]
    Probe(String),
    /// The tiktoken encoder could not be initialised.
    #[error("tokenizer init failed: {0}")]
    Tokenizer(String),
    /// Underlying I/O failure (spawn, pipe, file read/write).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// A ground-truth regex failed to compile.
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),
    /// HTTP transport failure while talking to the probe endpoint.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

/// The five payload categories compared by L2.
///
/// `Json` and `Code` are static (both sides compress the same committed
/// sample text); `Command`, `Grep` and `Diff` are captured live by running
/// the spec'd argv, so their ground truth is extracted dynamically from the
/// raw output of the very run being measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Json,
    Command,
    Grep,
    Code,
    Diff,
}

impl Category {
    /// All categories in report order.
    pub const ALL: [Category; 5] = [
        Category::Json,
        Category::Command,
        Category::Grep,
        Category::Code,
        Category::Diff,
    ];

    /// Stable lowercase name used in CLI flags, sample files and reports.
    pub fn name(self) -> &'static str {
        match self {
            Category::Json => "json",
            Category::Command => "command",
            Category::Grep => "grep",
            Category::Code => "code",
            Category::Diff => "diff",
        }
    }

    /// Parses a CLI/sample-file category name.
    pub fn parse(s: &str) -> Option<Category> {
        match s {
            "json" => Some(Category::Json),
            "command" => Some(Category::Command),
            "grep" => Some(Category::Grep),
            "code" => Some(Category::Code),
            "diff" => Some(Category::Diff),
            _ => None,
        }
    }

    /// True for categories whose content is produced by running a command.
    pub fn is_dynamic(self) -> bool {
        matches!(self, Category::Command | Category::Grep | Category::Diff)
    }

    /// Whether both sides receive byte-identical input for this category.
    ///
    /// tokenless' `JsonCompressor` only accepts JSON text, so a non-JSON
    /// static payload must be wrapped in a `{"content": ...}` envelope before it
    /// reaches the engine, while headroom receives the raw text. That makes the
    /// two sides' "before" bytes — and therefore their compressor behaviour —
    /// different, so the paired gap is not a like-for-like comparison and the
    /// report marks the category accordingly.
    ///
    /// `json` payloads are already compact JSON on both sides, and dynamic
    /// categories hand both sides the same captured command output.
    pub fn has_symmetric_inputs(self) -> bool {
        !matches!(self, Category::Code)
    }

    /// Why this category's sides are not byte-comparable, for the report.
    pub fn input_asymmetry_reason(self) -> Option<&'static str> {
        if self.has_symmetric_inputs() {
            return None;
        }
        Some(
            "tokenless compresses this payload inside a {\"content\": ...} JSON \
             envelope (its engine only accepts JSON values) while headroom \
             receives the raw text, so the two sides' original token counts and \
             compressor behaviour differ; the per-side rates stand on their own \
             but the cross-side gap is not like-for-like",
        )
    }

    /// Per-category p99 latency budget in milliseconds (quality gate).
    ///
    /// Grep and diff share the command budget because all three are captured
    /// through the same subprocess path.
    pub fn p99_budget_ms(self) -> f64 {
        match self {
            Category::Json => 2.0,
            Category::Code => 5.0,
            Category::Command | Category::Grep | Category::Diff => 10.0,
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One ground-truth item a compressed payload must still contain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GroundTruth {
    /// Literal substring match (case-sensitive).
    Substring(String),
    /// Regex match — used where the exact form varies (ids, counters).
    Pattern {
        /// The regex source; compiled at check time.
        regex: String,
    },
}

/// A single comparable payload: what both sides compress in one round.
#[derive(Debug, Clone)]
pub struct SampleRecord {
    /// Stable id referenced by task simulations and report rows.
    pub id: String,
    pub category: Category,
    /// The exact text handed to every compressor side.
    pub content: String,
    /// Items that must survive compression; empty until dynamically
    /// extracted for command/grep/diff samples.
    pub ground_truth: Vec<GroundTruth>,
}

/// One side's measurement for a single sample repetition.
#[derive(Debug, Clone, Serialize)]
pub struct SideResult {
    /// Compressed text as the model would receive it.
    pub compressed: String,
    /// o200k_base token count of the original text (headline base).
    pub tokens_before: usize,
    /// o200k_base token count of the compressed text.
    pub tokens_after: usize,
    /// cl100k_base counts, reported alongside for tokenizer sensitivity.
    pub tokens_before_cl100k: usize,
    pub tokens_after_cl100k: usize,
    /// Latency in seconds; the basis differs per side (see module docs).
    pub latency_s: f64,
    /// Which latency basis produced `latency_s` — kept on every row so
    /// downstream aggregation cannot mix bases unnoticed.
    pub latency_basis: &'static str,
    /// Strategy label reported by the side, when it exposes one.
    pub strategy: Option<String>,
    /// Side-reported token counts (headroom only), as cross-check evidence.
    pub side_tokens_before: Option<u64>,
    pub side_tokens_after: Option<u64>,
}

impl SideResult {
    /// Compression rate `1 - after/before` on the o200k headline counts.
    /// Zero-token originals yield 0.0 rather than a division blow-up.
    pub fn compression_rate(&self) -> f64 {
        if self.tokens_before == 0 {
            return 0.0;
        }
        1.0 - self.tokens_after as f64 / self.tokens_before as f64
    }
}

/// The paired outcome of one repetition of one sample: every side that ran.
///
/// Missing sides mean the toolchain was unavailable and the run degraded;
/// the report lists these degradations explicitly.
#[derive(Debug, Clone, Serialize)]
pub struct PairedOutcome {
    pub sample_id: String,
    pub category: Category,
    /// Repetition index within the sample (0-based).
    pub rep: usize,
    pub tokenless: Option<SideResult>,
    pub headroom: Option<SideResult>,
}
