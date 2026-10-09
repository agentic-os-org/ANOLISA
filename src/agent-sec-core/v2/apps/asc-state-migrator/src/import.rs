//! Destination handling: `apply`, `verify` and `rollback`.
//!
//! The destination is the v2 system store the daemon itself owns, opened
//! through the same kernel path the daemon's writer uses, so schema
//! convergence happens once and identically. Imports are idempotent by
//! `event_id`, journaled per run for exact rollback, and never touch a source
//! file.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asc_persistence_sqlite::security_events::writer::LOG_PREFIX;
use asc_security_events::{
    SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, SecurityEvent,
    config::daemon_security_event_paths_readonly,
};
use asc_sqlite_kernel::{KernelError, SqliteStore, current_epoch};
use rusqlite::Connection;

use crate::discovery::{DiscoveryOptions, RejectedSource};
use crate::journal::{self, RunRecord, RunSource, RunTotals};
use crate::report::{
    ApplyReport, PlanReport, RollbackReport, RolledBackRun, RunVerification, SourcePlanEntry,
    SourceVerification, VerifyReport,
};
use crate::source::{self, FileIdentity, SourceRow, SourceScan};

/// The insert statement of the v1/v2 writer, mirrored verbatim.
pub const INSERT_SQL: &str = "INSERT INTO security_events (event_id, event_type, category, \
     result, timestamp, timestamp_epoch, trace_id, pid, uid, session_id, run_id, call_id, \
     tool_call_id, verdict, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, \
     ?12, ?13, ?14, ?15) ON CONFLICT(event_id) DO NOTHING";

/// Event ids per verification/rollback lookup batch.
const ID_CHUNK: usize = 500;

/// Options of one `apply`.
pub struct ApplyOptions {
    /// Retention window in days; `None` disables the cutoff.
    pub retention_days: Option<u32>,
    /// Whether `JSONL` gap recovery runs after the `SQLite` pass.
    pub jsonl_recovery: bool,
    /// Observability retention window in days; `None` disables the cutoff
    /// (#6605 phase 5).
    pub observability_retention_days: Option<u32>,
    /// Whether the explicit observability `JSONL` recovery pass runs
    /// (#6605 phase 5).
    pub observability_jsonl_recovery: bool,
    /// Wall clock at run start, injectable for tests.
    pub now_epoch: f64,
}

/// Unwraps a kernel `Option` that `raise_on_error` guarantees to be `Some`.
fn required<T>(value: Option<T>) -> Result<T, crate::MigratorError> {
    value.ok_or_else(|| crate::MigratorError::DestinationUnusable {
        path: String::new(),
        reason: "the destination store went away mid-operation".to_owned(),
    })
}

/// Resolves the destination database path.
///
/// Purely a path decision: creating (or chmod'ing) the parent directory is a
/// write side effect that belongs to the write commands' store opening, so
/// `plan` and `verify` never touch the filesystem here. The daemon default
/// is resolved through the read-only variant of the daemon's path helper,
/// which also does not prepare the data directory.
///
/// # Errors
///
/// Fails when an explicit path is relative, or when the daemon default cannot
/// be resolved.
pub fn resolve_destination(explicit: Option<&Path>) -> Result<PathBuf, crate::MigratorError> {
    match explicit {
        Some(path) => {
            if !path.is_absolute() {
                return Err(crate::MigratorError::DestinationUnusable {
                    path: path.display().to_string(),
                    reason: "destination must be an absolute path".to_owned(),
                });
            }
            Ok(path.to_path_buf())
        }
        None => Ok(daemon_security_event_paths_readonly()?.1),
    }
}

/// Opens the destination store through the daemon's kernel path and forces
/// schema convergence.
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
        SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
        asc_persistence_sqlite::security_events::SECURITY_EVENTS_TABLES,
        Some(Arc::new(
            asc_persistence_sqlite::security_events::SecurityEventsMigrator,
        )),
        LOG_PREFIX,
    )?;
    store.with_connection(true, |_| Ok(()))?;
    Ok(store)
}

/// Opens the destination store read-only for `verify`, without creating or
/// migrating anything.
///
/// # Errors
///
/// Fails when the destination file does not exist: verification is read-only,
/// so a missing database is an error, not something to create.
pub fn open_destination_readonly(path: &Path) -> Result<SqliteStore, crate::MigratorError> {
    if !path.is_file() {
        return Err(crate::MigratorError::DestinationUnusable {
            path: path.display().to_string(),
            reason: "does not exist; run apply first".to_owned(),
        });
    }
    Ok(SqliteStore::new(
        path,
        true,
        SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
        asc_persistence_sqlite::security_events::SECURITY_EVENTS_TABLES,
        None,
        LOG_PREFIX,
    )?)
}

/// Discovery plus validation of every configured source.
///
/// Rejections of scan-found directories are soft (returned for the report);
/// the orchestrator decides which of them were explicit `--source` arguments
/// and escalates those to fatal errors before `apply`.
///
/// `open_jsonl` is false for a `--sqlite-only` run, so a damaged `JSONL`
/// stream cannot reject a source whose `SQLite` stream is the intended
/// input.
pub fn scan_sources(
    options: &DiscoveryOptions,
    force: bool,
    writer_grace: u32,
    now_epoch: f64,
    open_jsonl: bool,
) -> (Vec<SourceScan>, Vec<RejectedSource>, Vec<PathBuf>) {
    let discovery = crate::discovery::discover(options);
    let mut rejected = discovery.rejected;
    let mut scans = Vec::new();
    for source in &discovery.sources {
        match source::validate_and_scan(source, force, writer_grace, now_epoch, open_jsonl) {
            Ok(scan) => scans.push(scan),
            Err(rejection) => rejected.push(rejection),
        }
    }
    (scans, rejected, discovery.system_owned)
}

/// Builds the read-only `plan` report.
///
/// # Errors
///
/// Propagates destination-resolution failures only; source problems are
/// reported, never fatal, in a plan.
pub fn plan(
    options: &DiscoveryOptions,
    destination: &Path,
    retention_days: Option<u32>,
    force: bool,
    writer_grace: u32,
    open_jsonl: bool,
) -> Result<PlanReport, crate::MigratorError> {
    let (scans, rejected, system_owned) =
        scan_sources(options, force, writer_grace, current_epoch(), open_jsonl);

    let sources = scans
        .iter()
        .map(|scan| SourcePlanEntry {
            dir: scan.dir.display().to_string(),
            owner_uid: scan.owner_uid,
            admin_mapped: scan.admin_mapped,
            sqlite_rows: scan.sqlite.as_ref().map(|sqlite| sqlite.rows),
            sqlite_schema: scan.sqlite.as_ref().map(|sqlite| sqlite.user_version),
            jsonl_records: scan.jsonl.as_ref().map(|jsonl| jsonl.records),
            jsonl_backups: scan.jsonl.as_ref().map_or(0, |jsonl| jsonl.backups.len()),
            observability_sqlite_rows: scan
                .observability
                .as_ref()
                .and_then(|observability| observability.sqlite.as_ref())
                .map(|sqlite| sqlite.rows),
            observability_schema: scan
                .observability
                .as_ref()
                .and_then(|observability| observability.sqlite.as_ref())
                .map(|sqlite| sqlite.user_version),
            observability_jsonl_records: scan
                .observability
                .as_ref()
                .and_then(|observability| observability.jsonl.as_ref())
                .map(|jsonl| jsonl.records),
            observability_jsonl_backups: scan
                .observability
                .as_ref()
                .and_then(|observability| observability.jsonl.as_ref())
                .map_or(0, |jsonl| jsonl.backups.len()),
        })
        .collect();

    Ok(PlanReport {
        destination: destination.display().to_string(),
        observability_destination: crate::observability::destination_for(destination)
            .display()
            .to_string(),
        retention_days,
        sources,
        rejected: rejected
            .into_iter()
            .map(|item| (item.dir.display().to_string(), item.reason))
            .collect(),
        system_owned: system_owned
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

/// Imports every validated source into the destination and journals the run.
///
/// `rejected` carries the discovery-found sources validation skipped, so a
/// partial migration still tells the operator whose state was left behind;
/// explicit `--source` rejections never reach this point (they abort first).
///
/// # Errors
///
/// Fails when there is no usable source, or when a destination or source
/// `SQLite` operation fails.
pub fn apply(
    scans: &[SourceScan],
    rejected: &[crate::discovery::RejectedSource],
    destination: &Path,
    options: &ApplyOptions,
) -> Result<ApplyReport, crate::MigratorError> {
    if scans.is_empty() {
        return Err(crate::MigratorError::NoSources);
    }

    let store = open_destination(destination)?;
    let observability_destination = crate::observability::destination_for(destination);
    let observability_store = crate::observability::open_destination(&observability_destination)?;
    let cutoff = options
        .retention_days
        .map(|days| options.now_epoch - f64::from(days) * 86_400.0);
    let observability_cutoff = options
        .observability_retention_days
        .map(|days| options.now_epoch - f64::from(days) * 86_400.0);

    let started_at = asc_security_events::timestamp::now_iso();
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut state = RunState::default();
    let mut run_sources = Vec::with_capacity(scans.len());
    for scan in scans {
        run_sources.push(import_source(
            scan,
            &store,
            &observability_store,
            cutoff,
            observability_cutoff,
            options,
            &mut state,
        )?);
    }

    let totals = run_sources
        .iter()
        .fold(RunTotals::default(), |mut acc, item| {
            acc.imported += item.imported;
            acc.duplicates_existing += item.duplicates_existing;
            acc.duplicates_cross_source += item.duplicates_cross_source;
            acc.retention_skipped += item.retention_skipped;
            acc.uid_conflicts += item.uid_conflicts;
            acc.malformed_jsonl += item.malformed_jsonl;
            if let Some(observability) = &item.observability {
                acc.observability.imported += observability.imported;
                acc.observability.duplicates_existing += observability.duplicates_existing;
                acc.observability.duplicates_cross_source += observability.duplicates_cross_source;
                acc.observability.retention_skipped += observability.retention_skipped;
                acc.observability.malformed_jsonl += observability.malformed_jsonl;
            }
            acc
        });

    let finished_at = asc_security_events::timestamp::now_iso();
    let record = RunRecord {
        run_id: run_id.clone(),
        started_at,
        finished_at,
        destination: destination.display().to_string(),
        retention_days: options.retention_days,
        sources: run_sources,
        totals,
        imported_event_ids: state.imported_event_ids,
        imported_observability_rowids: state.imported_observability_rowids,
        rolled_back: false,
        rolled_back_at: None,
    };
    journal::append(&journal::journal_path(destination), &record)?;

    Ok(ApplyReport {
        run_id,
        destination: destination.display().to_string(),
        journal: journal::journal_path(destination).display().to_string(),
        started_at: record.started_at.clone(),
        finished_at: record.finished_at.clone(),
        retention_days: options.retention_days,
        sources: record.sources.clone(),
        totals: record.totals.clone(),
        rejected: rejected
            .iter()
            .map(|item| (item.dir.display().to_string(), item.reason.clone()))
            .collect(),
    })
}

/// Mutable state one apply run accumulates across its sources.
#[derive(Default)]
struct RunState {
    /// Security-event ids handled this run.
    seen: HashSet<String>,
    /// Security-event ids the run is responsible for.
    imported_event_ids: Vec<String>,
    /// Observability content keys handled this run.
    observability_seen: HashSet<crate::observability::RowKey>,
    /// Observability destination row ids the run is responsible for.
    imported_observability_rowids: Vec<i64>,
}

/// Imports one source's security-events and observability streams.
///
/// # Errors
///
/// Propagates every stream failure of this source.
fn import_source(
    scan: &SourceScan,
    store: &SqliteStore,
    observability_store: &SqliteStore,
    cutoff: Option<f64>,
    observability_cutoff: Option<f64>,
    options: &ApplyOptions,
    run_state: &mut RunState,
) -> Result<RunSource, crate::MigratorError> {
    let mut stats = RunSource {
        dir: scan.dir.display().to_string(),
        owner_uid: scan.owner_uid,
        admin_mapped: scan.admin_mapped,
        dir_uid: scan.dir_uid,
        sqlite_identity: scan.sqlite.as_ref().map(|sqlite| sqlite.identity.clone()),
        sqlite_wal_identity: scan
            .sqlite
            .as_ref()
            .and_then(|sqlite| sqlite.wal_identity.clone()),
        jsonl_identity: scan.jsonl.as_ref().map(|jsonl| jsonl.identity.clone()),
        sqlite_rows_read: 0,
        jsonl_records_read: 0,
        imported: 0,
        duplicates_existing: 0,
        duplicates_cross_source: 0,
        retention_skipped: 0,
        uid_conflicts: 0,
        malformed_jsonl: 0,
        observability: None,
    };
    let mut observability_stats =
        scan.observability
            .as_ref()
            .map(|observability| crate::journal::ObservabilitySourceStats {
                sqlite_identity: observability
                    .sqlite
                    .as_ref()
                    .map(|sqlite| sqlite.identity.clone()),
                sqlite_wal_identity: observability
                    .sqlite
                    .as_ref()
                    .and_then(|sqlite| sqlite.wal_identity.clone()),
                jsonl_identity: observability
                    .jsonl
                    .as_ref()
                    .map(|jsonl| jsonl.identity.clone()),
                ..crate::journal::ObservabilitySourceStats::default()
            });

    let mut import = SourceImport {
        store,
        owner_uid: scan.owner_uid,
        cutoff,
        seen: &mut run_state.seen,
        imported_event_ids: &mut run_state.imported_event_ids,
    };
    if let Some(sqlite_scan) = &scan.sqlite {
        import.sqlite_pass(sqlite_scan, &mut stats)?;
    }
    if options.jsonl_recovery
        && let Some(jsonl_scan) = &scan.jsonl
    {
        import.jsonl_pass(jsonl_scan, &mut stats)?;
    }

    if let Some(observability_scan) = &scan.observability {
        let mut observability_import = crate::observability::ObservabilityImport::new(
            observability_store,
            observability_cutoff,
            &mut run_state.observability_seen,
            &mut run_state.imported_observability_rowids,
        );
        let stats_ref = observability_stats
            .get_or_insert_with(crate::journal::ObservabilitySourceStats::default);
        if let Some(sqlite_scan) = &observability_scan.sqlite {
            observability_import.sqlite_pass(sqlite_scan, scan.owner_uid, stats_ref)?;
        }
        if options.observability_jsonl_recovery
            && let Some(jsonl_scan) = &observability_scan.jsonl
        {
            observability_import.jsonl_pass(jsonl_scan, scan.owner_uid, stats_ref)?;
        }
    }
    stats.observability = observability_stats;
    Ok(stats)
}

/// Mutable state shared by one source's two import passes.
struct SourceImport<'a> {
    store: &'a SqliteStore,
    owner_uid: u32,
    cutoff: Option<f64>,
    seen: &'a mut HashSet<String>,
    imported_event_ids: &'a mut Vec<String>,
}

impl SourceImport<'_> {
    /// Reads the source's `SQLite` stream in batches and inserts every row
    /// that survives the retention cutoff and the run-wide dedup.
    ///
    /// Reads the source's `SQLite` stream in batches and inserts every row
    /// that survives the retention cutoff and the run-wide dedup.
    ///
    /// The connection opens the snapshot taken through the descriptor that
    /// passed validation, so a source directory swapped after the scan
    /// cannot redirect the import.
    ///
    /// # Errors
    ///
    /// Propagates source read and destination insert failures.
    fn sqlite_pass(
        &mut self,
        scan: &crate::source::SqliteScan,
        stats: &mut RunSource,
    ) -> Result<(), crate::MigratorError> {
        let src = source::open_read_only(scan.snapshot_db())?;
        let mut last_rowid = 0i64;
        loop {
            let batch = source::read_batch(&src, last_rowid)?;
            if batch.is_empty() {
                break;
            }
            last_rowid = batch.last().map_or(last_rowid, |(rowid, _)| *rowid);
            stats.sqlite_rows_read += batch.len() as u64;

            let mut rows_to_insert: Vec<&SourceRow> = Vec::new();
            for (_, row) in &batch {
                if let Some(cutoff) = self.cutoff
                    && row.timestamp_epoch < cutoff
                {
                    stats.retention_skipped += 1;
                    continue;
                }
                if !self.seen.insert(row.event_id.clone()) {
                    stats.duplicates_cross_source += 1;
                    continue;
                }
                if row.recorded_uid != i64::from(self.owner_uid) {
                    stats.uid_conflicts += 1;
                }
                rows_to_insert.push(row);
            }

            let results = required(self.store.with_connection(true, |conn| {
                insert_rows_transactional(conn, &rows_to_insert, self.owner_uid)
            })?)?;
            for (inserted, row) in results.into_iter().zip(rows_to_insert) {
                if inserted {
                    stats.imported += 1;
                } else {
                    stats.duplicates_existing += 1;
                }
                // The responsibility set covers every source id the run
                // verified in the destination, not only fresh inserts: a
                // re-run completing a crashed attempt must be able to roll
                // back rows the crashed attempt imported.
                self.imported_event_ids.push(row.event_id.clone());
            }
        }
        Ok(())
    }

    /// Fills gaps the `SQLite` stream is missing from the `JSONL` stream.
    ///
    /// Malformed recovery lines are counted, not fatal; a destination failure
    /// is.
    ///
    /// # Errors
    ///
    /// Propagates destination failures and stream-open failures.
    fn jsonl_pass(
        &mut self,
        scan: &crate::source::JsonlScan,
        stats: &mut RunSource,
    ) -> Result<(), crate::MigratorError> {
        let mut fatal: Option<crate::MigratorError> = None;
        source::for_each_jsonl_record(scan, |record| {
            let Ok(event) = record else {
                stats.malformed_jsonl += 1;
                return;
            };
            stats.jsonl_records_read += 1;
            let Ok(epoch) =
                asc_security_events::timestamp::utc_iso_to_epoch(&event.timestamp, "timestamp")
            else {
                stats.malformed_jsonl += 1;
                return;
            };
            let Ok(details) = serde_json::to_string(&event.details) else {
                stats.malformed_jsonl += 1;
                return;
            };
            if let Some(cutoff) = self.cutoff
                && epoch < cutoff
            {
                stats.retention_skipped += 1;
                return;
            }
            if !self.seen.insert(event.event_id.clone()) {
                stats.duplicates_cross_source += 1;
                return;
            }
            if u64::from(event.uid) != u64::from(self.owner_uid) {
                stats.uid_conflicts += 1;
            }
            match self.store.with_connection(true, |conn| {
                insert_event_row(conn, &event, self.owner_uid, epoch, &details)
            }) {
                Ok(Some(true)) => {
                    stats.imported += 1;
                    self.imported_event_ids.push(event.event_id.clone());
                }
                Ok(Some(false)) => {
                    stats.duplicates_existing += 1;
                    self.imported_event_ids.push(event.event_id.clone());
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

fn insert_rows_transactional(
    conn: &Connection,
    rows: &[&SourceRow],
    owner_uid: u32,
) -> Result<Vec<bool>, KernelError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let mut results = Vec::with_capacity(rows.len());
    let outcome = (|| -> Result<(), rusqlite::Error> {
        for row in rows {
            let changed = conn.execute(
                INSERT_SQL,
                rusqlite::params![
                    row.event_id,
                    row.event_type,
                    row.category,
                    row.result,
                    row.timestamp,
                    row.timestamp_epoch,
                    row.trace_id.clone().unwrap_or_default(),
                    row.pid,
                    i64::from(owner_uid),
                    row.session_id,
                    row.run_id,
                    row.call_id,
                    row.tool_call_id,
                    row.verdict,
                    row.details,
                ],
            )?;
            results.push(changed > 0);
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(results)
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err.into())
        }
    }
}

fn insert_event_row(
    conn: &Connection,
    event: &SecurityEvent,
    owner_uid: u32,
    epoch: f64,
    details: &str,
) -> Result<bool, KernelError> {
    let result = match event.result {
        asc_security_events::EventResult::Succeeded => "succeeded",
        asc_security_events::EventResult::Failed => "failed",
    };
    let changed = conn.execute(
        INSERT_SQL,
        rusqlite::params![
            event.event_id,
            event.event_type,
            event.category,
            result,
            event.timestamp,
            epoch,
            event.trace_id,
            event.pid,
            i64::from(owner_uid),
            event.session_id,
            event.run_id,
            event.call_id,
            event.tool_call_id,
            asc_security_events::extract_verdict(&event.details),
            details,
        ],
    )?;
    Ok(changed > 0)
}

/// Re-checks journaled runs against the destination.
///
/// # Errors
///
/// Fails when the journal cannot be read, a requested run is unknown, or the
/// destination cannot be opened.
pub fn verify(
    destination: &Path,
    run_id: Option<&str>,
) -> Result<VerifyReport, crate::MigratorError> {
    let records = journal::load(&journal::journal_path(destination))?;
    let selected: Vec<&RunRecord> = match run_id {
        Some(id) => {
            let record = records
                .iter()
                .find(|record| record.run_id == id)
                .ok_or_else(|| crate::MigratorError::RunNotFound(id.to_owned()))?;
            vec![record]
        }
        // The CLI documents a bare `verify` as checking the most recent run,
        // not the whole history: an older run's mismatch must not fail the
        // latest healthy migration.
        None => records
            .last()
            .map(|record| vec![record])
            .unwrap_or_default(),
    };

    let store = open_destination_readonly(destination)?;
    let quick_check = required(store.with_connection(true, |conn| {
        conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .map_err(KernelError::from)
    })?)?;

    let observability_destination = crate::observability::destination_for(destination);
    let observability_store =
        crate::observability::open_destination_readonly(&observability_destination)?;
    let observability_quick_check =
        required(observability_store.with_connection(true, |conn| {
            conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .map_err(KernelError::from)
        })?)?;

    let mut runs = Vec::new();
    for record in selected {
        runs.push(verify_run(record, &store, &observability_store)?);
    }

    Ok(VerifyReport {
        destination: destination.display().to_string(),
        quick_check,
        observability_destination: observability_destination.display().to_string(),
        observability_quick_check,
        runs,
    })
}

/// Re-checks the journaled `WAL` sidecar identity against the sidecar on
/// disk now.
///
/// A checkpoint folds the sidecar into the main database and deletes it, so
/// an absent sidecar is a normal end state rather than drift: the main
/// database's own comparison covers the checkpoint because the checkpoint
/// rewrote that file. Only a sidecar that still exists and differs from the
/// journaled identity reports as changed — the `WAL`-only commit a
/// post-run writer can make without touching the main database.
fn wal_identity_matches(dir: &str, file: &str, identity: &FileIdentity) -> bool {
    let path = Path::new(dir).join(file);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return true;
    };
    meta.dev() == identity.dev
        && meta.ino() == identity.ino
        && meta.size() == identity.size
        && meta.mtime() == identity.mtime
}

/// Re-checks one journaled run against both destination stores.
///
/// # Errors
///
/// Propagates destination read failures.
fn verify_run(
    record: &RunRecord,
    store: &SqliteStore,
    observability_store: &SqliteStore,
) -> Result<RunVerification, crate::MigratorError> {
    let present = required(store.with_connection(true, |conn| {
        chunked_ids(conn, &record.imported_event_ids, count_ids_present)
    })?)?;
    let expected = if record.rolled_back {
        0
    } else {
        record.imported_event_ids.len()
    };
    let observability_present = required(observability_store.with_connection(true, |conn| {
        crate::observability::chunked_rowids(
            conn,
            &record.imported_observability_rowids,
            crate::observability::count_rowids_present,
        )
    })?)?;
    let observability_expected = if record.rolled_back {
        0
    } else {
        record.imported_observability_rowids.len()
    };
    let sources = record
        .sources
        .iter()
        .map(|item| SourceVerification {
            dir: item.dir.clone(),
            sqlite_unchanged: item
                .sqlite_identity
                .as_ref()
                .map(|identity| identity_matches(&item.dir, "security-events.db", identity)),
            sqlite_wal_unchanged: item
                .sqlite_wal_identity
                .as_ref()
                .map(|identity| {
                    wal_identity_matches(&item.dir, "security-events.db-wal", identity)
                }),
            jsonl_unchanged: item
                .jsonl_identity
                .as_ref()
                .map(|identity| identity_matches(&item.dir, "security-events.jsonl", identity)),
            observability_sqlite_unchanged: item
                .observability
                .as_ref()
                .and_then(|observability| observability.sqlite_identity.as_ref())
                .map(|identity| {
                    crate::observability::identity_matches(&item.dir, "observability.db", identity)
                }),
            observability_sqlite_wal_unchanged: item
                .observability
                .as_ref()
                .and_then(|observability| observability.sqlite_wal_identity.as_ref())
                .map(|identity| {
                    wal_identity_matches(&item.dir, "observability.db-wal", identity)
                }),
            observability_jsonl_unchanged: item
                .observability
                .as_ref()
                .and_then(|observability| observability.jsonl_identity.as_ref())
                .map(|identity| {
                    crate::observability::identity_matches(
                        &item.dir,
                        "observability.jsonl",
                        identity,
                    )
                }),
        })
        .collect();
    Ok(RunVerification {
        run_id: record.run_id.clone(),
        rolled_back: record.rolled_back,
        expected_present: expected as u64,
        present,
        observability_expected_present: observability_expected as u64,
        observability_present,
        ok: present == expected as u64 && observability_present == observability_expected as u64,
        sources,
    })
}

fn identity_matches(dir: &str, file: &str, identity: &FileIdentity) -> bool {
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

/// Removes the rows journaled runs imported.
///
/// # Errors
///
/// Fails when no selector is given, the run is unknown or already rolled
/// back, or the destination refuses the deletes.
pub fn rollback(
    destination: &Path,
    run_id: Option<&str>,
    all: bool,
) -> Result<RollbackReport, crate::MigratorError> {
    if run_id.is_none() && !all {
        return Err(crate::MigratorError::Journal {
            path: journal::journal_path(destination).display().to_string(),
            reason: "pass --run-id or --all".to_owned(),
        });
    }
    let journal_path = journal::journal_path(destination);
    let mut records = journal::load(&journal_path)?;
    if records.is_empty() {
        return Err(crate::MigratorError::RunNotFound(
            run_id.unwrap_or("<any>").to_owned(),
        ));
    }

    let mut targets: Vec<usize> = Vec::new();
    if let Some(id) = run_id {
        let index = records
            .iter()
            .position(|record| record.run_id == id)
            .ok_or_else(|| crate::MigratorError::RunNotFound(id.to_owned()))?;
        if records[index].rolled_back {
            return Err(crate::MigratorError::AlreadyRolledBack(id.to_owned()));
        }
        targets.push(index);
    } else {
        for (index, record) in records.iter().enumerate() {
            if !record.rolled_back {
                targets.push(index);
            }
        }
    }

    let store = open_destination(destination)?;
    let observability_destination = crate::observability::destination_for(destination);
    let observability_store = crate::observability::open_destination(&observability_destination)?;
    let mut runs = Vec::new();
    let now = asc_security_events::timestamp::now_iso();
    for index in targets {
        let ids = records[index].imported_event_ids.clone();
        let removed =
            required(store.with_connection(true, |conn| chunked_ids(conn, &ids, delete_ids))?)?;
        let rowids = records[index].imported_observability_rowids.clone();
        let observability_removed =
            required(observability_store.with_connection(true, |conn| {
                crate::observability::chunked_rowids(
                    conn,
                    &rowids,
                    crate::observability::delete_rowids,
                )
            })?)?;
        records[index].rolled_back = true;
        records[index].rolled_back_at = Some(now.clone());
        runs.push(RolledBackRun {
            run_id: records[index].run_id.clone(),
            requested: ids.len() as u64,
            removed,
            observability_requested: rowids.len() as u64,
            observability_removed,
        });
    }
    journal::rewrite(&journal_path, &records)?;

    Ok(RollbackReport {
        destination: destination.display().to_string(),
        runs,
    })
}

fn chunked_ids(
    conn: &Connection,
    ids: &[String],
    operation: fn(&Connection, &[String]) -> Result<u64, rusqlite::Error>,
) -> Result<u64, KernelError> {
    let mut total = 0;
    for chunk in ids.chunks(ID_CHUNK) {
        total += operation(conn, chunk)?;
    }
    Ok(total)
}

fn count_ids_present(conn: &Connection, ids: &[String]) -> Result<u64, rusqlite::Error> {
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT COUNT(*) FROM security_events WHERE event_id IN ({placeholders})");
    conn.query_row(&sql, rusqlite::params_from_iter(ids.iter()), |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| u64::try_from(count).expect("COUNT(*) is never negative"))
}

fn delete_ids(conn: &Connection, ids: &[String]) -> Result<u64, rusqlite::Error> {
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("DELETE FROM security_events WHERE event_id IN ({placeholders})");
    conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
        .map(|removed| removed as u64)
}
