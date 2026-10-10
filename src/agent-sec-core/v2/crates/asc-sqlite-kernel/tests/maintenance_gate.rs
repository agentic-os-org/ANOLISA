//! The maintenance gate must not advance over a failed pass.
//!
//! A pass whose prune fails must leave the daily marker untouched, so the
//! next maintenance opportunity retries instead of waiting a full window.

use std::path::Path;
use std::sync::Arc;

use asc_sqlite_kernel::maintenance::{maintenance_lock_path, maintenance_marker_path};
use asc_sqlite_kernel::{
    ColumnSpec, Fault, FaultPolicy, KernelError, MaintenanceOutcome, Outcome, RecordRepository,
    SqliteSink, SqliteStore, TableSpec,
};
use rusqlite::Connection;
use tempfile::TempDir;

const WIDGETS: &[TableSpec] = &[TableSpec {
    name: "widgets",
    strict: false,
    constraints: &[],
    columns: &[ColumnSpec {
        name: "id",
        definition: "TEXT PRIMARY KEY",
    }],
    indexes: &[],
    extra_columns: &[],
}];

/// A repository whose prune always fails, to drive the failure path.
struct FailingPruneRepository;

impl RecordRepository for FailingPruneRepository {
    type Record = String;

    fn tables(&self) -> &'static [TableSpec] {
        WIDGETS
    }

    fn insert_or_raise(&self, _conn: &Connection, _record: &String) -> Result<bool, KernelError> {
        Ok(true)
    }

    fn prune(
        &self,
        _conn: &Connection,
        _max_age_days: u32,
        _now: f64,
    ) -> Result<usize, KernelError> {
        Err(KernelError::io(
            "prune",
            Path::new("widgets"),
            std::io::Error::other("disk full"),
        ))
    }
}

struct SwallowPolicy;

impl FaultPolicy for SwallowPolicy {
    type Record = String;

    fn on_fault(&self, _fault: &Fault<'_>, _record: &String) -> Outcome {
        Outcome::swallow()
    }
}

fn failing_sink(path: &Path) -> SqliteSink<FailingPruneRepository, SwallowPolicy> {
    let store = Arc::new(SqliteStore::new(path, false, 1, WIDGETS, None, "[test]").expect("store"));
    SqliteSink::new(store, FailingPruneRepository, SwallowPolicy, Some(30), true)
}

#[test]
fn a_failed_pass_does_not_advance_the_gate_marker() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = failing_sink(&path);
    // The store opens lazily on the first write.
    sink.write(&String::from("w1"));

    sink.close(1000.0);

    assert!(
        !maintenance_marker_path(&path).exists(),
        "a failed prune must not advance the daily gate - the marker belongs to a successful pass only"
    );
}

/// A repository whose prune always succeeds, to drive the healthy path.
struct HealthyRepository;

impl RecordRepository for HealthyRepository {
    type Record = String;

    fn tables(&self) -> &'static [TableSpec] {
        WIDGETS
    }

    fn insert_or_raise(&self, _conn: &Connection, _record: &String) -> Result<bool, KernelError> {
        Ok(true)
    }

    fn prune(
        &self,
        _conn: &Connection,
        _max_age_days: u32,
        _now: f64,
    ) -> Result<usize, KernelError> {
        Ok(0)
    }
}

fn healthy_sink(path: &Path) -> SqliteSink<HealthyRepository, SwallowPolicy> {
    let store = Arc::new(SqliteStore::new(path, false, 1, WIDGETS, None, "[test]").expect("store"));
    SqliteSink::new(store, HealthyRepository, SwallowPolicy, Some(30), true)
}

#[test]
fn the_detailed_outcome_reports_failure_and_retries() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = failing_sink(&path);
    sink.write(&String::from("w1"));

    assert!(matches!(
        sink.run_maintenance_detailed(1000.0),
        MaintenanceOutcome::Failed(_)
    ));
    assert!(
        !maintenance_marker_path(&path).exists(),
        "the marker only belongs to a successful pass"
    );
    // The gate did not advance, so the next attempt retries instead of
    // waiting a full window.
    assert!(matches!(
        sink.run_maintenance_detailed(1001.0),
        MaintenanceOutcome::Failed(_)
    ));
    sink.close(1002.0);
}

#[test]
fn the_detailed_outcome_reports_ran_then_not_due() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = healthy_sink(&path);
    sink.write(&String::from("w1"));

    assert_eq!(
        sink.run_maintenance_detailed(1000.0),
        MaintenanceOutcome::Ran
    );
    assert!(maintenance_marker_path(&path).exists());
    assert_eq!(
        sink.run_maintenance_detailed(1001.0),
        MaintenanceOutcome::NotDue
    );
    sink.close(1002.0);
}

// #6679 review follow-up (DJOB-RET-015): the shutdown final pass must
// respect the unit's stop budget - when the remaining deadline cannot fit
// the pass, close skips the gated maintenance (the marker stays unadvanced
// and the next startup catch-up re-runs it) and still drops the connection.
#[test]
fn a_final_pass_that_cannot_fit_the_stop_budget_is_skipped() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = healthy_sink(&path);
    sink.write(&String::from("w1"));

    // A deadline already in the past leaves no budget for the pass.
    let deadline = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .expect("an instant one second in the past");
    sink.close_with_deadline(1000.0, Some(deadline));
    assert!(
        !maintenance_marker_path(&path).exists(),
        "the skipped pass must not advance the gate marker"
    );
    assert_eq!(
        sink.run_maintenance_detailed(1001.0),
        MaintenanceOutcome::NotDue,
        "the closed store must not run a later pass"
    );
}

// #6679 review follow-up (DJOB-RET-016): a deadline with budget left runs
// the final pass exactly like the deadline-less close.
#[test]
fn a_final_pass_with_budget_left_still_runs() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = healthy_sink(&path);
    sink.write(&String::from("w1"));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    sink.close_with_deadline(1000.0, Some(deadline));
    assert!(
        maintenance_marker_path(&path).exists(),
        "a pass that fits the budget must still run"
    );
}

// #6679 review follow-up: gate-internal failures (lock contention, lock-file
// open failures, marker write failures) are real, diagnosable outcomes and
// must not masquerade as `NotDue` - `NotDue` sends the caller back to the
// hourly cadence with no diagnostic and no retry.

/// Holds the gate's cross-process lock the way a concurrent v1 process would.
fn hold_the_maintenance_lock(path: &Path) -> std::fs::File {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(maintenance_lock_path(path))
        .expect("open lock file");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .expect("hold lock");
    lock
}

#[test]
fn a_contended_gate_is_not_reported_as_not_due() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = healthy_sink(&path);
    sink.write(&String::from("w1"));

    let held = hold_the_maintenance_lock(&path);
    assert_ne!(
        sink.run_maintenance_detailed(1000.0),
        MaintenanceOutcome::NotDue,
        "a contended gate is a distinct, diagnosable state - not an absence of work"
    );
    assert!(
        matches!(
            sink.run_maintenance_detailed(1001.0),
            MaintenanceOutcome::Contended
        ),
        "the still-open gate under contention must report Contended"
    );
    drop(held);
    assert_eq!(
        sink.run_maintenance_detailed(1002.0),
        MaintenanceOutcome::Ran,
        "once the lock is free the still-open gate must run"
    );
    sink.close(1003.0);
}

#[test]
fn a_lock_open_failure_is_not_reported_as_not_due() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = healthy_sink(&path);
    sink.write(&String::from("w1"));
    // A directory at the lock path makes the lock-file open fail for real.
    std::fs::create_dir(maintenance_lock_path(&path)).expect("block the lock path");

    assert!(
        matches!(
            sink.run_maintenance_detailed(1000.0),
            MaintenanceOutcome::Failed(_)
        ),
        "a lock-file open failure is an error, not a closed gate"
    );
    sink.close(1002.0);
}

#[test]
fn a_marker_write_failure_is_not_reported_as_not_due() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = healthy_sink(&path);
    sink.write(&String::from("w1"));
    // A directory at the marker path makes the atomic marker rename fail while
    // the prune itself succeeds.
    std::fs::create_dir(maintenance_marker_path(&path)).expect("block the marker path");

    assert!(
        matches!(
            sink.run_maintenance_detailed(1000.0),
            MaintenanceOutcome::Failed(_)
        ),
        "a marker write failure is an error - the gate did not advance and the caller must retry"
    );
    sink.close(1002.0);
}
