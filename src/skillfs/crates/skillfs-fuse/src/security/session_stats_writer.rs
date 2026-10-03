//! Dedicated JSONL writer for mount-session summary statistics.
//!
//! Writes to the session metrics log path:
//!   `/var/log/anolisa/sls/ops/skillfs.jsonl`
//!
//! Design contract:
//!
//! * The target file must already exist. Deployment owns file creation,
//!   ownership, permissions, and logrotate policy. SkillFS only appends to
//!   the pre-created file.
//! * Every write opens the file, appends one JSONL line, and closes the
//!   handle, so rename-based rotation never strands writes in a stale fd.
//! * No background thread, no bounded channel. The summary is written
//!   synchronously at mount exit — typically once per session.
//! * Write failures are logged via `tracing::warn` but never propagate as
//!   FUSE errors or change mount exit status.
//! * This writer does **not** reuse [`super::audit::JsonlFileAuditSink`].

use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::warn;

use super::session_stats::{SkillfsSessionSummary, serialize_session_summary};
use super::telemetry_gate::{TELEMETRY_DISABLED_SENTINEL, telemetry_allowed_at};

/// Default session metrics log path per the default deployment convention.
pub const SKILLFS_SESSION_METRICS_LOG_PATH: &str = "/var/log/anolisa/sls/ops/skillfs.jsonl";

/// Result of a successful [`SessionStatsWriter::write_summary`] call.
///
/// `Ok(())` alone was ambiguous once the telemetry gate could suppress the
/// write: a caller could not tell an appended line apart from a silent skip and
/// would log a misleading "flushed" message. This distinguishes the two so the
/// caller logs success only when a line was actually appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryWriteOutcome {
    /// The summary line was appended to the metrics log.
    Written,
    /// The telemetry sentinel is present, so nothing was written. This is a
    /// normal, non-error state and must not affect mount exit status.
    SkippedDisabled,
}

/// Best-effort JSONL summary writer.
///
/// Each call to [`SessionStatsWriter::write_summary`] opens an existing target
/// file in append mode, writes one JSON line, and closes the handle. Missing
/// files are treated as deployment/configuration errors. Errors are surfaced as
/// `tracing::warn` and returned as `Err`, but the CLI must not retry or abort.
pub struct SessionStatsWriter {
    path: PathBuf,
    /// Telemetry disable sentinel. Pinned to [`TELEMETRY_DISABLED_SENTINEL`] in
    /// production (no override path exists); tests inject a temp path so the
    /// gate does not depend on the host's real sentinel.
    sentinel: PathBuf,
}

impl SessionStatsWriter {
    /// Create a writer targeting the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            sentinel: PathBuf::from(TELEMETRY_DISABLED_SENTINEL),
        }
    }

    /// Create a writer targeting the default session metrics log.
    pub fn default_path() -> Self {
        Self::new(SKILLFS_SESSION_METRICS_LOG_PATH)
    }

    /// Override the telemetry sentinel path (tests only) so writer tests do not
    /// depend on the host's real `/etc/anolisa/.telemetry_disabled`.
    #[cfg(test)]
    fn with_sentinel(mut self, sentinel: impl Into<PathBuf>) -> Self {
        self.sentinel = sentinel.into();
        self
    }

    /// The target file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write one session summary as a JSONL line (open + append + close).
    ///
    /// Returns `Ok(())` when a line was appended **or** when the telemetry
    /// sentinel suppressed the write — both are non-error outcomes that must
    /// not affect mount exit status. On IO failure, logs a warning and returns
    /// the underlying error. Callers must treat failure as non-fatal.
    ///
    /// This is the stable, backward-compatible signature. Callers that need to
    /// distinguish an actual write from a sentinel skip (e.g. to avoid logging
    /// a spurious "flushed" message) should use
    /// [`SessionStatsWriter::write_summary_with_outcome`].
    pub fn write_summary(&self, summary: &SkillfsSessionSummary) -> std::io::Result<()> {
        self.write_summary_with_outcome(summary).map(|_| ())
    }

    /// Like [`SessionStatsWriter::write_summary`] but reports whether a line was
    /// actually appended.
    ///
    /// Returns [`SummaryWriteOutcome::Written`] when a line was appended and
    /// [`SummaryWriteOutcome::SkippedDisabled`] when the telemetry sentinel
    /// suppressed the write. On IO failure, logs a warning and returns the
    /// underlying error. Callers must treat failure as non-fatal.
    pub fn write_summary_with_outcome(
        &self,
        summary: &SkillfsSessionSummary,
    ) -> std::io::Result<SummaryWriteOutcome> {
        // Re-check the disable sentinel on every write (before serialization
        // and open) so creating/removing it takes effect immediately without a
        // restart. Disabled is a normal state: skip silently and report the skip
        // so a suppressed summary neither errors nor logs a spurious success.
        if !telemetry_allowed_at(&self.sentinel) {
            return Ok(SummaryWriteOutcome::SkippedDisabled);
        }

        let mut line = serialize_session_summary(summary);
        line.push('\n');

        // Deployment owns file creation, ownership, permissions, and
        // logrotate policy. Do not create the file here; a missing path should
        // surface as a non-fatal configuration error.
        let result = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .and_then(|mut file| file.write_all(line.as_bytes()));

        if let Err(ref e) = result {
            warn!(
                error = %e,
                path = %self.path.display(),
                "skillfs session stats: failed to write summary to session metrics log"
            );
        }

        result.map(|()| SummaryWriteOutcome::Written)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::session_stats::SkillfsSessionStats;

    /// Build a writer whose telemetry sentinel points at a path inside `dir`
    /// that is never created, isolating writer tests from the host's real
    /// `/etc/anolisa/.telemetry_disabled`.
    fn writer_enabled(log_path: impl Into<PathBuf>, dir: &Path) -> SessionStatsWriter {
        SessionStatsWriter::new(log_path).with_sentinel(dir.join(".telemetry_disabled"))
    }

    #[test]
    fn writes_one_valid_json_line_to_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("skillfs.jsonl");

        std::fs::File::create(&log_path).unwrap();

        let writer = writer_enabled(&log_path, dir.path());
        let stats = SkillfsSessionStats::new();
        stats.set_skill_counts(20, 6);
        stats.record_skill_hit("weather");
        let summary = stats.build_summary("test-write-1", "agent");

        assert_eq!(
            writer
                .write_summary_with_outcome(&summary)
                .expect("write must succeed"),
            SummaryWriteOutcome::Written
        );

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "expected exactly 1 line, got {}",
            lines.len()
        );

        let parsed: serde_json::Value =
            serde_json::from_str(lines[0]).expect("line must be valid JSON");
        assert_eq!(parsed["component.name"], "skillfs");
        assert_eq!(parsed["session_id"], "test-write-1");
        assert_eq!(parsed["pruned_skill_count"], 14);
    }

    #[test]
    fn second_write_appends_another_line() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("skillfs.jsonl");

        std::fs::File::create(&log_path).unwrap();

        let writer = writer_enabled(&log_path, dir.path());

        let stats1 = SkillfsSessionStats::new();
        let summary1 = stats1.build_summary("session-a", "agent");
        writer.write_summary(&summary1).unwrap();

        let stats2 = SkillfsSessionStats::new();
        stats2.record_decision(crate::security::session_stats::RuntimeDecisionOutcome::Allow);
        let summary2 = stats2.build_summary("session-b", "agent");
        writer.write_summary(&summary2).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 lines, got {}", lines.len());

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["session_id"], "session-a");
        assert_eq!(second["session_id"], "session-b");
        assert_eq!(second["allow_times"], 1);
    }

    #[test]
    fn missing_log_file_returns_not_found_and_does_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("missing-skillfs.jsonl");

        let writer = writer_enabled(&log_path, dir.path());
        let stats = SkillfsSessionStats::new();
        let summary = stats.build_summary("missing-file-test", "agent");
        let err = writer
            .write_summary(&summary)
            .expect_err("missing log file must not be created implicitly");

        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(
            !log_path.exists(),
            "writer must not create missing session metrics log"
        );
    }

    #[test]
    fn write_failure_on_invalid_path_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let writer = writer_enabled("/nonexistent/deep/skillfs.jsonl", dir.path());
        let stats = SkillfsSessionStats::new();
        let summary = stats.build_summary("fail-test", "agent");
        let result = writer.write_summary(&summary);
        assert!(result.is_err(), "expected write to fail on invalid path");
    }

    #[test]
    fn disabled_sentinel_skips_write_and_reports_skip() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&log_path).unwrap();

        // Point the gate at a sentinel that exists -> writes are suppressed.
        let sentinel = dir.path().join(".telemetry_disabled");
        std::fs::File::create(&sentinel).unwrap();
        let writer = SessionStatsWriter::new(&log_path).with_sentinel(&sentinel);

        let stats = SkillfsSessionStats::new();
        let summary = stats.build_summary("disabled-test", "agent");
        assert_eq!(
            writer.write_summary_with_outcome(&summary).unwrap(),
            SummaryWriteOutcome::SkippedDisabled
        );
        assert!(
            std::fs::read_to_string(&log_path).unwrap().is_empty(),
            "disabled telemetry must not append any line"
        );
    }

    #[test]
    fn default_path_is_session_metrics_path() {
        let writer = SessionStatsWriter::default_path();
        assert_eq!(
            writer.path().to_str().unwrap(),
            "/var/log/anolisa/sls/ops/skillfs.jsonl"
        );
    }
}
