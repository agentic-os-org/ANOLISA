//! Statistics record definitions for tokenless.
//!
//! Each record represents a single compression or rewriting operation
//! with before/after metrics and optional text content for diff export.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Type of operation performed (three compression types)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum OperationType {
    /// Schema compression (BeforeModel hook)
    CompressSchema,
    /// Response compression (PostToolUse hook)
    CompressResponse,
    /// Command rewriting (RTK, PreToolUse hook)
    RewriteCommand,
    /// TOON format compression (PostToolUse hook)
    CompressToon,
}

impl OperationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationType::CompressSchema => "compress-schema",
            OperationType::CompressResponse => "compress-response",
            OperationType::RewriteCommand => "rewrite-command",
            OperationType::CompressToon => "compress-toon",
        }
    }
}

impl FromStr for OperationType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "compress-schema" => Ok(OperationType::CompressSchema),
            "compress-response" => Ok(OperationType::CompressResponse),
            "rewrite-command" => Ok(OperationType::RewriteCommand),
            "compress-toon" => Ok(OperationType::CompressToon),
            other => Err(format!("unknown operation type: {other}")),
        }
    }
}

/// Whether the compression result was actually applied or only predicted.
///
/// `Active` is the normal mode: the compressed output is emitted and reaches
/// the LLM context. `DryRun` is the toggle-off mode: the compression is
/// computed (so the predicted savings are recorded) but the original text is
/// emitted instead — letting the same task run with/without compression to
/// compare E2E effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CompressionMode {
    #[default]
    Active,
    DryRun,
}

impl CompressionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompressionMode::Active => "active",
            CompressionMode::DryRun => "dry-run",
        }
    }

    /// Parse a stored mode value. Unknown/empty values (legacy rows with no
    /// `mode` column, or NULLs) fall back to `Active` rather than erroring,
    /// so historical data remains readable. Accepts both `dry-run` (current
    /// serde/db form) and legacy `dryrun` for backward compatibility.
    pub fn from_db(s: &str) -> Self {
        match s {
            "dry-run" | "dryrun" => CompressionMode::DryRun,
            _ => CompressionMode::Active,
        }
    }
}

/// A single statistics record stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsRecord {
    /// Database record ID (auto-increment primary key)
    pub id: i64,
    /// Timestamp when the record was created
    pub timestamp: DateTime<Local>,
    /// Type of operation (compress-schema, compress-response, rewrite-command)
    pub operation: OperationType,
    /// Agent identifier (e.g., "copilot-shell")
    pub agent_id: String,
    /// Source process ID (optional)
    pub source_pid: Option<i64>,
    /// Session ID for grouping related operations
    pub session_id: Option<String>,
    /// Tool use ID for correlation with specific tool calls
    pub tool_use_id: Option<String>,
    /// Byte length before compression (equals char count for ASCII)
    pub before_chars: usize,
    /// Tokens before compression (estimated)
    pub before_tokens: usize,
    /// Byte length after compression (equals char count for ASCII)
    pub after_chars: usize,
    /// Tokens after compression (estimated)
    pub after_tokens: usize,
    /// Original text content (for diff export)
    pub before_text: Option<String>,
    /// Compressed/rewritten text content (for diff export)
    pub after_text: Option<String>,
    /// Original command output (for rewrite-command output comparison)
    pub before_output: Option<String>,
    /// Rewritten command output (for rewrite-command output comparison)
    pub after_output: Option<String>,
    /// Whether compression was applied (Active) or only predicted (DryRun)
    pub mode: CompressionMode,
    /// Number of stash writes triggered by this compression (reversible stash
    /// feature). `None` for records written by older versions (pre-stash) or
    /// for operations that don't involve the stash.
    pub stash_writes: Option<i64>,
    /// Number of stash writes that failed during this compression (backend
    /// error — disk full, locked DB, I/O). `None` for pre-stash records or
    /// operations without a stash store attached.
    pub stash_errors: Option<i64>,
    /// Stash entry count at record time (a gauge of stash growth). `None` for
    /// pre-stash records or operations without a stash store attached.
    pub stash_size: Option<i64>,
    /// Detected content taxonomy (protocol `content_type` wire string).
    /// `None` for low-level paths and operations without a detector.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Origin declared by the adapter (protocol `content_origin` wire
    /// string). `None` outside the unified entry point.
    ///
    /// Roadmap §4.7 wants this to separate a protected-path passthrough from
    /// an ordinary no-savings outcome when tuning the release list. It cannot
    /// do that yet: `record_compression` returns before writing any row when
    /// `after_tokens >= before_tokens`, so neither outcome is recorded at
    /// all. This column is what those rows will carry once a recording policy
    /// emits them.
    #[serde(default)]
    pub content_origin: Option<String>,
    /// Lifecycle operations that shaped the emitted content.
    #[serde(default)]
    pub applied_operations: Option<Vec<String>>,
    /// Recovery state of the emitted content.
    #[serde(default)]
    pub recoverability: Option<String>,
    /// Token counter identity used for this row's estimates. `None` outside
    /// the unified entry.
    #[serde(default)]
    pub tokenizer_id: Option<String>,
    /// Truncations without an emitted recovery marker. `None` outside the
    /// unified entry or when the operation cannot truncate.
    #[serde(default)]
    pub unrecoverable_truncations: Option<i64>,
}

impl StatsRecord {
    /// Create a new stats record
    pub fn new(
        operation: OperationType,
        agent_id: String,
        before_chars: usize,
        before_tokens: usize,
        after_chars: usize,
        after_tokens: usize,
    ) -> Self {
        Self {
            id: -1,
            timestamp: Local::now(),
            operation,
            agent_id,
            source_pid: None,
            session_id: None,
            tool_use_id: None,
            before_chars,
            before_tokens,
            after_chars,
            after_tokens,
            before_text: None,
            after_text: None,
            before_output: None,
            after_output: None,
            mode: CompressionMode::default(),
            stash_writes: None,
            stash_errors: None,
            stash_size: None,
            content_type: None,
            content_origin: None,
            applied_operations: None,
            recoverability: None,
            tokenizer_id: None,
            unrecoverable_truncations: None,
        }
    }

    /// Set the session ID
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set the tool use ID
    pub fn with_tool_use_id(mut self, tool_use_id: impl Into<String>) -> Self {
        self.tool_use_id = Some(tool_use_id.into());
        self
    }

    /// Set the source PID
    pub fn with_source_pid(mut self, pid: i64) -> Self {
        self.source_pid = Some(pid);
        self
    }

    /// Set whether compression was applied (Active) or only predicted (DryRun)
    pub fn with_mode(mut self, mode: CompressionMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set text content before compression
    pub fn with_before_text(mut self, text: String) -> Self {
        self.before_text = Some(text);
        self
    }

    /// Set text content after compression
    pub fn with_after_text(mut self, text: String) -> Self {
        self.after_text = Some(text);
        self
    }

    /// Set before/after text for diff export
    pub fn with_text(mut self, before: String, after: String) -> Self {
        self.before_text = Some(before);
        self.after_text = Some(after);
        self
    }

    /// Set before/after command output for output comparison
    pub fn with_output(mut self, before: String, after: String) -> Self {
        self.before_output = Some(before);
        self.after_output = Some(after);
        self
    }

    /// Set stash write/error counts and stash size for this record (reversible
    /// stash observability). All `Option`; pass `None` when the operation had
    /// no stash store attached — this distinguishes "no stash" from "stash,
    /// zero writes" in stats queries.
    pub fn with_stash(
        mut self,
        writes: Option<usize>,
        errors: Option<usize>,
        size: Option<usize>,
    ) -> Self {
        self.stash_writes = writes.map(|w| w as i64);
        self.stash_errors = errors.map(|e| e as i64);
        self.stash_size = size.map(|s| s as i64);
        self
    }

    /// Sets lifecycle metadata written by the shared recording path.
    pub fn with_entry_metadata(
        mut self,
        content_type: Option<String>,
        content_origin: Option<String>,
        applied_operations: Option<Vec<String>>,
        recoverability: Option<String>,
        tokenizer_id: impl Into<String>,
        unrecoverable_truncations: Option<i64>,
    ) -> Self {
        self.content_type = content_type;
        self.content_origin = content_origin;
        self.applied_operations = applied_operations;
        self.recoverability = recoverability;
        self.tokenizer_id = Some(tokenizer_id.into());
        self.unrecoverable_truncations = unrecoverable_truncations;
        self
    }

    /// Characters saved by compression
    pub fn chars_saved(&self) -> usize {
        self.before_chars.saturating_sub(self.after_chars)
    }

    /// Tokens saved by compression
    pub fn tokens_saved(&self) -> usize {
        self.before_tokens.saturating_sub(self.after_tokens)
    }

    /// Characters saved percentage
    pub fn chars_percent(&self) -> f64 {
        if self.before_chars > 0 {
            (self.chars_saved() as f64 / self.before_chars as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Tokens saved percentage
    pub fn tokens_percent(&self) -> f64 {
        if self.before_tokens > 0 {
            (self.tokens_saved() as f64 / self.before_tokens as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Get a formatted summary line for list output
    pub fn format_summary_line(&self) -> String {
        let pid = self
            .source_pid
            .map(|p| format!(" pid:{p}"))
            .unwrap_or_default();
        let session = self.session_id.as_deref().unwrap_or("-");
        let tool = self.tool_use_id.as_deref().unwrap_or("-");

        format!(
            "[ID:{}] {} | {}{} | Session:{} | Tool:{} | Chars:{}→{}(-{}) | Tokens:{}→{}(-{:.0}%)",
            self.id,
            self.timestamp.format("%Y-%m-%d %H:%M:%S"),
            self.agent_id,
            pid,
            session,
            tool,
            self.before_chars,
            self.after_chars,
            self.chars_saved(),
            self.before_tokens,
            self.after_tokens,
            self.tokens_percent(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/record_tests.rs");
}
