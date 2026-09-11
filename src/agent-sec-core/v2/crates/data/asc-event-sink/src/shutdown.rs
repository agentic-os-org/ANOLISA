//! Host-driven shutdown for the `SQLite` sinks.
//!
//! Migrated from the `atexit.register(_sqlite_writer.close)` calls in both v1
//! `__init__.py` files.
//!
//! # Known semantic difference from v1
//!
//! Rust has no `atexit` equivalent that is reachable here: registering one needs
//! `libc::atexit` and therefore `unsafe`, which this workspace forbids. So v1's
//! "maintenance happens automatically when the process exits" cannot be
//! reproduced. [`shutdown_sinks`] is a **contract the host must call** — wire it
//! into the graceful-exit path. A process that never calls it simply never runs
//! the gated maintenance pass; nothing is lost or corrupted, the retention prune
//! and `WAL` checkpoint are just deferred to whichever process closes next.

use asc_sqlite_kernel::current_epoch;

use crate::singletons::{initialized_observability_sqlite_writer, initialized_sqlite_writer};

/// Closes every `SQLite` sink that was actually built.
///
/// Idempotent, and a no-op when no sink was ever accessed: an uninitialized slot
/// is left alone rather than built, so shutdown never creates a database.
pub fn shutdown_sinks() {
    shutdown_sinks_at(current_epoch());
}

/// Closes the sinks as of `now`.
///
/// The maintenance pass is time-gated, so an injected `now` is what makes the
/// gating observable in a test.
pub fn shutdown_sinks_at(now: f64) {
    if let Some(sink) = initialized_sqlite_writer() {
        sink.close_at(now);
    }
    if let Some(sink) = initialized_observability_sqlite_writer() {
        sink.close_at(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::singletons::{
        install_observability_sqlite_writer_for_test, install_sqlite_writer_for_test,
        reset_sinks_for_test,
    };
    use crate::test_support::{event, record, serial, temp_dir};
    use asc_persistence_sqlite::observability::ObservabilitySqliteWriter;
    use asc_persistence_sqlite::security_events::SqliteEventWriter;

    #[test]
    fn shutting_down_untouched_sinks_creates_nothing() {
        let _guard = serial();
        reset_sinks_for_test();

        shutdown_sinks_at(1000.0);
        shutdown_sinks_at(1000.0);
    }

    #[test]
    fn shutdown_runs_the_maintenance_pass_once_per_window() {
        let _guard = serial();
        let dir = temp_dir();
        let db = dir.path().join("security-events.db");
        let marker = dir.path().join("security-events.db.maintenance");

        let sink = install_sqlite_writer_for_test(SqliteEventWriter::new(&db).expect("writer"));
        sink.write(&event("e-1"));

        shutdown_sinks_at(1000.0);
        assert!(marker.exists(), "the first close must run maintenance");
        let first = std::fs::read_to_string(&marker).expect("marker");

        // Still inside the same window: the gate must keep it closed.
        shutdown_sinks_at(1001.0);
        assert_eq!(
            std::fs::read_to_string(&marker).expect("marker"),
            first,
            "a second close inside the interval must not re-run maintenance"
        );
        reset_sinks_for_test();
    }

    #[test]
    fn no_shutdown_means_no_maintenance() {
        let _guard = serial();
        let dir = temp_dir();
        let db = dir.path().join("security-events.db");
        let marker = dir.path().join("security-events.db.maintenance");

        let sink = install_sqlite_writer_for_test(SqliteEventWriter::new(&db).expect("writer"));
        sink.write(&event("e-1"));
        // Deliberately no shutdown call: this is the documented v1 divergence.
        reset_sinks_for_test();

        assert!(db.exists(), "the write itself still landed");
        assert!(
            !marker.exists(),
            "without an atexit hook, maintenance only happens when the host asks"
        );
    }

    #[test]
    fn both_streams_are_closed() {
        let _guard = serial();
        let dir = temp_dir();
        let security = dir.path().join("security-events.db");
        let observability = dir.path().join("observability.db");

        let security_sink =
            install_sqlite_writer_for_test(SqliteEventWriter::new(&security).expect("writer"));
        let observability_sink = install_observability_sqlite_writer_for_test(
            ObservabilitySqliteWriter::new(&observability).expect("writer"),
        );
        security_sink.write(&event("e-1"));
        observability_sink.write(&record());

        shutdown_sinks_at(1000.0);

        assert!(
            dir.path().join("security-events.db.maintenance").exists(),
            "the security stream was closed"
        );
        assert!(
            dir.path().join("observability.db.maintenance").exists(),
            "the observability stream was closed"
        );
        reset_sinks_for_test();
    }
}
