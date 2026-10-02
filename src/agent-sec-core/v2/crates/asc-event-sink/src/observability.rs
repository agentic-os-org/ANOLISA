//! Dual write for observability records, with both paths surfacing failures.
//!
//! Migrated from v1 `observability/__init__.py::record_observability`.

use asc_observability::ObservabilityRecord;

use crate::error::SinkError;
use crate::singletons::{observability_sqlite_writer, observability_writer};

/// Appends `record` to the `JSONL` log and inserts it into the `SQLite` index.
///
/// Unlike [`crate::log_event`], **both** paths report failure. That asymmetry is
/// deliberate in v1: an observability ingestion failure is a data-loss event its
/// caller is expected to see, so the `SQLite` path uses `write_or_raise` and the
/// `JSONL` path raises too. The `JSONL` path runs first and short-circuits, again
/// matching v1's two plain statements.
///
/// # Errors
///
/// Returns [`SinkError`] when either path cannot be built or the write fails.
pub fn record_observability(record: &ObservabilityRecord) -> Result<(), SinkError> {
    observability_writer()?.write(record)?;
    observability_sqlite_writer()?.write_or_raise(record)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::singletons::{
        install_observability_sqlite_writer_for_test, install_observability_writer_for_test,
        reset_sinks_for_test,
    };
    use crate::test_support::{record, serial, temp_dir};
    use asc_event_log::ObservabilityWriter;
    use asc_persistence_sqlite::observability::{ObservabilityReader, ObservabilitySqliteWriter};
    use std::fs;

    #[test]
    fn both_paths_receive_the_record() {
        let _guard = serial();
        let dir = temp_dir();
        let log = dir.path().join("observability.jsonl");
        let db = dir.path().join("observability.db");

        install_observability_writer_for_test(ObservabilityWriter::new(&log));
        let sqlite = install_observability_sqlite_writer_for_test(
            ObservabilitySqliteWriter::new(&db).expect("writer"),
        );

        record_observability(&record()).expect("both paths");
        sqlite.close_at(1000.0);

        assert_eq!(fs::read_to_string(&log).expect("log").lines().count(), 1);
        assert_eq!(ObservabilityReader::new(&db).expect("reader").count(), 1);
        reset_sinks_for_test();
    }

    #[test]
    fn a_broken_jsonl_path_surfaces_and_skips_the_sqlite_insert() {
        let _guard = serial();
        let dir = temp_dir();
        let log = dir.path().join("observability.jsonl");
        fs::create_dir(&log).expect("occupy the log path");
        let db = dir.path().join("observability.db");

        install_observability_writer_for_test(ObservabilityWriter::new(&log));
        let sqlite = install_observability_sqlite_writer_for_test(
            ObservabilitySqliteWriter::new(&db).expect("writer"),
        );

        let error = record_observability(&record()).expect_err("the JSONL path must raise");
        assert!(matches!(error, SinkError::EventLog(_)));

        sqlite.close_at(1000.0);
        assert!(
            !db.exists(),
            "the first statement raises, so v1 never reaches the SQLite write"
        );
        reset_sinks_for_test();
    }

    #[test]
    fn a_broken_database_surfaces_after_the_jsonl_append() {
        let _guard = serial();
        let dir = temp_dir();
        let log = dir.path().join("observability.jsonl");
        let db = dir.path().join("observability.db");
        fs::create_dir(&db).expect("occupy the database path");

        install_observability_writer_for_test(ObservabilityWriter::new(&log));
        install_observability_sqlite_writer_for_test(
            ObservabilitySqliteWriter::new(&db).expect("writer"),
        );

        let error = record_observability(&record()).expect_err("the SQLite path must raise");
        assert!(matches!(error, SinkError::Kernel(_)));
        assert_eq!(
            fs::read_to_string(&log).expect("log").lines().count(),
            1,
            "the JSONL append already happened before the failure"
        );
        reset_sinks_for_test();
    }
}
