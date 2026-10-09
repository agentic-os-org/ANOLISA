//! Report shapes and rendering.
//!
//! Every command produces one serializable report; `--json` prints it as
//! JSON, the default prints a short human summary. The JSON form is the
//! machine-readable migration evidence the design contract asks for.

use serde::Serialize;

use crate::journal::{RunSource, RunTotals};

/// What `plan` found.
#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    /// Destination database the apply would write to.
    pub destination: String,
    /// Retention cutoff an apply would apply, in days (`None` = disabled).
    pub retention_days: Option<u32>,
    /// Validated sources with their counts.
    pub sources: Vec<SourcePlanEntry>,
    /// Rejected candidates with reasons.
    pub rejected: Vec<(String, String)>,
    /// Directories skipped because they already are the system store.
    pub system_owned: Vec<String>,
}

/// One source's plan entry.
#[derive(Debug, Clone, Serialize)]
pub struct SourcePlanEntry {
    /// Source directory.
    pub dir: String,
    /// Owner imported rows would carry.
    pub owner_uid: u32,
    /// Whether the owner came from `--map-owner`.
    pub admin_mapped: bool,
    /// Rows in the `SQLite` stream, when present.
    pub sqlite_rows: Option<u64>,
    /// Schema revision of the `SQLite` stream, when present.
    pub sqlite_schema: Option<u32>,
    /// Records in the `JSONL` stream, when present.
    pub jsonl_records: Option<u64>,
    /// Rotated `JSONL` backups found.
    pub jsonl_backups: usize,
    /// Whether the directory also holds observability streams (deferred).
    pub observability_deferred: bool,
}

/// What `apply` did.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    /// Run identifier (also in the journal).
    pub run_id: String,
    /// Destination database.
    pub destination: String,
    /// Journal file the run was appended to.
    pub journal: String,
    /// When the run started, UTC ISO-8601.
    pub started_at: String,
    /// When the run finished, UTC ISO-8601.
    pub finished_at: String,
    /// Retention cutoff applied, in days (`None` = disabled).
    pub retention_days: Option<u32>,
    /// Per-source evidence and counters.
    pub sources: Vec<RunSource>,
    /// Totals across sources.
    pub totals: RunTotals,
    /// Discovery-found sources that were skipped by validation. Explicit
    /// `--source` rejections abort the run instead; these are the ones an
    /// operator must still see after a partial migration.
    pub rejected: Vec<(String, String)>,
}

/// What `verify` checked.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    /// Destination database.
    pub destination: String,
    /// Result of `PRAGMA quick_check` on the destination.
    pub quick_check: String,
    /// One entry per journaled run examined.
    pub runs: Vec<RunVerification>,
}

/// One run's verification outcome.
#[derive(Debug, Clone, Serialize)]
pub struct RunVerification {
    /// Run identifier.
    pub run_id: String,
    /// Whether the run was rolled back.
    pub rolled_back: bool,
    /// Rows the journal says should be present (0 when rolled back).
    pub expected_present: u64,
    /// Rows actually present.
    pub present: u64,
    /// Whether expected and present agree.
    pub ok: bool,
    /// Per-source file-identity checks.
    pub sources: Vec<SourceVerification>,
}

/// One source's verify outcome.
#[derive(Debug, Clone, Serialize)]
pub struct SourceVerification {
    /// Source directory.
    pub dir: String,
    /// Whether the `SQLite` file still has the journaled identity.
    pub sqlite_unchanged: Option<bool>,
    /// Whether the snapshotted `WAL` sidecar still has the journaled
    /// identity (`None` when the run had no frame-bearing sidecar).
    pub sqlite_wal_unchanged: Option<bool>,
    /// Whether the `JSONL` file still has the journaled identity.
    pub jsonl_unchanged: Option<bool>,
}

/// What `rollback` removed.
#[derive(Debug, Clone, Serialize)]
pub struct RollbackReport {
    /// Destination database.
    pub destination: String,
    /// One entry per rolled-back run.
    pub runs: Vec<RolledBackRun>,
}

/// One rolled-back run.
#[derive(Debug, Clone, Serialize)]
pub struct RolledBackRun {
    /// Run identifier.
    pub run_id: String,
    /// Rows the journal recorded for the run.
    pub requested: u64,
    /// Rows actually deleted.
    pub removed: u64,
}

use std::fmt::Write as _;

/// Renders the plan summary a human reads by default.
#[must_use]
pub fn render_plan(report: &PlanReport) -> String {
    let mut out = String::new();
    let retention = match report.retention_days {
        Some(days) => format!("{days}d"),
        None => "off".to_owned(),
    };
    let _ = writeln!(
        out,
        "destination: {} (retention {retention})",
        report.destination
    );
    if report.sources.is_empty() {
        let _ = writeln!(out, "sources: none");
    }
    for source in &report.sources {
        let mapped = if source.admin_mapped {
            " (admin-mapped)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "source {}: owner {}{mapped}",
            source.dir, source.owner_uid
        );
        if let Some(rows) = source.sqlite_rows {
            let _ = writeln!(
                out,
                "  sqlite: {rows} rows (schema rev {})",
                source.sqlite_schema.unwrap_or(0)
            );
        }
        if let Some(records) = source.jsonl_records {
            let _ = writeln!(
                out,
                "  jsonl: {records} records (+{} backups)",
                source.jsonl_backups
            );
        }
        if source.observability_deferred {
            let _ = writeln!(
                out,
                "  observability: present, deferred to the owner-aware schema"
            );
        }
    }
    for (dir, reason) in &report.rejected {
        let _ = writeln!(out, "rejected {dir}: {reason}");
    }
    for dir in &report.system_owned {
        let _ = writeln!(out, "skipped {dir}: already the system store");
    }
    out
}

/// Renders the apply summary a human reads by default.
#[must_use]
pub fn render_apply(report: &ApplyReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "run {} -> {}", report.run_id, report.destination);
    for source in &report.sources {
        let _ = writeln!(
            out,
            "source {} (owner {}): imported {}, already-present {}, cross-source {}, \
             retention-skipped {}, uid-conflicts {}, malformed-jsonl {}",
            source.dir,
            source.owner_uid,
            source.imported,
            source.duplicates_existing,
            source.duplicates_cross_source,
            source.retention_skipped,
            source.uid_conflicts,
            source.malformed_jsonl
        );
    }
    let totals = &report.totals;
    let _ = writeln!(
        out,
        "totals: imported {}, already-present {}, cross-source {}, retention-skipped {}, \
         uid-conflicts {}, malformed-jsonl {}",
        totals.imported,
        totals.duplicates_existing,
        totals.duplicates_cross_source,
        totals.retention_skipped,
        totals.uid_conflicts,
        totals.malformed_jsonl
    );
    let _ = writeln!(out, "journal: {}", report.journal);
    for (dir, reason) in &report.rejected {
        let _ = writeln!(out, "rejected {dir}: {reason}");
    }
    out
}

/// Renders the verify summary a human reads by default.
#[must_use]
pub fn render_verify(report: &VerifyReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "destination {} quick_check: {}",
        report.destination, report.quick_check
    );
    if report.runs.is_empty() {
        let _ = writeln!(out, "runs: none journaled yet");
    }
    for run in &report.runs {
        let state = if run.rolled_back {
            " (rolled back)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "run {}: {} present {} of {} expected{state}",
            run.run_id,
            if run.ok { "ok" } else { "MISMATCH" },
            run.present,
            run.expected_present
        );
        for source in &run.sources {
            for (label, unchanged) in [
                ("sqlite", source.sqlite_unchanged),
                ("sqlite wal", source.sqlite_wal_unchanged),
                ("jsonl", source.jsonl_unchanged),
            ] {
                if let Some(unchanged) = unchanged {
                    let _ = writeln!(
                        out,
                        "  {} {label}: {}",
                        source.dir,
                        if unchanged {
                            "unchanged"
                        } else {
                            "CHANGED since the run"
                        }
                    );
                }
            }
        }
    }
    out
}

/// Renders the rollback summary a human reads by default.
#[must_use]
pub fn render_rollback(report: &RollbackReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "destination {}", report.destination);
    if report.runs.is_empty() {
        let _ = writeln!(out, "no run rolled back");
    }
    for run in &report.runs {
        let _ = writeln!(
            out,
            "run {}: removed {} of {} journaled rows",
            run.run_id, run.removed, run.requested
        );
    }
    let _ = writeln!(out, "sources were not modified");
    out
}
