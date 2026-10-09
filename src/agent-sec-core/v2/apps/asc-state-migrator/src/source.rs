//! Source validation and stream reading.
//!
//! A source directory is only trusted after its stream files survive the
//! checks the design contract demands: regular files, no symlinks, no extra
//! hard links, not world-writable, owned by the directory's user, not written
//! to recently, and a schema revision the migrator understands. Reading is
//! always `SQLite`-first with `JSONL` as explicit gap recovery, because the
//! two streams were an independent fail-open dual write in v1 (#6605).

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use asc_event_log::jsonl::is_backup_suffix;
use asc_security_events::{
    SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
    config::{DEFAULT_SECURITY_STREAM, FALLBACK_DIR_NAME, stream_db_path_in, stream_log_path_in},
};
use rusqlite::{Connection, OpenFlags};

use crate::discovery::{DiscoveredSource, RejectedSource};

/// Rows per source read batch, matching the library migrator's batch size.
pub const SOURCE_BATCH_SIZE: i64 = 5000;

/// Columns without which a source table cannot be interpreted.
const REQUIRED_COLUMNS: &[&str] = &[
    "event_id",
    "event_type",
    "category",
    "result",
    "timestamp",
    "timestamp_epoch",
    "pid",
    "uid",
    "details",
];

/// Identity of one stream file, captured for journal evidence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    /// `st_dev` of the file.
    pub dev: u64,
    /// `st_ino` of the file.
    pub ino: u64,
    /// Size in bytes at scan time.
    pub size: u64,
    /// Modification time in epoch seconds at scan time.
    pub mtime: i64,
}

/// One validated source, with what the plan needs to know about it.
#[derive(Debug, Clone)]
pub struct SourceScan {
    /// The source directory.
    pub dir: PathBuf,
    /// Owner the imported rows will carry.
    pub owner_uid: u32,
    /// Whether the owner came from `--map-owner`.
    pub admin_mapped: bool,
    /// The directory's own `uid`.
    pub dir_uid: u32,
    /// Validated `SQLite` stream, when present.
    pub sqlite: Option<SqliteScan>,
    /// Validated `JSONL` stream, when present.
    pub jsonl: Option<JsonlScan>,
    /// Whether the directory also holds observability streams.
    ///
    /// Observability import waits on the owner-aware destination schema
    /// (issue #6605 phase 5), so its presence is reported, not acted on.
    pub observability_present: bool,
}

/// The `SQLite` side of a source.
#[derive(Debug, Clone)]
pub struct SqliteScan {
    /// Database path.
    pub path: PathBuf,
    /// File identity for the journal.
    pub identity: FileIdentity,
    /// Schema revision the source declares.
    pub user_version: u32,
    /// Rows in `security_events` at scan time.
    pub rows: u64,
}

/// The `JSONL` side of a source.
#[derive(Debug, Clone)]
pub struct JsonlScan {
    /// Main log path.
    pub path: PathBuf,
    /// Rotated backups, oldest-name first.
    pub backups: Vec<PathBuf>,
    /// File identity of the main log.
    pub identity: FileIdentity,
    /// Non-empty lines in the main log and its backups at scan time.
    pub records: u64,
}

/// One source row, exactly as the source stored it.
///
/// `recorded_uid` is the `uid` column v1 wrote; it is evidence, never the
/// owner the destination row will carry.
#[derive(Debug, Clone)]
pub struct SourceRow {
    /// Event identifier (dedup key).
    pub event_id: String,
    /// Producer-defined event kind.
    pub event_type: String,
    /// Action category.
    pub category: String,
    /// `succeeded` or `failed`.
    pub result: String,
    /// UTC ISO-8601 timestamp.
    pub timestamp: String,
    /// Epoch seconds.
    pub timestamp_epoch: f64,
    /// Middleware trace identifier, when the revision has the column.
    pub trace_id: Option<String>,
    /// Producing process id.
    pub pid: i64,
    /// The `uid` v1 recorded (untrusted as an owner).
    pub recorded_uid: i64,
    /// Session correlation, when present.
    pub session_id: Option<String>,
    /// Run correlation, when present.
    pub run_id: Option<String>,
    /// Call correlation, when present.
    pub call_id: Option<String>,
    /// Tool-call correlation, when present.
    pub tool_call_id: Option<String>,
    /// Verdict, when present.
    pub verdict: Option<String>,
    /// The `details` JSON, verbatim.
    pub details: String,
}

/// Validates and scans one discovered source.
///
/// # Errors
///
/// Returns a [`RejectedSource`] carrying the first failed check. The
/// writer-grace rejection mentions `--force` so operators get the remedy in
/// the message.
pub fn validate_and_scan(
    source: &DiscoveredSource,
    force: bool,
    writer_grace: u32,
    now_epoch: f64,
) -> Result<SourceScan, RejectedSource> {
    let reject = |reason: String| RejectedSource {
        dir: source.dir.clone(),
        reason,
    };

    let db_path = stream_db_path_in(&source.dir, DEFAULT_SECURITY_STREAM)
        .map_err(|err| reject(format!("stream name: {err}")))?;
    let jsonl_path = stream_log_path_in(&source.dir, DEFAULT_SECURITY_STREAM)
        .map_err(|err| reject(format!("stream name: {err}")))?;

    let mut newest_mtime: Option<i64> = None;
    let mut sqlite = None;
    let mut jsonl = None;

    if path_exists(&db_path) {
        let identity = check_stream_file(&db_path, source.dir_uid).map_err(reject)?;
        newest_mtime = Some(newest_mtime.unwrap_or(i64::MIN).max(identity.mtime));
        sqlite = Some(scan_sqlite(&db_path, identity).map_err(reject)?);
        // v1 runs its store in WAL mode: a live writer commits to
        // `security-events.db-wal` while the main database's mtime only
        // moves at checkpoint, so the sidecar is often the only fresh write
        // evidence. The link itself is stat'ed, never followed.
        if let Ok(meta) = fs::symlink_metadata(sidecar_of(&db_path, "-wal")) {
            newest_mtime = Some(newest_mtime.unwrap_or(i64::MIN).max(meta.mtime()));
        }
    }

    if path_exists(&jsonl_path) {
        let identity = check_stream_file(&jsonl_path, source.dir_uid).map_err(reject)?;
        newest_mtime = Some(newest_mtime.unwrap_or(i64::MIN).max(identity.mtime));
        let backups = rotated_backups(&jsonl_path);
        for backup in &backups {
            let identity = check_stream_file(backup, source.dir_uid).map_err(reject)?;
            newest_mtime = Some(newest_mtime.unwrap_or(i64::MIN).max(identity.mtime));
        }
        let records = count_jsonl_records(&jsonl_path, &backups);
        jsonl = Some(JsonlScan {
            path: jsonl_path,
            backups,
            identity,
            records,
        });
    }

    if !force {
        if let Some(mtime) = newest_mtime {
            // File mtimes in epoch seconds fit f64 exactly for any date this
            // filesystem can represent, so the cast is lossless in practice.
            #[allow(clippy::cast_precision_loss)]
            let age = (now_epoch - mtime as f64).max(0.0);
            if age < f64::from(writer_grace) {
                return Err(reject(format!(
                    "stream modified {age:.0}s ago (grace {writer_grace}s); \
                     stop v1 writers or pass --force"
                )));
            }
        }
    }

    let observability_db = stream_db_path_in(&source.dir, "observability")
        .map(|path| path_exists(&path))
        .unwrap_or(false);
    let observability_jsonl = stream_log_path_in(&source.dir, "observability")
        .map(|path| path_exists(&path))
        .unwrap_or(false);

    Ok(SourceScan {
        dir: source.dir.clone(),
        owner_uid: source.owner_uid,
        admin_mapped: source.admin_mapped,
        dir_uid: source.dir_uid,
        sqlite,
        jsonl,
        observability_present: observability_db || observability_jsonl,
    })
}

/// Applies the per-file trust checks and captures identity.
fn check_stream_file(path: &Path, dir_uid: u32) -> Result<FileIdentity, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|err| format!("{}: cannot stat: {err}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{}: is a symlink — refusing to use",
            path.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!("{}: is not a regular file", path.display()));
    }
    if meta.nlink() > 1 {
        return Err(format!(
            "{}: has {} hard links — refusing to use",
            path.display(),
            meta.nlink()
        ));
    }
    if meta.permissions().mode() & 0o002 != 0 {
        return Err(format!(
            "{}: is world-writable (mode {:o})",
            path.display(),
            meta.permissions().mode() & 0o777
        ));
    }
    if meta.uid() != dir_uid {
        return Err(format!(
            "{}: owned by uid {} but the directory is owned by uid {dir_uid}",
            path.display(),
            meta.uid()
        ));
    }
    Ok(FileIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime: meta.mtime(),
    })
}

fn scan_sqlite(path: &Path, identity: FileIdentity) -> Result<SqliteScan, String> {
    let wrap = |err: rusqlite::Error| format!("{}: {err}", path.display());
    let connection = open_read_only(path).map_err(wrap)?;
    let user_version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(wrap)?;
    if user_version > SECURITY_EVENTS_SQLITE_SCHEMA_VERSION {
        return Err(format!(
            "{}: schema revision {user_version} is newer than this migrator understands ({})",
            path.display(),
            SECURITY_EVENTS_SQLITE_SCHEMA_VERSION
        ));
    }
    let has_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='security_events')",
            [],
            |row| row.get(0),
        )
        .map_err(wrap)?;
    if !has_table {
        return Err(format!(
            "{}: no security_events table — not a v1 security-event store",
            path.display()
        ));
    }
    let columns = table_columns(&connection).map_err(wrap)?;
    for column in REQUIRED_COLUMNS {
        if !columns.contains(&(*column).to_owned()) {
            return Err(format!(
                "{}: security_events is missing required column '{column}'",
                path.display()
            ));
        }
    }
    let counted: i64 = connection
        .query_row("SELECT COUNT(*) FROM security_events", [], |row| row.get(0))
        .map_err(wrap)?;
    let rows = u64::try_from(counted).expect("COUNT(*) is never negative");
    Ok(SqliteScan {
        path: path.to_path_buf(),
        identity,
        user_version,
        rows,
    })
}

/// Opens a source database read-only.
///
/// # Errors
///
/// Propagates `rusqlite` open failures (corrupt file, locked, unreadable).
pub fn open_read_only(path: &Path) -> Result<Connection, rusqlite::Error> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

fn table_columns(connection: &Connection) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA table_info(security_events)")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

fn column_or_null(columns: &[String], name: &str) -> String {
    if columns.iter().any(|column| column == name) {
        name.to_owned()
    } else {
        format!("NULL AS {name}")
    }
}

/// Reads one batch of source rows after `last_rowid`, with each row's `rowid`
/// for paging.
///
/// Older revisions without the optional correlation columns yield `NULL`s, so
/// a rev-1 or rev-2 database migrates without being touched.
///
/// # Errors
///
/// Propagates `rusqlite` failures mid-batch.
pub fn read_batch(
    connection: &Connection,
    last_rowid: i64,
) -> Result<Vec<(i64, SourceRow)>, rusqlite::Error> {
    let columns = table_columns(connection)?;
    let sql = format!(
        "SELECT rowid AS _source_rowid_, event_id, event_type, category, result, timestamp, \
         timestamp_epoch, {trace}, pid, uid, {session}, {run}, {call}, {tool}, {verdict}, \
         details FROM security_events WHERE rowid > ?1 ORDER BY rowid LIMIT {SOURCE_BATCH_SIZE}",
        trace = column_or_null(&columns, "trace_id"),
        session = column_or_null(&columns, "session_id"),
        run = column_or_null(&columns, "run_id"),
        call = column_or_null(&columns, "call_id"),
        tool = column_or_null(&columns, "tool_call_id"),
        verdict = column_or_null(&columns, "verdict"),
    );
    let mut statement = connection.prepare(&sql)?;
    let mapped = statement
        .query_map([last_rowid], |row| {
            let rowid: i64 = row.get("_source_rowid_")?;
            let source_row = SourceRow {
                event_id: row.get("event_id")?,
                event_type: row.get("event_type")?,
                category: row.get("category")?,
                result: row.get("result")?,
                timestamp: row.get("timestamp")?,
                timestamp_epoch: row.get("timestamp_epoch")?,
                trace_id: row.get("trace_id")?,
                pid: row.get("pid")?,
                recorded_uid: row.get("uid")?,
                session_id: row.get("session_id")?,
                run_id: row.get("run_id")?,
                call_id: row.get("call_id")?,
                tool_call_id: row.get("tool_call_id")?,
                verdict: row.get("verdict")?,
                details: row.get("details")?,
            };
            Ok((rowid, source_row))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(mapped)
}

/// Streams every `JSONL` record of a source, main log then backups.
///
/// Malformed lines are reported instead of aborting: the `JSONL` stream is
/// recovery input and a crash-truncated tail must not stop the migration.
///
/// # Errors
///
/// Returns an error only when a file cannot be opened.
pub fn for_each_jsonl_record<F>(scan: &JsonlScan, mut on_record: F) -> Result<(), String>
where
    F: FnMut(Result<asc_security_events::SecurityEvent, String>),
{
    let mut paths = vec![scan.path.clone()];
    paths.extend(scan.backups.iter().cloned());
    for path in paths {
        let file = fs::File::open(&path)
            .map_err(|err| format!("{}: cannot open: {err}", path.display()))?;
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    on_record(Err(format!("{}: read error: {err}", path.display())));
                    continue;
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str(trimmed) {
                Ok(event) => on_record(Ok(event)),
                Err(err) => on_record(Err(format!("{}: {err}", path.display()))),
            }
        }
    }
    Ok(())
}

fn rotated_backups(main: &Path) -> Vec<PathBuf> {
    let Some(parent) = main.parent() else {
        return Vec::new();
    };
    let Some(stem) = main.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut backups: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(stem))
                .is_some_and(|suffix| suffix.starts_with('.') && is_backup_suffix(&suffix[1..]))
        })
        .collect();
    backups.sort();
    backups
}

fn count_jsonl_records(main: &Path, backups: &[PathBuf]) -> u64 {
    let mut paths = vec![main.to_path_buf()];
    paths.extend(backups.iter().cloned());
    let mut records = 0u64;
    for path in paths {
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                records += 1;
            }
        }
    }
    records
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

// Sibling of a stream file with a raw suffix appended, e.g. `<db>-wal`.
fn sidecar_of(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| std::ffi::OsString::from("stream"), ToOwned::to_owned);
    name.push(suffix);
    path.with_file_name(name)
}

/// The v1 fallback directory name, re-exported for operators.
#[must_use]
pub fn fallback_dir_name() -> &'static str {
    FALLBACK_DIR_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_event_row(connection: &Connection, id: &str, epoch: f64) {
        connection
            .execute(
                "INSERT INTO security_events (event_id, event_type, category, result, timestamp, \
                 timestamp_epoch, trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, \
                 verdict, details) VALUES (?1, 't', 'c', 'succeeded', '2026-01-01T00:00:00Z', ?2, \
                 '', 1, 1000, NULL, NULL, NULL, NULL, NULL, '{}')",
                rusqlite::params![id, epoch],
            )
            .unwrap();
    }

    fn full_schema(connection: &Connection) {
        connection
            .execute_batch(
                "CREATE TABLE security_events (event_id TEXT NOT NULL PRIMARY KEY, event_type TEXT \
             NOT NULL, category TEXT NOT NULL, result TEXT NOT NULL DEFAULT 'succeeded', \
             timestamp TEXT NOT NULL, timestamp_epoch FLOAT NOT NULL, trace_id TEXT NOT NULL \
             DEFAULT '', pid INTEGER NOT NULL, uid INTEGER NOT NULL, session_id TEXT, run_id \
             TEXT, call_id TEXT, tool_call_id TEXT, verdict TEXT, details TEXT NOT NULL)",
            )
            .unwrap();
    }

    #[test]
    fn read_batch_adapts_to_a_table_without_the_correlation_columns() {
        let temp = tempfile::tempdir().unwrap();
        let connection = Connection::open(temp.path().join("old.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE security_events (event_id TEXT NOT NULL PRIMARY KEY, event_type TEXT \
             NOT NULL, category TEXT NOT NULL, result TEXT NOT NULL, timestamp TEXT NOT NULL, \
             timestamp_epoch FLOAT NOT NULL, trace_id TEXT NOT NULL DEFAULT '', pid INTEGER \
             NOT NULL, uid INTEGER NOT NULL, details TEXT NOT NULL)",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO security_events (event_id, event_type, category, result, timestamp, \
                 timestamp_epoch, trace_id, pid, uid, details) VALUES ('rev2-event', 't', 'c', \
                 'succeeded', '2026-01-01T00:00:00Z', 1.0, '', 1, 1000, '{}')",
                [],
            )
            .unwrap();

        let rows = read_batch(&connection, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.event_id, "rev2-event");
        assert_eq!(rows[0].1.session_id, None);
        assert_eq!(rows[0].1.verdict, None);
        assert_eq!(rows[0].1.recorded_uid, 1000);
    }

    #[test]
    fn read_batch_pages_by_rowid() {
        let temp = tempfile::tempdir().unwrap();
        let connection = Connection::open(temp.path().join("paged.db")).unwrap();
        full_schema(&connection);
        for index in 0..3 {
            write_event_row(&connection, &format!("e{index}"), 1.0);
        }
        let first = read_batch(&connection, 0).unwrap();
        assert_eq!(first.len(), 3);
        assert!(read_batch(&connection, 3).unwrap().is_empty());
    }

    #[test]
    fn a_future_schema_version_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("future.db");
        let connection = Connection::open(&path).unwrap();
        full_schema(&connection);
        connection
            .pragma_update(
                None,
                "user_version",
                SECURITY_EVENTS_SQLITE_SCHEMA_VERSION + 1,
            )
            .unwrap();
        drop(connection);

        let err = scan_sqlite(
            &path,
            FileIdentity {
                dev: 0,
                ino: 0,
                size: 0,
                mtime: 0,
            },
        )
        .expect_err("future revision must be rejected");
        assert!(err.contains("newer than this migrator"));
    }

    #[test]
    fn rotated_backups_are_sorted_and_suffix_checked() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("security-events.jsonl");
        fs::write(&main, "{}\n").unwrap();
        fs::write(
            temp.path()
                .join("security-events.jsonl.20260101-000000.000"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            temp.path()
                .join("security-events.jsonl.20260102-000000.000"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("security-events.jsonl.not-a-stamp"),
            "{}\n",
        )
        .unwrap();

        let backups = rotated_backups(&main);
        assert_eq!(backups.len(), 2, "only rotation-stamped files count");
        assert!(backups[0].to_string_lossy().contains("20260101-000000.000"));
    }
}
