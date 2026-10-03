//! Fire-and-forget dual write for security events.
//!
//! Migrated from v1 `security_events/__init__.py::log_event`.

use asc_security_events::SecurityEvent;

use crate::singletons::{sqlite_writer, writer};

/// Appends `event` to the `JSONL` log and inserts it into `SQLite`.
///
/// The two paths are independent on purpose: v1 wraps each in its own
/// `try/except`, so a broken `JSONL` directory must not stop the `SQLite` insert
/// and vice versa. That is why this is two statements rather than a `?` chain.
/// Nothing is ever reported to the caller — a hook must not fail because
/// bookkeeping did.
pub fn log_event(event: &SecurityEvent) {
    if let Ok(sink) = writer() {
        sink.write(event);
    }
    if let Ok(sink) = sqlite_writer() {
        sink.write(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::singletons::{
        install_sqlite_writer_for_test, install_writer_for_test, reset_sinks_for_test,
    };
    use crate::test_support::{event, serial, temp_dir};
    use asc_event_log::SecurityEventWriter;
    use asc_persistence_sqlite::security_events::{
        EventFilters, SqliteEventReader, SqliteEventWriter,
    };
    use std::fs;
    use std::path::Path;

    fn jsonl_lines(path: &Path) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn sqlite_ids(path: &Path) -> Vec<String> {
        let reader = SqliteEventReader::new(path).expect("reader");
        reader
            .query_default_page(&EventFilters::default())
            .into_iter()
            .map(|event| event.event_id)
            .collect()
    }

    #[test]
    fn both_paths_receive_the_event() {
        let _guard = serial();
        let dir = temp_dir();
        let log = dir.path().join("security-events.jsonl");
        let db = dir.path().join("security-events.db");

        install_writer_for_test(SecurityEventWriter::new(&log));
        let sqlite = install_sqlite_writer_for_test(SqliteEventWriter::new(&db).expect("writer"));

        log_event(&event("e-1"));
        sqlite.close_at(1000.0);

        assert_eq!(jsonl_lines(&log).len(), 1);
        assert_eq!(sqlite_ids(&db), vec!["e-1".to_owned()]);
        reset_sinks_for_test();
    }

    #[test]
    fn a_broken_jsonl_path_does_not_stop_the_sqlite_insert() {
        let _guard = serial();
        let dir = temp_dir();
        // A directory in place of the log file makes every append fail.
        let log = dir.path().join("security-events.jsonl");
        fs::create_dir(&log).expect("occupy the log path");
        let db = dir.path().join("security-events.db");

        install_writer_for_test(SecurityEventWriter::new(&log));
        let sqlite = install_sqlite_writer_for_test(SqliteEventWriter::new(&db).expect("writer"));

        log_event(&event("e-2"));
        sqlite.close_at(1000.0);

        assert_eq!(
            sqlite_ids(&db),
            vec!["e-2".to_owned()],
            "v1's two independent try/except blocks must not be collapsed"
        );
        reset_sinks_for_test();
    }

    #[test]
    fn a_broken_database_does_not_stop_the_jsonl_append() {
        let _guard = serial();
        let dir = temp_dir();
        let log = dir.path().join("security-events.jsonl");
        let db = dir.path().join("security-events.db");
        fs::create_dir(&db).expect("occupy the database path");

        install_writer_for_test(SecurityEventWriter::new(&log));
        install_sqlite_writer_for_test(SqliteEventWriter::new(&db).expect("writer"));

        log_event(&event("e-3"));

        assert_eq!(jsonl_lines(&log).len(), 1);
        reset_sinks_for_test();
    }

    #[test]
    fn both_paths_broken_is_still_silent() {
        let _guard = serial();
        let dir = temp_dir();
        let log = dir.path().join("security-events.jsonl");
        let db = dir.path().join("security-events.db");
        fs::create_dir(&log).expect("occupy the log path");
        fs::create_dir(&db).expect("occupy the database path");

        install_writer_for_test(SecurityEventWriter::new(&log));
        install_sqlite_writer_for_test(SqliteEventWriter::new(&db).expect("writer"));

        // The contract is the absence of a panic and of a return value.
        log_event(&event("e-4"));
        reset_sinks_for_test();
    }
}
