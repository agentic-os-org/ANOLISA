//! Cross-process gate for low-frequency `SQLite` maintenance.
//!
//! Migrated from v1 `sqlite_maintenance.py`. The marker and lock file names, the
//! double-checked due test and the "only a durable marker advances the gate"
//! rule are all preserved.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{FlockOperation, Mode, OFlags, flock};

use crate::error::KernelError;
use crate::sink::MaintenanceOutcome;

/// Default gate interval: once per day.
pub const DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS: f64 = 24.0 * 60.0 * 60.0;

/// Returns the marker path for `db_path`.
#[must_use]
pub fn maintenance_marker_path(db_path: &Path) -> PathBuf {
    suffixed(db_path, ".maintenance")
}

/// Returns the lock path for `db_path`.
#[must_use]
pub fn maintenance_lock_path(db_path: &Path) -> PathBuf {
    suffixed(db_path, ".maintenance.lock")
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Returns the current wall-clock time as a Unix timestamp.
#[must_use]
pub fn current_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |delta| delta.as_secs_f64())
}

/// Runs `maintenance` at most once per interval per database path.
///
/// Returns whether maintenance ran **and** the marker was refreshed. Every
/// failure path returns `false` rather than propagating, because v1 treats this
/// as opportunistic housekeeping. Callers that must distinguish a closed gate
/// from a contended or failed one use [`run_sqlite_maintenance_detailed`].
///
/// The sequence is: cheap due check, non-blocking `flock` (skip if another
/// process holds it), re-check under the lock, run, then write the marker
/// atomically.
pub fn run_sqlite_maintenance_if_due(
    db_path: &Path,
    interval_seconds: Option<f64>,
    now: Option<f64>,
    maintenance: impl FnOnce() -> Result<(), KernelError>,
) -> bool {
    matches!(
        run_sqlite_maintenance_detailed(db_path, interval_seconds, now, maintenance),
        MaintenanceOutcome::Ran
    )
}

/// Runs `maintenance` at most once per interval per database path, with the
/// gate's own failures surfaced.
///
/// The sequence matches [`run_sqlite_maintenance_if_due`]; only the reporting
/// differs. A gate that is closed reports [`MaintenanceOutcome::NotDue`]. A
/// lock held by another process reports [`MaintenanceOutcome::Contended`]. A
/// failure to open or lock the lock file, a failure of `maintenance` itself,
/// and a failure to write or rename the marker all report
/// [`MaintenanceOutcome::Failed`] - none of them advance the marker, so the
/// next attempt retries instead of waiting a full window.
pub fn run_sqlite_maintenance_detailed(
    db_path: &Path,
    interval_seconds: Option<f64>,
    now: Option<f64>,
    maintenance: impl FnOnce() -> Result<(), KernelError>,
) -> MaintenanceOutcome {
    let interval = interval_seconds.unwrap_or(DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS);
    let marker_path = maintenance_marker_path(db_path);
    let lock_path = maintenance_lock_path(db_path);
    let current_time = now.unwrap_or_else(current_epoch);

    if !maintenance_due(&marker_path, interval, current_time) {
        return MaintenanceOutcome::NotDue;
    }

    let lock = match try_acquire_lock_detailed(&lock_path) {
        Ok(lock) => lock,
        Err(LockError::Contended) => return MaintenanceOutcome::Contended,
        Err(LockError::Failed(error)) => {
            return MaintenanceOutcome::Failed(error.to_string());
        }
    };

    let outcome = if maintenance_due(&marker_path, interval, current_time) {
        match maintenance() {
            Ok(()) => match mark_complete(&marker_path, current_time) {
                Ok(()) => MaintenanceOutcome::Ran,
                Err(error) => MaintenanceOutcome::Failed(error.to_string()),
            },
            Err(error) => MaintenanceOutcome::Failed(error.to_string()),
        }
    } else {
        // Another holder completed the pass between the first due check and
        // taking the lock.
        MaintenanceOutcome::NotDue
    };

    let _ = flock(&lock, FlockOperation::Unlock);
    drop(lock);
    outcome
}

/// Returns whether the gate is open at `now`.
///
/// A non-positive interval always opens the gate, and a marker in the future is
/// treated as stale — both straight from v1.
fn maintenance_due(marker_path: &Path, interval_seconds: f64, now: f64) -> bool {
    if interval_seconds <= 0.0 {
        return true;
    }
    match read_last_maintenance(marker_path) {
        None => true,
        Some(last_run) if last_run > now => true,
        Some(last_run) => now - last_run >= interval_seconds,
    }
}

fn read_last_maintenance(marker_path: &Path) -> Option<f64> {
    fs::read_to_string(marker_path)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
}

/// Writes the marker atomically via a pid-suffixed temp file, as v1 does.
fn mark_complete(marker_path: &Path, now: f64) -> Result<(), KernelError> {
    let file_name = marker_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_path = marker_path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));

    fs::write(&tmp_path, format!("{now:.6}\n"))
        .map_err(|err| KernelError::io("write", &tmp_path, err))?;
    let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
    fs::rename(&tmp_path, marker_path).map_err(|err| KernelError::io("rename", &tmp_path, err))?;
    Ok(())
}

/// Why a non-blocking lock attempt could not be taken.
enum LockError {
    /// Another process holds the lock.
    Contended,
    /// The lock file could not be opened or locked.
    Failed(KernelError),
}

/// Takes the advisory lock without blocking, reporting why it could not.
///
/// The lock file stays on disk: `flock` state belongs to the open descriptor, and
/// unlinking lock files creates cross-process races. A held lock is
/// [`LockError::Contended`]; any open or `flock` failure other than
/// `EWOULDBLOCK` is [`LockError::Failed`].
fn try_acquire_lock_detailed(lock_path: &Path) -> Result<std::fs::File, LockError> {
    let flags = OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC;
    let fd = rustix::fs::open(lock_path, flags, Mode::RUSR | Mode::WUSR).map_err(|error| {
        LockError::Failed(KernelError::io(
            "open",
            lock_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        ))
    })?;
    let file = std::fs::File::from(fd);
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Err(LockError::Contended),
        Err(error) => Err(LockError::Failed(KernelError::io(
            "lock",
            lock_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        ))),
    }
}

/// Takes the advisory lock without blocking.
///
/// The lock file stays on disk: `flock` state belongs to the open descriptor, and
/// unlinking lock files creates cross-process races. Returns `None` when the
/// lock cannot be taken for any reason.
#[cfg(test)]
fn try_acquire_lock(lock_path: &Path) -> Option<std::fs::File> {
    try_acquire_lock_detailed(lock_path).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use tempfile::TempDir;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the signature must match the maintenance closure"
    )]
    fn always_ok() -> Result<(), KernelError> {
        Ok(())
    }

    #[test]
    fn marker_and_lock_names_match_v1() {
        let db = Path::new("/tmp/a/events.db");
        assert_eq!(
            maintenance_marker_path(db),
            Path::new("/tmp/a/events.db.maintenance")
        );
        assert_eq!(
            maintenance_lock_path(db),
            Path::new("/tmp/a/events.db.maintenance.lock")
        );
    }

    #[test]
    fn first_run_is_due_and_writes_the_marker() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        let ran = Cell::new(false);

        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            || {
                ran.set(true);
                Ok(())
            }
        ));
        assert!(ran.get());
        assert_eq!(
            read_last_maintenance(&maintenance_marker_path(&db)),
            Some(1000.0)
        );
        assert_eq!(
            fs::metadata(maintenance_marker_path(&db))
                .expect("marker")
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn a_second_run_inside_the_interval_is_skipped() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            always_ok
        ));

        let ran = Cell::new(false);
        assert!(!run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1500.0),
            || {
                ran.set(true);
                Ok(())
            }
        ));
        assert!(!ran.get(), "the gate must stay closed inside the interval");
    }

    #[test]
    fn the_gate_opens_again_exactly_at_the_interval() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        run_sqlite_maintenance_if_due(&db, Some(100.0), Some(1000.0), always_ok);

        assert!(!run_sqlite_maintenance_if_due(
            &db,
            Some(100.0),
            Some(1099.0),
            always_ok
        ));
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(100.0),
            Some(1100.0),
            always_ok
        ));
    }

    #[test]
    fn a_non_positive_interval_always_runs() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(0.0),
            Some(1.0),
            always_ok
        ));
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(0.0),
            Some(1.0),
            always_ok
        ));
    }

    #[test]
    fn a_marker_in_the_future_is_treated_as_stale() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        fs::write(maintenance_marker_path(&db), "99999999\n").expect("seed");
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            always_ok
        ));
    }

    #[test]
    fn a_corrupt_marker_is_treated_as_absent() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        fs::write(maintenance_marker_path(&db), "not a number").expect("seed");
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            always_ok
        ));
    }

    #[test]
    fn a_failing_maintenance_does_not_advance_the_gate() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        assert!(!run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            || Err(KernelError::Malformed("boom".to_owned()))
        ));
        assert!(!maintenance_marker_path(&db).exists());
        assert!(
            run_sqlite_maintenance_if_due(&db, Some(3600.0), Some(1000.0), always_ok),
            "the next attempt must still be due"
        );
    }

    #[test]
    fn a_held_lock_causes_a_skip() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        let held = try_acquire_lock(&maintenance_lock_path(&db)).expect("take lock");

        let ran = Cell::new(false);
        assert!(!run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            || {
                ran.set(true);
                Ok(())
            }
        ));
        assert!(!ran.get(), "a contended lock must skip, not block");

        drop(held);
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            always_ok
        ));
    }

    #[test]
    fn the_detailed_gate_reports_a_held_lock_as_contended() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        let held = try_acquire_lock(&maintenance_lock_path(&db)).expect("take lock");

        assert_eq!(
            run_sqlite_maintenance_detailed(&db, Some(3600.0), Some(1000.0), always_ok),
            MaintenanceOutcome::Contended
        );

        drop(held);
        assert_eq!(
            run_sqlite_maintenance_detailed(&db, Some(3600.0), Some(1000.0), always_ok),
            MaintenanceOutcome::Ran
        );
    }

    #[test]
    fn the_detailed_gate_reports_a_lock_open_failure() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        fs::create_dir(maintenance_lock_path(&db)).expect("block the lock path");

        assert!(matches!(
            run_sqlite_maintenance_detailed(&db, Some(3600.0), Some(1000.0), always_ok),
            MaintenanceOutcome::Failed(_)
        ));
    }

    #[test]
    fn the_detailed_gate_reports_a_marker_write_failure() {
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join("events.db");
        fs::create_dir(maintenance_marker_path(&db)).expect("block the marker path");

        assert!(matches!(
            run_sqlite_maintenance_detailed(&db, Some(3600.0), Some(1000.0), always_ok),
            MaintenanceOutcome::Failed(_)
        ));
        // The marker was not advanced, so the gate stays open for the next
        // attempt.
        fs::remove_dir(maintenance_marker_path(&db)).expect("unblock the marker path");
        assert!(run_sqlite_maintenance_if_due(
            &db,
            Some(3600.0),
            Some(1000.0),
            always_ok
        ));
    }

    #[test]
    fn the_default_interval_is_one_day() {
        assert!(
            (DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS - 24.0 * 60.0 * 60.0).abs() < f64::EPSILON
        );
    }
}
