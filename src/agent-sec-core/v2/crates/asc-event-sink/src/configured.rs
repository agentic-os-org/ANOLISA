//! Explicit-path security-event and observability sinks for daemon composition roots.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use asc_event_log::SecurityEventWriter;
use asc_persistence_sqlite::security_events::SqliteEventWriter;
use asc_security_events::SecurityEvent;
use asc_sqlite_kernel::MaintenanceOutcome;

use crate::SinkError;

/// Process-local lazily initialized value with an explicitly supplied path.
#[derive(Debug)]
struct Slot<T> {
    value: RwLock<Option<Arc<T>>>,
}

impl<T> Slot<T> {
    const fn new() -> Self {
        Self {
            value: RwLock::new(None),
        }
    }

    fn get_or_try_init(
        &self,
        build: impl FnOnce() -> Result<T, SinkError>,
    ) -> Result<Arc<T>, SinkError> {
        if let Some(value) = self.peek() {
            return Ok(value);
        }
        let mut guard = self.value.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(value) = guard.as_ref() {
            return Ok(Arc::clone(value));
        }
        let value = Arc::new(build()?);
        *guard = Some(Arc::clone(&value));
        Ok(value)
    }

    fn peek(&self) -> Option<Arc<T>> {
        self.value
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone)
    }
}

/// Explicit-path dual-write security-event sinks.
///
/// The daemon owns these paths and never falls back to process environment
/// resolution. `JSONL` and `SQLite` initialization remain independent as in v1.
#[derive(Debug)]
pub struct ConfiguredSecurityEventSinks {
    sqlite_path: PathBuf,
    jsonl: SecurityEventWriter,
    sqlite: Slot<SqliteEventWriter>,
}

impl ConfiguredSecurityEventSinks {
    /// Creates explicit-path sinks without touching the filesystem.
    #[must_use]
    pub fn new(jsonl_path: PathBuf, sqlite_path: PathBuf) -> Self {
        Self {
            sqlite_path,
            jsonl: SecurityEventWriter::new(jsonl_path),
            sqlite: Slot::new(),
        }
    }

    /// Prepares the JSONL file at the configured path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the configured path cannot be prepared.
    pub fn warm_jsonl(&self) -> Result<(), SinkError> {
        self.jsonl.probe()?;
        Ok(())
    }

    /// Builds the `SQLite` writer at the configured path.
    ///
    /// # Errors
    ///
    /// Returns a construction error if the configured path cannot be prepared.
    pub fn warm_sqlite(&self) -> Result<(), SinkError> {
        self.sqlite_writer()?.probe()?;
        Ok(())
    }

    /// Prepares optional query indexes separately from required storage readiness.
    ///
    /// # Errors
    /// Returns index preparation failures for the composition root to diagnose.
    pub fn prepare_query_indexes(&self) -> Result<(), SinkError> {
        self.sqlite_writer()?.prepare_query_indexes()?;
        Ok(())
    }

    /// Dual-writes one event while isolating the two persistence paths.
    pub fn log_event(&self, event: &SecurityEvent) {
        let jsonl = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.jsonl.write(event);
        }));
        if jsonl.is_err() {
            tracing::warn!(target: "asc_process_diagnostic", "security_event_jsonl_callback_failed");
        }
        let sqlite = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match self.sqlite_writer() {
                Ok(writer) => writer.write(event),
                Err(_) => {
                    tracing::warn!(target: "asc_process_diagnostic", "security_event_sqlite_initialization_failed");
                }
            }
        }));
        if sqlite.is_err() {
            tracing::warn!(target: "asc_process_diagnostic", "security_event_sqlite_callback_failed");
        }
    }

    /// Runs maintenance and closes the configured `SQLite` writer if initialized.
    pub fn close(&self) {
        self.close_with_deadline(None);
    }

    /// [`close`](ConfiguredSecurityEventSinks::close) with a deadline for
    /// the gated final maintenance pass; see
    /// [`SqliteSink::close_with_deadline`].
    pub fn close_with_deadline(&self, deadline: Option<std::time::Instant>) {
        if let Some(writer) = self.sqlite.peek() {
            writer.close_with_deadline(deadline);
        }
    }

    /// Runs the gated `SQLite` retention pass without closing the writer.
    ///
    /// v1's retention rode on short-lived CLI processes exiting through
    /// `atexit`; a daemon that keeps running never takes that path, so
    /// expired events survive until an orderly shutdown. The daemon calls
    /// this once at startup (the catch-up after a hard kill) and then on a
    /// fixed cadence; the shared cross-process gate decides whether anything
    /// actually runs. A failed pass does not advance the gate, so the next
    /// call retries.
    #[must_use]
    pub fn run_sqlite_retention(&self, now: f64) -> MaintenanceOutcome {
        self.sqlite
            .peek()
            .map_or(MaintenanceOutcome::NotDue, |writer| {
                writer.run_maintenance_at(now)
            })
    }

    fn sqlite_writer(&self) -> Result<Arc<SqliteEventWriter>, SinkError> {
        self.sqlite
            .get_or_try_init(|| Ok(SqliteEventWriter::new(&self.sqlite_path)?))
    }
}

/// Explicit-path foreground observability sinks, independent of process globals.
#[derive(Debug)]
pub struct ConfiguredObservabilitySinks {
    sqlite_path: PathBuf,
    jsonl: asc_event_log::ObservabilityWriter,
    sqlite: Slot<asc_persistence_sqlite::observability::ObservabilitySqliteWriter>,
}

impl ConfiguredObservabilitySinks {
    /// Configures paths without opening either stream.
    #[must_use]
    pub fn new(jsonl_path: PathBuf, sqlite_path: PathBuf) -> Self {
        Self {
            sqlite_path,
            jsonl: asc_event_log::ObservabilityWriter::new(jsonl_path),
            sqlite: Slot::new(),
        }
    }

    /// Initializes the daemon schema before admitting read queries.
    ///
    /// # Errors
    /// Propagates initialization or storage failures without silently enabling an unscoped reader.
    pub fn warm_sqlite(&self) -> Result<(), SinkError> {
        self.sqlite_writer()?.probe()?;
        Ok(())
    }

    /// Writes JSONL, then atomically inserts the authenticated owner and `SQLite` record.
    ///
    /// # Errors
    /// Propagates either destination's failure; `SQLite` failure may follow a JSONL append.
    pub fn record(
        &self,
        record: &asc_observability::ObservabilityRecord,
        uid: u32,
    ) -> Result<(), SinkError> {
        self.jsonl.write(record, uid)?;
        self.sqlite_writer()?.write_or_raise(record, uid)?;
        Ok(())
    }

    fn sqlite_writer(
        &self,
    ) -> Result<Arc<asc_persistence_sqlite::observability::ObservabilitySqliteWriter>, SinkError>
    {
        self.sqlite.get_or_try_init(|| {
            Ok(
                asc_persistence_sqlite::observability::ObservabilitySqliteWriter::new(
                    &self.sqlite_path,
                )?,
            )
        })
    }

    /// Runs retention maintenance and closes `SQLite` if a record initialized it.
    pub fn close(&self) {
        self.close_with_deadline(None);
    }

    /// [`close`](ConfiguredObservabilitySinks::close) with a deadline for
    /// the gated final maintenance pass; see
    /// [`SqliteSink::close_with_deadline`].
    pub fn close_with_deadline(&self, deadline: Option<std::time::Instant>) {
        if let Some(writer) = self.sqlite.peek() {
            writer.close_with_deadline(deadline);
        }
    }

    /// Runs the gated `SQLite` retention pass without closing the writer.
    ///
    /// The daemon's periodic retention task calls this for the observability
    /// store alongside the security store's pass; the shared cross-process
    /// gate decides whether anything actually runs. A failed pass does not
    /// advance the gate's marker, so the next call retries. After a failed
    /// startup warm this returns `NotDue` until the next record re-warms
    /// the writer - the observability stream is best-effort by design.
    #[must_use]
    pub fn run_sqlite_retention(&self, now: f64) -> MaintenanceOutcome {
        self.sqlite
            .peek()
            .map_or(MaintenanceOutcome::NotDue, |writer| {
                writer.run_maintenance_at(now)
            })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Map;

    use super::*;

    #[test]
    fn run_sqlite_retention_runs_once_per_window_without_closing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sqlite = dir.path().join("events.db");
        let sinks =
            ConfiguredSecurityEventSinks::new(dir.path().join("events.jsonl"), sqlite.clone());

        // Before the SQLite writer is warmed there is nothing to maintain.
        assert_eq!(
            sinks.run_sqlite_retention(1000.0),
            MaintenanceOutcome::NotDue
        );

        sinks.warm_sqlite().expect("warm sqlite");
        assert_eq!(sinks.run_sqlite_retention(1000.0), MaintenanceOutcome::Ran);
        let marker = sqlite.with_extension(std::ffi::OsString::from("db.maintenance"));
        let _ = marker;
        assert_eq!(
            sinks.run_sqlite_retention(1001.0),
            MaintenanceOutcome::NotDue
        );
        sinks.close();
    }

    #[test]
    fn warm_initializes_both_explicit_destinations_without_events() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jsonl = dir.path().join("events.jsonl");
        let sqlite = dir.path().join("events.db");
        let sinks = ConfiguredSecurityEventSinks::new(jsonl.clone(), sqlite.clone());

        assert!(!jsonl.exists());
        assert!(!sqlite.exists());
        sinks.warm_jsonl().expect("warm jsonl");
        sinks.warm_sqlite().expect("warm sqlite");

        assert!(jsonl.exists());
        assert!(sqlite.exists());
        assert_eq!(
            fs::read_to_string(&jsonl).expect("jsonl").lines().count(),
            0
        );
        sinks.log_event(&SecurityEvent::new("code_scan", "code_scan", Map::new()));
        assert_eq!(fs::read_to_string(jsonl).expect("jsonl").lines().count(), 1);
    }

    /// DPROC-QRY-001: optional query DDL must not determine writer readiness.
    #[test]
    fn query_index_failure_does_not_disable_required_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let sinks =
            ConfiguredSecurityEventSinks::new(dir.path().join("events.jsonl"), path.clone());
        sinks.warm_sqlite().unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE idx_security_uid_time (id INTEGER)")
            .unwrap();
        assert!(sinks.prepare_query_indexes().is_err());
        sinks.warm_sqlite().unwrap();
        sinks.log_event(&SecurityEvent::new("code_scan", "code_scan", Map::new()));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM security_events", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
        conn.execute_batch("DROP TABLE idx_security_uid_time")
            .unwrap();
        // Index preparation retries at startup with a fresh writer connection.
        sinks.close();
        sinks.prepare_query_indexes().unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_security_uid_%'", [], |r| r.get::<_, u64>(0)).unwrap(), 3);
    }

    #[test]
    fn warm_failures_are_isolated_by_destination() {
        let dir = tempfile::tempdir().expect("temp dir");
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, b"blocked").expect("block path");
        let sqlite = dir.path().join("events.db");
        let sinks = ConfiguredSecurityEventSinks::new(blocked.join("events.jsonl"), sqlite.clone());

        assert!(sinks.warm_jsonl().is_err());
        sinks.warm_sqlite().expect("warm independent sqlite");
        assert!(sqlite.exists());

        fs::remove_file(&blocked).expect("unblock path");
        sinks.log_event(&SecurityEvent::new("code_scan", "code_scan", Map::new()));
        assert_eq!(
            fs::read_to_string(blocked.join("events.jsonl"))
                .expect("recovered jsonl")
                .lines()
                .count(),
            1
        );
        fs::remove_dir_all(&blocked).expect("remove recovered directory");
        fs::write(&blocked, b"blocked").expect("block path again");

        let jsonl = dir.path().join("events.jsonl");
        let sinks = ConfiguredSecurityEventSinks::new(jsonl.clone(), blocked.join("events.db"));
        sinks.warm_jsonl().expect("warm independent jsonl");
        assert!(sinks.warm_sqlite().is_err());
        assert!(jsonl.exists());
    }

    #[test]
    fn observability_run_sqlite_retention_runs_once_per_window_without_closing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sinks = ConfiguredObservabilitySinks::new(
            dir.path().join("observability.jsonl"),
            dir.path().join("observability.db"),
        );

        // Before the SQLite writer is warmed there is nothing to maintain.
        assert_eq!(
            sinks.run_sqlite_retention(1000.0),
            MaintenanceOutcome::NotDue
        );

        sinks.warm_sqlite().expect("warm sqlite");
        assert_eq!(sinks.run_sqlite_retention(1000.0), MaintenanceOutcome::Ran);
        assert_eq!(
            sinks.run_sqlite_retention(1001.0),
            MaintenanceOutcome::NotDue
        );
        sinks.close();
    }
}

#[cfg(test)]
mod observability_tests {
    use super::ConfiguredObservabilitySinks;
    use crate::SinkError;
    use crate::test_support::{record, temp_dir};
    use asc_persistence_sqlite::observability::ObservabilityReader;
    use std::fs;

    /// QRY-004: the configured daemon sink requires a UID on every write.
    #[test]
    fn both_paths_receive_the_record() {
        let dir = temp_dir();
        let log = dir.path().join("observability.jsonl");
        let db = dir.path().join("observability.db");

        let sinks = ConfiguredObservabilitySinks::new(log.clone(), db.clone());

        assert!(!log.exists());
        assert!(!db.exists());
        sinks.close();
        assert!(
            !db.exists(),
            "closing unused sinks must not initialize storage"
        );
        let owners = [0, 1000, u32::MAX];
        for uid in owners {
            sinks.record(&record(), uid).expect("both paths");
        }
        let jsonl_owners: Vec<u32> = fs::read_to_string(&log)
            .expect("log")
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["uid"]
                    .as_u64()
                    .unwrap()
                    .try_into()
                    .unwrap()
            })
            .collect();
        assert_eq!(jsonl_owners, owners);
        assert_eq!(ObservabilityReader::new(&db).expect("reader").count(), 3);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let sqlite_owners = conn
            .prepare("SELECT uid FROM observability_events ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, u32>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(sqlite_owners, owners);
        sinks.close();
        assert!(dir.path().join("observability.db.maintenance").exists());
    }

    #[test]
    fn a_broken_jsonl_path_surfaces_and_skips_the_sqlite_insert() {
        let dir = temp_dir();
        let log = dir.path().join("observability.jsonl");
        fs::create_dir(&log).expect("occupy the log path");
        let db = dir.path().join("observability.db");

        let sinks = ConfiguredObservabilitySinks::new(log.clone(), db.clone());

        let error = sinks
            .record(&record(), 1000)
            .expect_err("the JSONL path must raise");
        assert!(matches!(error, SinkError::EventLog(_)));

        assert!(
            !db.exists(),
            "the first statement raises, so v1 never reaches the SQLite write"
        );
    }

    #[test]
    fn a_broken_database_surfaces_after_the_jsonl_append() {
        let dir = temp_dir();
        let log = dir.path().join("observability.jsonl");
        let db = dir.path().join("observability.db");
        fs::create_dir(&db).expect("occupy the database path");

        let sinks = ConfiguredObservabilitySinks::new(log.clone(), db.clone());

        let error = sinks
            .record(&record(), 1000)
            .expect_err("the SQLite path must raise");
        assert!(matches!(error, SinkError::Kernel(_)));
        assert_eq!(
            fs::read_to_string(&log).expect("log").lines().count(),
            1,
            "the JSONL append already happened before the failure"
        );
    }
}
