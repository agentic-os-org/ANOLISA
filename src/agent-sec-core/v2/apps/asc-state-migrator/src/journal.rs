//! The migration run journal.
//!
//! Every `apply` appends one record here; `verify` and `rollback` read it.
//! The journal is a sidecar of the destination database (never a table inside
//! it) so the store's schema contract stays exactly what the daemon and v1
//! converge to. It is created `0600` in the destination's `0700` directory.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::source::FileIdentity;

/// One `apply` run, as evidence for `verify` and `rollback`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    /// Unique run identifier.
    pub run_id: String,
    /// When the run started, UTC ISO-8601.
    pub started_at: String,
    /// When the run finished, UTC ISO-8601.
    pub finished_at: String,
    /// Destination database the run wrote to.
    pub destination: String,
    /// Retention cutoff the run applied, in days (`None` = no cutoff).
    pub retention_days: Option<u32>,
    /// Per-source evidence and counters.
    pub sources: Vec<RunSource>,
    /// Totals across sources.
    pub totals: RunTotals,
    /// Event ids this run imported, for exact rollback.
    pub imported_event_ids: Vec<String>,
    /// Whether the run has been rolled back.
    pub rolled_back: bool,
    /// When the run was rolled back, if it was.
    pub rolled_back_at: Option<String>,
}

/// Per-source evidence inside a run record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSource {
    /// Source directory.
    pub dir: String,
    /// Owner the imported rows carry.
    pub owner_uid: u32,
    /// Whether the owner came from `--map-owner`.
    pub admin_mapped: bool,
    /// The directory's own `uid`.
    pub dir_uid: u32,
    /// `SQLite` stream identity at import time.
    pub sqlite_identity: Option<FileIdentity>,
    /// `JSONL` stream identity at import time.
    pub jsonl_identity: Option<FileIdentity>,
    /// Rows read from the `SQLite` stream.
    pub sqlite_rows_read: u64,
    /// Records read from the `JSONL` stream (recovery input).
    pub jsonl_records_read: u64,
    /// Rows imported into the destination.
    pub imported: u64,
    /// Rows already present in the destination.
    pub duplicates_existing: u64,
    /// Rows already imported by an earlier source in the same run.
    pub duplicates_cross_source: u64,
    /// Rows dropped by the retention cutoff.
    pub retention_skipped: u64,
    /// Rows whose recorded `uid` differed from the verified owner.
    pub uid_conflicts: u64,
    /// Malformed `JSONL` records skipped during recovery.
    pub malformed_jsonl: u64,
}

/// Run-wide counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunTotals {
    /// Rows imported into the destination.
    pub imported: u64,
    /// Rows already present in the destination.
    pub duplicates_existing: u64,
    /// Rows already imported by an earlier source in the same run.
    pub duplicates_cross_source: u64,
    /// Rows dropped by the retention cutoff.
    pub retention_skipped: u64,
    /// Rows whose recorded `uid` differed from the verified owner.
    pub uid_conflicts: u64,
    /// Malformed `JSONL` records skipped during recovery.
    pub malformed_jsonl: u64,
}

/// Returns the journal path for a destination database.
#[must_use]
pub fn journal_path(destination: &Path) -> PathBuf {
    PathBuf::from(format!("{}.migrator-journal.jsonl", destination.display()))
}

/// Loads every record, oldest first.
///
/// # Errors
///
/// Returns [`crate::MigratorError::Journal`] when the file exists but cannot
/// be read or parsed. A missing journal is an empty list, not an error.
pub fn load(path: &Path) -> Result<Vec<RunRecord>, crate::MigratorError> {
    let journal_error = |reason: String| crate::MigratorError::Journal {
        path: path.display().to_string(),
        reason,
    };
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(journal_error(err.to_string())),
    };
    let mut records = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: RunRecord = serde_json::from_str(trimmed)
            .map_err(|err| journal_error(format!("line {}: {err}", index + 1)))?;
        records.push(record);
    }
    Ok(records)
}

/// Appends one record, creating the journal `0600` on first use.
///
/// # Errors
///
/// Returns [`crate::MigratorError::Io`] or [`crate::MigratorError::Journal`]
/// when the file cannot be written.
pub fn append(path: &Path, record: &RunRecord) -> Result<(), crate::MigratorError> {
    let fresh = !path.exists();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if fresh {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let mut line = serde_json::to_string(record).map_err(|err| crate::MigratorError::Journal {
        path: path.display().to_string(),
        reason: err.to_string(),
    })?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Rewrites the journal with updated records (`rollback` marking).
///
/// # Errors
///
/// Returns [`crate::MigratorError::Io`] when the rewrite fails.
pub fn rewrite(path: &Path, records: &[RunRecord]) -> Result<(), crate::MigratorError> {
    let mut content = String::new();
    for record in records {
        let line = serde_json::to_string(record).map_err(|err| crate::MigratorError::Journal {
            path: path.display().to_string(),
            reason: err.to_string(),
        })?;
        content.push_str(&line);
        content.push('\n');
    }
    fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(run_id: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.to_owned(),
            started_at: "2026-10-08T00:00:00Z".to_owned(),
            finished_at: "2026-10-08T00:00:01Z".to_owned(),
            destination: "/dest/security-events.db".to_owned(),
            retention_days: Some(30),
            sources: vec![],
            totals: RunTotals::default(),
            imported_event_ids: vec!["e1".to_owned()],
            rolled_back: false,
            rolled_back_at: None,
        }
    }

    #[test]
    fn append_then_load_round_trips_multiple_runs() {
        let temp = tempfile::tempdir().unwrap();
        let path = journal_path(&temp.path().join("security-events.db"));
        append(&path, &sample_record("r1")).unwrap();
        append(&path, &sample_record("r2")).unwrap();
        let records = load(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].run_id, "r1");
        assert_eq!(records[1].run_id, "r2");
        assert_eq!(records[1].imported_event_ids, ["e1"]);
    }

    #[test]
    fn a_missing_journal_loads_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let path = journal_path(&temp.path().join("security-events.db"));
        assert!(load(&path).unwrap().is_empty());
    }

    #[test]
    fn the_journal_is_created_private() {
        let temp = tempfile::tempdir().unwrap();
        let path = journal_path(&temp.path().join("security-events.db"));
        append(&path, &sample_record("r1")).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn rewrite_replaces_every_record() {
        let temp = tempfile::tempdir().unwrap();
        let path = journal_path(&temp.path().join("security-events.db"));
        append(&path, &sample_record("r1")).unwrap();
        let mut records = load(&path).unwrap();
        records[0].rolled_back = true;
        records[0].rolled_back_at = Some("2026-10-08T01:00:00Z".to_owned());
        rewrite(&path, &records).unwrap();
        let reloaded = load(&path).unwrap();
        assert!(reloaded[0].rolled_back);
        assert_eq!(
            reloaded[0].rolled_back_at.as_deref(),
            Some("2026-10-08T01:00:00Z")
        );
    }
}
