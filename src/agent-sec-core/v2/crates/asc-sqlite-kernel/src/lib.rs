//! Domain-neutral `SQLite` store, schema convergence and write pipeline.
//!
//! This crate knows nothing about security events or observability. That is
//! enforced two ways: no domain noun appears in the source, and a guard test
//! asserts `Cargo.toml` carries no dependency on either contract crate. In v1
//! the equivalent code lives inside the `security_events` package, which is why
//! `observability/sqlite_writer.py` has to reach across packages for three
//! underscore-private functions. Here everything the domain layer needs is
//! explicitly `pub`.
//!
//! Migrated from v1 `security_events/{orm_store,sqlite_maintenance}.py` plus the
//! parts of the two `sqlite_writer.py` / `sqlite_reader.py` pairs that were
//! duplicated verbatim.

#![forbid(unsafe_code)]

pub mod connection;
pub mod error;
pub mod fault;
pub mod maintenance;
pub mod migration;
pub mod path;
pub mod query;
pub mod repository;
pub mod schema;
pub mod sink;
pub mod store;

pub use connection::open_connection;
pub use error::{KernelError, SCHEMA_ERROR_MARKERS, is_busy, is_corruption, is_schema};
pub use fault::{Failure, Fault, FaultPolicy, Outcome, Phase, WriteFault};
pub use maintenance::{
    DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS, current_epoch, run_sqlite_maintenance_if_due,
};
pub use migration::SchemaMigrator;
pub use path::{normalize_sqlite_path, sqlite_database_files};
pub use query::ReadOnlySource;
pub use repository::RecordRepository;
pub use schema::{
    ColumnSpec, ExtraColumn, IndexSpec, TableSpec, ensure_schema, ensure_schema_if_needed,
    is_valid_identifier, warn_readonly_schema_readiness,
};
pub use sink::SqliteSink;
pub use store::SqliteStore;

/// Compile-time assertion that `T` is `Send`.
///
/// Used to pin the `Send` requirement on the three long-lived types. The daemon
/// will need to move synchronous `SQLite` work into `spawn_blocking`, and a
/// non-`Send` field hidden inside any of them would only surface at that point,
/// forcing a rewrite.
pub const fn assert_send<T: Send>() {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const WIDGETS: &[TableSpec] = &[TableSpec {
        name: "widgets",
        columns: &[ColumnSpec {
            name: "id",
            definition: "TEXT PRIMARY KEY",
        }],
        indexes: &[],
        extra_columns: &[],
    }];

    struct NoopRepository;

    impl RecordRepository for NoopRepository {
        type Record = ();

        fn tables(&self) -> &'static [TableSpec] {
            WIDGETS
        }

        fn insert_or_raise(
            &self,
            _conn: &rusqlite::Connection,
            _record: &(),
        ) -> Result<bool, KernelError> {
            Ok(true)
        }

        fn prune(
            &self,
            _conn: &rusqlite::Connection,
            _max_age_days: u32,
            _now: f64,
        ) -> Result<usize, KernelError> {
            Ok(0)
        }
    }

    struct NoopPolicy;

    impl FaultPolicy for NoopPolicy {
        type Record = ();

        fn on_fault(&self, _fault: &Fault<'_>, _record: &()) -> Outcome {
            Outcome::swallow()
        }
    }

    /// The three long-lived kernel types must stay `Send`.
    #[test]
    fn long_lived_types_are_send() {
        assert_send::<SqliteStore>();
        assert_send::<SqliteSink<NoopRepository, NoopPolicy>>();
        assert_send::<ReadOnlySource<NoopRepository>>();
        assert_send::<Arc<SqliteStore>>();
    }

    /// Guards the layering: the kernel must not learn about either domain.
    ///
    /// A `Cargo.toml` check rather than a code check, because the failure mode is
    /// someone adding the dependency later for convenience.
    #[test]
    fn cargo_manifest_has_no_domain_dependency() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in ["asc-security-events", "asc-observability", "asc-event-log"] {
            assert!(
                !manifest.contains(forbidden),
                "asc-sqlite-kernel must stay domain neutral, found {forbidden}"
            );
        }
    }

    /// Guards the layering from the other side: no domain noun in the source.
    #[test]
    fn source_files_carry_no_domain_nouns() {
        let sources = [
            include_str!("connection.rs"),
            include_str!("fault.rs"),
            include_str!("path.rs"),
            include_str!("query.rs"),
            include_str!("repository.rs"),
            include_str!("schema.rs"),
            include_str!("sink.rs"),
            include_str!("store.rs"),
        ];
        // Only production identifiers are checked. Prose in doc comments
        // legitimately cites the v1 modules these were migrated from, and test
        // fixtures legitimately use v1 table names to pin warning text.
        for source in sources {
            let production = source.split("#[cfg(test)]").next().unwrap_or(source);
            for line in production.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                for forbidden in ["security_events", "observability", "SecurityEvent"] {
                    assert!(
                        !line.contains(forbidden),
                        "domain noun {forbidden} leaked into kernel code: {line}"
                    );
                }
            }
        }
    }
}
