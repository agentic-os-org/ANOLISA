//! Observability import into the owner-aware v2 system store (#6605 phase 5).
//!
//! The v1 observability stream has no owner field and no stable identifier,
//! which shapes everything here:
//!
//! * the destination is the system store's own schema — the v1 shape plus a
//!   converged `owner` column — so imported rows carry the **verified** source
//!   owner and nothing else decides who can see them later;
//! * idempotency cannot ride an `event_id` conflict clause. A row is matched
//!   by its full content **including the owner**: a re-run, a crashed attempt
//!   being completed, or a second source with the same content all find the
//!   row already present instead of duplicating it;
//! * rollback needs a handle the destination owns, so the journal records the
//!   destination row ids this run inserted (or verified already present).

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use asc_observability::ObservabilityRecord;
use asc_persistence_sqlite::observability::{
    SYSTEM_OBSERVABILITY_SQLITE_SCHEMA_VERSION, SYSTEM_OBSERVABILITY_TABLES,
};
use asc_sqlite_kernel::{KernelError, SqliteStore};
use rusqlite::{Connection, OptionalExtension};

use crate::journal::ObservabilitySourceStats;
use crate::source::{
    self, FileIdentity, JsonlScan, ObservabilitySourceRow, ObservabilitySqliteScan,
};

/// The insert statement of the observability system store.
const INSERT_SQL: &str = "INSERT INTO observability_events (hook, observed_at, \
     observed_at_epoch, session_id, run_id, metrics_json, metadata_json, call_id, tool_call_id, \
     owner) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// Finds one row by full content, owner included.
///
/// `observed_at` (the wire text) pins the instant exactly as the source wrote
/// it; two rows that spell the same instant differently are different content.
/// `call_id` / `tool_call_id` compare with `IS` so `NULL` matches `NULL`.
const FIND_SQL: &str = "SELECT id FROM observability_events WHERE owner = ?1 AND hook = ?2 AND \
     observed_at = ?3 AND session_id = ?4 AND run_id = ?5 AND metrics_json = ?6 AND \
     metadata_json = ?7 AND call_id IS ?8 AND tool_call_id IS ?9 LIMIT 1";

/// Row ids per verification/rollback lookup batch.
const ID_CHUNK: usize = 500;

/// Returns the observability destination that pairs with a security-events
/// destination.
///
/// Both streams live in the daemon's data directory, so the observability
/// system store is the `observability.db` beside the security-events database
/// the operator named — the same rule the daemon's own resolution produces.
#[must_use]
pub fn destination_for(security_destination: &Path) -> PathBuf {
    security_destination
        .parent()
        .unwrap_or(security_destination)
        .join("observability.db")
}

/// Opens the observability system store and forces schema convergence.
///
/// A missing file is created; a v1-shaped file (revision 1, no `owner`)
/// converges in place, because the system schema version is higher and the
/// kernel cannot take its version-match fast path.
///
/// # Errors
///
/// Propagates kernel open/convergence failures.
pub fn open_destination(path: &Path) -> Result<SqliteStore, crate::MigratorError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let store = SqliteStore::new(
        path,
        false,
        SYSTEM_OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        SYSTEM_OBSERVABILITY_TABLES,
        None,
        asc_observability::OBSERVABILITY_LOG_PREFIX,
    )?;
    store.with_connection(true, |_| Ok(()))?;
    Ok(store)
}

/// Opens the observability system store read-only for `verify`, without
/// creating or converging anything.
///
/// # Errors
///
/// Fails when the store file does not exist: verification is read-only, so a
/// missing database is an error, not something to create.
pub fn open_destination_readonly(path: &Path) -> Result<SqliteStore, crate::MigratorError> {
    if !path.is_file() {
        return Err(crate::MigratorError::DestinationUnusable {
            path: path.display().to_string(),
            reason: "observability destination does not exist; run apply first".to_owned(),
        });
    }
    Ok(SqliteStore::new(
        path,
        true,
        SYSTEM_OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        SYSTEM_OBSERVABILITY_TABLES,
        None,
        asc_observability::OBSERVABILITY_LOG_PREFIX,
    )?)
}

/// The content identity of one observability row under one owner.
///
/// Every field is compared byte for byte; the epoch column is derived from the
/// wire text, so the text alone decides identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowKey {
    /// Verified owner the row carries.
    pub owner: u32,
    /// Hook name as stored.
    pub hook: String,
    /// Wire-format observation timestamp.
    pub observed_at: String,
    /// Session correlation.
    pub session_id: String,
    /// Run correlation.
    pub run_id: String,
    /// Serialized metrics object.
    pub metrics_json: String,
    /// Serialized metadata object.
    pub metadata_json: String,
    /// Optional LLM call correlation.
    pub call_id: Option<String>,
    /// Optional tool call correlation.
    pub tool_call_id: Option<String>,
}

impl RowKey {
    fn of(owner: u32, row: &ObservabilitySourceRow) -> Self {
        Self {
            owner,
            hook: row.hook.clone(),
            observed_at: row.observed_at.clone(),
            session_id: row.session_id.clone(),
            run_id: row.run_id.clone(),
            metrics_json: row.metrics_json.clone(),
            metadata_json: row.metadata_json.clone(),
            call_id: row.call_id.clone(),
            tool_call_id: row.tool_call_id.clone(),
        }
    }
}

/// Converts a recovered `JSONL` record into the row form the importer writes.
///
/// Metrics and metadata are re-serialized through the record's own
/// serialization — the same filtering the v1 `SQLite` writer applied — so a
/// recovered row lands exactly where the lost `SQLite` row would have.
///
/// # Errors
///
/// Returns a message when either JSON blob cannot be serialized.
fn row_of_record(record: &ObservabilityRecord) -> Result<ObservabilitySourceRow, String> {
    let metrics_json = record
        .metrics()
        .to_json_string()
        .map_err(|err| format!("metrics must be an object: {err}"))?;
    let metadata_json = record
        .metadata()
        .to_json_string()
        .map_err(|err| format!("metadata must be an object: {err}"))?;
    let metadata = record.metadata();
    Ok(ObservabilitySourceRow {
        hook: record.hook().as_str().to_owned(),
        observed_at: record.observed_at_iso(),
        observed_at_epoch: record.observed_at_epoch(),
        session_id: metadata.session_id.clone(),
        run_id: metadata.run_id.clone(),
        metrics_json,
        metadata_json,
        call_id: metadata.call_id.clone(),
        tool_call_id: metadata.tool_call_id.clone(),
    })
}

/// Finds a row by content, or inserts it. Returns `(inserted, destination id)`.
///
/// The id of an already-present row is returned too: the journal's
/// responsibility set must cover rows a crashed earlier attempt imported, so
/// a completing run can still roll them back.
fn find_or_insert(
    conn: &Connection,
    owner_uid: u32,
    row: &ObservabilitySourceRow,
) -> Result<(bool, i64), KernelError> {
    let found = conn
        .query_row(
            FIND_SQL,
            rusqlite::params![
                i64::from(owner_uid),
                row.hook,
                row.observed_at,
                row.session_id,
                row.run_id,
                row.metrics_json,
                row.metadata_json,
                row.call_id,
                row.tool_call_id,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok((false, id));
    }
    conn.execute(
        INSERT_SQL,
        rusqlite::params![
            row.hook,
            row.observed_at,
            row.observed_at_epoch,
            row.session_id,
            row.run_id,
            row.metrics_json,
            row.metadata_json,
            row.call_id,
            row.tool_call_id,
            i64::from(owner_uid),
        ],
    )?;
    Ok((true, conn.last_insert_rowid()))
}

/// Import state shared by one apply run's observability passes.
pub struct ObservabilityImport<'a> {
    /// The observability system store.
    store: &'a SqliteStore,
    /// Retention cutoff in epoch seconds; `None` imports every age.
    cutoff: Option<f64>,
    /// Content keys this run already handled, for cross-source counting.
    seen: &'a mut HashSet<RowKey>,
    /// Destination row ids this run is responsible for.
    rowids: &'a mut Vec<i64>,
}

impl<'a> ObservabilityImport<'a> {
    /// Builds the per-run import state.
    pub fn new(
        store: &'a SqliteStore,
        cutoff: Option<f64>,
        seen: &'a mut HashSet<RowKey>,
        rowids: &'a mut Vec<i64>,
    ) -> Self {
        Self {
            store,
            cutoff,
            seen,
            rowids,
        }
    }

    /// Reads the source's observability `SQLite` stream in batches and inserts
    /// every row that survives the retention cutoff and the run-wide dedup.
    ///
    /// Each batch is one transaction, mirroring the security-events pass.
    ///
    /// # Errors
    ///
    /// Propagates source read and destination insert failures.
    pub fn sqlite_pass(
        &mut self,
        scan: &ObservabilitySqliteScan,
        owner_uid: u32,
        stats: &mut ObservabilitySourceStats,
    ) -> Result<(), crate::MigratorError> {
        let src = source::open_read_only(scan.snapshot_db())?;
        let mut last_rowid = 0i64;
        loop {
            let batch = source::read_observability_batch(&src, last_rowid)?;
            if batch.is_empty() {
                break;
            }
            last_rowid = batch.last().map_or(last_rowid, |(rowid, _)| *rowid);
            stats.sqlite_rows_read += batch.len() as u64;

            let mut survivors: Vec<(i64, &ObservabilitySourceRow)> = Vec::new();
            for (rowid, row) in &batch {
                if let Some(cutoff) = self.cutoff
                    && row.observed_at_epoch < cutoff
                {
                    stats.retention_skipped += 1;
                    continue;
                }
                if !self.seen.insert(RowKey::of(owner_uid, row)) {
                    stats.duplicates_cross_source += 1;
                    continue;
                }
                survivors.push((*rowid, row));
            }

            let results = self.store.with_connection(true, |conn| {
                conn.execute_batch("BEGIN IMMEDIATE")?;
                let mut inserted = Vec::with_capacity(survivors.len());
                let outcome = (|| -> Result<(), KernelError> {
                    for (_, row) in &survivors {
                        inserted.push(find_or_insert(conn, owner_uid, row)?);
                    }
                    Ok(())
                })();
                match outcome {
                    Ok(()) => conn.execute_batch("COMMIT")?,
                    Err(err) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Err(err);
                    }
                }
                Ok(inserted)
            })?;
            let results = results.ok_or_else(|| crate::MigratorError::DestinationUnusable {
                path: String::new(),
                reason: "the observability destination store went away mid-operation".to_owned(),
            })?;
            for (inserted, id) in results {
                if inserted {
                    stats.imported += 1;
                } else {
                    stats.duplicates_existing += 1;
                }
                self.rowids.push(id);
            }
        }
        Ok(())
    }

    /// Fills observability gaps the `SQLite` stream is missing from the `JSONL`
    /// stream.
    ///
    /// This pass is explicit (`--recover-observability-jsonl`): the stream has
    /// no stable id, so recovery matches rows by content and stays an operator
    /// decision. Malformed recovery lines are counted, not fatal; a
    /// destination failure is.
    ///
    /// # Errors
    ///
    /// Propagates destination failures and stream-open failures.
    pub fn jsonl_pass(
        &mut self,
        scan: &JsonlScan,
        owner_uid: u32,
        stats: &mut ObservabilitySourceStats,
    ) -> Result<(), crate::MigratorError> {
        let mut fatal: Option<crate::MigratorError> = None;
        source::for_each_observability_jsonl_record(scan, |record| {
            let Ok(record) = record else {
                stats.malformed_jsonl += 1;
                return;
            };
            stats.jsonl_records_read += 1;
            let Ok(row) = row_of_record(&record) else {
                stats.malformed_jsonl += 1;
                return;
            };
            if let Some(cutoff) = self.cutoff
                && row.observed_at_epoch < cutoff
            {
                stats.retention_skipped += 1;
                return;
            }
            if !self.seen.insert(RowKey::of(owner_uid, &row)) {
                stats.duplicates_cross_source += 1;
                return;
            }
            match self
                .store
                .with_connection(true, |conn| find_or_insert(conn, owner_uid, &row))
            {
                Ok(Some((inserted, id))) => {
                    if inserted {
                        stats.imported += 1;
                    } else {
                        stats.duplicates_existing += 1;
                    }
                    self.rowids.push(id);
                }
                Ok(None) => stats.malformed_jsonl += 1,
                Err(err) => fatal = Some(err.into()),
            }
        })
        .map_err(crate::MigratorError::Jsonl)?;
        if let Some(err) = fatal {
            return Err(err);
        }
        Ok(())
    }
}

/// Returns how many of `ids` are still present in the observability store.
///
/// # Errors
///
/// Propagates `rusqlite` failures.
pub fn count_rowids_present(conn: &Connection, ids: &[i64]) -> Result<u64, rusqlite::Error> {
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT COUNT(*) FROM observability_events WHERE id IN ({placeholders})");
    conn.query_row(&sql, rusqlite::params_from_iter(ids.iter()), |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| u64::try_from(count).expect("COUNT(*) is never negative"))
}

/// Deletes the observability rows `ids` names.
///
/// # Errors
///
/// Propagates `rusqlite` failures.
pub fn delete_rowids(conn: &Connection, ids: &[i64]) -> Result<u64, rusqlite::Error> {
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("DELETE FROM observability_events WHERE id IN ({placeholders})");
    conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
        .map(|removed| removed as u64)
}

/// Runs `operation` over `ids` in bounded batches.
///
/// # Errors
///
/// Propagates `rusqlite` failures.
pub fn chunked_rowids(
    conn: &Connection,
    ids: &[i64],
    operation: fn(&Connection, &[i64]) -> Result<u64, rusqlite::Error>,
) -> Result<u64, KernelError> {
    let mut total = 0;
    for chunk in ids.chunks(ID_CHUNK) {
        total += operation(conn, chunk)?;
    }
    Ok(total)
}

/// Re-checks one file identity against the file on disk now.
#[must_use]
pub fn identity_matches(dir: &str, file: &str, identity: &FileIdentity) -> bool {
    let path = Path::new(dir).join(file);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return false;
    };
    // The full captured identity: an in-place append keeps dev/ino but grows
    // the file, so size and mtime are what distinguish real drift from a
    // replaced file.
    meta.dev() == identity.dev
        && meta.ino() == identity.ino
        && meta.size() == identity.size
        && meta.mtime() == identity.mtime
}
