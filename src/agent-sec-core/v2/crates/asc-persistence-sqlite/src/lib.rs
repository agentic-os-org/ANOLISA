//! `SQLite` bindings for the security-event and observability streams.
//!
//! This crate is the domain half of the split: [`asc_sqlite_kernel`] owns
//! connection lifecycle, schema convergence and the write ladder, while each
//! module here supplies the four things that actually differ per stream — the
//! table contract, the schema migrator, the repository and the fault policy.
//!
//! Migrated from v1 `security_events/{models,repositories}.py` and
//! `observability/{models,repositories}.py`, plus the per-stream branches of the
//! two `sqlite_writer.py` files.

#![forbid(unsafe_code)]

pub mod observability;
pub mod security_events;

#[cfg(test)]
mod tests {
    use asc_sqlite_kernel::{ColumnSpec, ExtraColumn, TableSpec, assert_send, is_valid_identifier};

    use crate::observability::OBSERVABILITY_TABLES;
    use crate::observability::policy::ObservabilityFaultPolicy;
    use crate::observability::repository::ObservabilityEventRepository;
    use crate::security_events::SECURITY_EVENTS_TABLES;
    use crate::security_events::migration::SecurityEventsMigrator;
    use crate::security_events::policy::{SecurityEventsFaultPolicy, StderrDropSink};
    use crate::security_events::repository::SecurityEventRepository;

    #[test]
    fn the_bound_types_are_send() {
        assert_send::<SecurityEventRepository>();
        assert_send::<ObservabilityEventRepository>();
        assert_send::<SecurityEventsFaultPolicy<StderrDropSink>>();
        assert_send::<ObservabilityFaultPolicy>();
        assert_send::<SecurityEventsMigrator>();
    }

    /// The two streams must never collide in one database file.
    #[test]
    fn the_two_streams_declare_different_tables() {
        assert_ne!(SECURITY_EVENTS_TABLES[0].name, OBSERVABILITY_TABLES[0].name);
    }

    /// Index names are global per database, so a collision would break
    /// convergence if the two streams ever shared a file.
    #[test]
    fn index_names_do_not_collide_across_streams() {
        let security: Vec<&str> = index_names(SECURITY_EVENTS_TABLES);
        for name in index_names(OBSERVABILITY_TABLES) {
            assert!(
                !security.contains(&name),
                "{name} is declared by both streams"
            );
        }
    }

    #[test]
    fn every_converged_column_passes_the_alter_table_allowlist() {
        for table in SECURITY_EVENTS_TABLES.iter().chain(OBSERVABILITY_TABLES) {
            for column in table.extra_columns {
                assert!(is_valid_identifier(column.name), "{}", column.name);
            }
        }
    }

    /// Guards the contract the kernel's `ALTER TABLE` allowlist enforces.
    #[test]
    fn a_column_name_with_a_digit_would_be_rejected() {
        let bad = ExtraColumn {
            name: "run_id2",
            definition: "TEXT",
        };
        assert!(
            !is_valid_identifier(bad.name),
            "digits are rejected by v1's identifier rule; a spec must not rely on them"
        );
    }

    #[test]
    fn declared_columns_are_unique_within_each_table() {
        for table in SECURITY_EVENTS_TABLES.iter().chain(OBSERVABILITY_TABLES) {
            let mut names: Vec<&str> = column_names(table);
            let total = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), total, "{} has a duplicate column", table.name);
        }
    }

    fn index_names(tables: &'static [TableSpec]) -> Vec<&'static str> {
        tables
            .iter()
            .flat_map(|table| table.indexes.iter().map(|index| index.name))
            .collect()
    }

    fn column_names(table: &'static TableSpec) -> Vec<&'static str> {
        table
            .columns
            .iter()
            .map(|column: &ColumnSpec| column.name)
            .collect()
    }
}
