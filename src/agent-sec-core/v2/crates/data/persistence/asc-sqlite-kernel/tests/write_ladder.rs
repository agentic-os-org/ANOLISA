//! Rung-by-rung coverage of the write ladder in `sink.rs`.
//!
//! Each test drives one classification branch of v1's decision tree and asserts
//! the `(kind, phase, busy)` triple the policy observes, because that triple is
//! the whole contract between the shared ladder and the per-stream policy.

use std::path::Path;
use std::sync::{Arc, Mutex};

use asc_sqlite_kernel::{
    ColumnSpec, Fault, FaultPolicy, KernelError, Outcome, Phase, RecordRepository, SqliteSink,
    SqliteStore, TableSpec, WriteFault,
};
use rusqlite::Connection;
use tempfile::TempDir;

const WIDGETS: &[TableSpec] = &[TableSpec {
    name: "widgets",
    columns: &[ColumnSpec {
        name: "id",
        definition: "TEXT PRIMARY KEY",
    }],
    indexes: &[],
    extra_columns: &[],
}];

/// What a scripted insert attempt should do.
enum Step {
    Wrote,
    Skipped,
    Fail(fn() -> KernelError),
    Real,
}

/// A repository whose insert outcomes are scripted up front.
struct ScriptedRepository {
    steps: Mutex<std::collections::VecDeque<Step>>,
    attempts: Mutex<usize>,
}

impl ScriptedRepository {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            attempts: Mutex::new(0),
        }
    }

    fn attempts(&self) -> usize {
        *self.attempts.lock().expect("attempts")
    }
}

impl RecordRepository for ScriptedRepository {
    type Record = String;

    fn tables(&self) -> &'static [TableSpec] {
        WIDGETS
    }

    fn insert_or_raise(
        &self,
        conn: &Connection,
        record: &Self::Record,
    ) -> Result<bool, KernelError> {
        *self.attempts.lock().expect("attempts") += 1;
        let step = self
            .steps
            .lock()
            .expect("steps")
            .pop_front()
            .unwrap_or(Step::Real);
        match step {
            Step::Wrote => Ok(true),
            Step::Skipped => Ok(false),
            Step::Fail(make) => Err(make()),
            Step::Real => {
                let changed = conn.execute(
                    "INSERT INTO widgets (id) VALUES (?1) ON CONFLICT(id) DO NOTHING",
                    [record],
                )?;
                Ok(changed > 0)
            }
        }
    }

    fn prune(&self, conn: &Connection, max_age_days: u32, _now: f64) -> Result<usize, KernelError> {
        let _ = max_age_days;
        Ok(conn.execute("DELETE FROM widgets WHERE id = 'stale'", [])?)
    }
}

/// A policy that records every fault and replies with a fixed outcome.
struct RecordingPolicy {
    seen: Mutex<Vec<(WriteFault, Phase, bool)>>,
    reply: fn() -> Outcome,
}

impl RecordingPolicy {
    fn swallowing() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            reply: Outcome::swallow,
        }
    }

    fn seen(&self) -> Vec<(WriteFault, Phase, bool)> {
        self.seen.lock().expect("seen").clone()
    }
}

impl FaultPolicy for RecordingPolicy {
    type Record = String;

    fn on_fault(&self, fault: &Fault<'_>, _record: &String) -> Outcome {
        self.seen
            .lock()
            .expect("seen")
            .push((fault.kind, fault.phase, fault.busy));
        (self.reply)()
    }
}

fn store(path: &Path) -> Arc<SqliteStore> {
    Arc::new(SqliteStore::new(path, false, 1, WIDGETS, None, "[test]").expect("store"))
}

fn sink(path: &Path, steps: Vec<Step>) -> SqliteSink<ScriptedRepository, RecordingPolicy> {
    SqliteSink::new(
        store(path),
        ScriptedRepository::new(steps),
        RecordingPolicy::swallowing(),
        None,
        true,
    )
}

/// Shorthand for the faults the sink's policy observed.
fn seen(sink: &SqliteSink<ScriptedRepository, RecordingPolicy>) -> Vec<(WriteFault, Phase, bool)> {
    sink.policy().seen()
}

fn busy() -> KernelError {
    KernelError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(5),
        Some("database is locked".to_owned()),
    ))
}

fn corrupt() -> KernelError {
    KernelError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(11),
        Some("database disk image is malformed".to_owned()),
    ))
}

fn schema_drift() -> KernelError {
    KernelError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(1),
        Some("table widgets has no column named verdict".to_owned()),
    ))
}

fn other_database() -> KernelError {
    KernelError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(19),
        Some("CHECK constraint failed".to_owned()),
    ))
}

fn malformed() -> KernelError {
    KernelError::Malformed("record rejected".to_owned())
}

fn io_fault() -> KernelError {
    KernelError::io(
        "write to",
        Path::new("/nowhere/events.db"),
        std::io::Error::other("disk gone"),
    )
}

#[test]
fn a_successful_write_reports_no_fault() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(&dir.path().join("events.db"), vec![]);
    sink.write(&"a".to_owned());
    assert!(seen(&sink).is_empty());
}

#[test]
fn an_insert_that_wrote_nothing_is_skipped() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(&dir.path().join("events.db"), vec![Step::Skipped]);
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Skipped, Phase::Insert, false)]
    );
}

#[test]
fn a_malformed_record_never_retries() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(&dir.path().join("events.db"), vec![Step::Fail(malformed)]);
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Malformed, Phase::Insert, false)]
    );
    assert_eq!(sink.repository().attempts(), 1);
}

#[test]
fn an_io_fault_reports_the_io_phase() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(&dir.path().join("events.db"), vec![Step::Fail(io_fault)]);
    sink.write(&"a".to_owned());
    assert_eq!(seen(&sink), vec![(WriteFault::Io, Phase::Io, false)]);
}

#[test]
fn a_busy_database_is_reported_as_busy() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(&dir.path().join("events.db"), vec![Step::Fail(busy)]);
    sink.write(&"a".to_owned());
    assert_eq!(seen(&sink), vec![(WriteFault::Busy, Phase::Insert, true)]);
    assert_eq!(
        sink.repository().attempts(),
        1,
        "a busy database must not be retried"
    );
}

#[test]
fn schema_drift_requests_a_repair() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(
        &dir.path().join("events.db"),
        vec![Step::Fail(schema_drift)],
    );
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Schema, Phase::Insert, false)]
    );
    assert!(
        sink.store().repair_requested(),
        "schema drift must schedule a convergence pass"
    );
}

#[test]
fn any_other_database_error_stops_before_the_retry() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(
        &dir.path().join("events.db"),
        vec![Step::Fail(other_database)],
    );
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Database, Phase::Insert, false)]
    );
    assert_eq!(sink.repository().attempts(), 1);
    assert!(!sink.store().repair_requested());
}

#[test]
fn corruption_retries_once_and_can_succeed() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(
        &dir.path().join("events.db"),
        vec![Step::Fail(corrupt), Step::Wrote],
    );
    sink.write(&"a".to_owned());
    assert!(seen(&sink).is_empty(), "a successful retry is not a fault");
    assert_eq!(sink.repository().attempts(), 2);
}

#[test]
fn a_retry_that_stays_busy_carries_the_busy_flag() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(
        &dir.path().join("events.db"),
        vec![Step::Fail(corrupt), Step::Fail(busy)],
    );
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Busy, Phase::CorruptionRetry, true)],
        "the busy flag is what keeps observability from disposing here"
    );
}

#[test]
fn a_retry_that_fails_malformed_is_not_busy() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(
        &dir.path().join("events.db"),
        vec![Step::Fail(corrupt), Step::Fail(malformed)],
    );
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Malformed, Phase::CorruptionRetry, false)],
        "this is the one rung where the two v1 flows genuinely disagree"
    );
}

#[test]
fn a_retry_that_wrote_nothing_reports_skipped_in_the_retry_phase() {
    let dir = TempDir::new().expect("temp dir");
    let sink = sink(
        &dir.path().join("events.db"),
        vec![Step::Fail(corrupt), Step::Skipped],
    );
    sink.write(&"a".to_owned());
    assert_eq!(
        seen(&sink),
        vec![(WriteFault::Skipped, Phase::CorruptionRetry, false)]
    );
}

#[test]
fn a_policy_can_dispose_and_surface_a_message() {
    struct Strict;
    impl FaultPolicy for Strict {
        type Record = String;

        fn on_fault(&self, _fault: &Fault<'_>, _record: &String) -> Outcome {
            Outcome::fail("failed to write event".to_owned()).with_dispose()
        }
    }

    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = SqliteSink::new(
        store(&path),
        ScriptedRepository::new(vec![Step::Fail(other_database)]),
        Strict,
        None,
        false,
    );

    let err = sink
        .write_or_raise(&"a".to_owned())
        .expect_err("the policy asked for a failure");
    assert!(
        err.to_string().contains("failed to write event"),
        "unexpected message: {err}"
    );
    assert!(!sink.store().is_open(), "the policy asked for a dispose");
    assert!(!sink.serializes_writes());
}

#[test]
fn a_policy_can_propagate_the_original_error() {
    struct Propagating;
    impl FaultPolicy for Propagating {
        type Record = String;

        fn on_fault(&self, _fault: &Fault<'_>, _record: &String) -> Outcome {
            Outcome::propagate()
        }
    }

    let dir = TempDir::new().expect("temp dir");
    let sink = SqliteSink::new(
        store(&dir.path().join("events.db")),
        ScriptedRepository::new(vec![Step::Fail(malformed)]),
        Propagating,
        None,
        false,
    );

    assert!(matches!(
        sink.write_or_raise(&"a".to_owned()),
        Err(KernelError::Malformed(_))
    ));
}

#[test]
fn write_swallows_everything_the_policy_surfaces() {
    struct Strict;
    impl FaultPolicy for Strict {
        type Record = String;

        fn on_fault(&self, _fault: &Fault<'_>, _record: &String) -> Outcome {
            Outcome::fail("boom".to_owned())
        }
    }

    let dir = TempDir::new().expect("temp dir");
    let sink = SqliteSink::new(
        store(&dir.path().join("events.db")),
        ScriptedRepository::new(vec![Step::Fail(other_database)]),
        Strict,
        None,
        false,
    );
    // The contract of `write` is that it never propagates.
    sink.write(&"a".to_owned());
}

#[test]
fn close_on_an_unopened_store_runs_no_maintenance() {
    let dir = TempDir::new().expect("temp dir");
    let db = dir.path().join("events.db");
    let sink = sink(&db, vec![]);
    sink.close(1000.0);
    assert!(
        !dir.path().join("events.db.maintenance").exists(),
        "an untouched store must not leave a maintenance marker"
    );
}

#[test]
fn close_runs_the_gated_maintenance_pass() {
    let dir = TempDir::new().expect("temp dir");
    let db = dir.path().join("events.db");
    let sink = SqliteSink::new(
        store(&db),
        ScriptedRepository::new(vec![]),
        RecordingPolicy::swallowing(),
        Some(7),
        true,
    );
    sink.write(&"stale".to_owned());
    sink.close(1000.0);

    let marker = dir.path().join("events.db.maintenance");
    assert!(marker.exists(), "close must refresh the gate marker");
    assert!(!sink.store().is_open());

    let reader = store(&db);
    let count = reader
        .with_connection(true, |conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM widgets", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .expect("read")
        .expect("value");
    assert_eq!(count, 0, "retention must have pruned the stale row");
}
