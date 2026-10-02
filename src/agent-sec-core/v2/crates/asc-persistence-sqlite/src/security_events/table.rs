//! The `security_events` table contract.
//!
//! Transcribed from v1 `security_events/models.py`. Column order, index names
//! and index column order are all wire contract: a v1 process and a v2 process
//! must converge the same database to the same shape.

use asc_sqlite_kernel::{ColumnSpec, ExtraColumn, IndexSpec, TableSpec};

/// The single table both v1 and v2 write for this stream.
pub const SECURITY_EVENTS_TABLES: &[TableSpec] = &[TableSpec {
    name: "security_events",
    columns: COLUMNS,
    indexes: INDEXES,
    extra_columns: EXTRA_COLUMNS,
}];

/// Columns in v1 `SecurityEventRecord` declaration order.
///
/// The declared types are v1's rendered `SQLAlchemy` types verbatim, including
/// `FLOAT` (not `REAL`): both carry `SQLite`'s `REAL` affinity, but the
/// differential harness compares `PRAGMA table_info` textually, so the spelling
/// is part of the contract. The two `server_default` values are reproduced the
/// same way so a row inserted by an older writer that omits them lands with the
/// same content.
///
/// `event_id` carries an explicit `NOT NULL`, which `SQLAlchemy` adds to every
/// primary key. Without it `SQLite` would accept a `NULL` in a `TEXT PRIMARY
/// KEY` — v1 rejects that, so omitting it would widen what v2 accepts.
const COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "event_id",
        definition: "TEXT NOT NULL PRIMARY KEY",
    },
    ColumnSpec {
        name: "event_type",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "category",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "result",
        definition: "TEXT NOT NULL DEFAULT 'succeeded'",
    },
    ColumnSpec {
        name: "timestamp",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "timestamp_epoch",
        definition: "FLOAT NOT NULL",
    },
    ColumnSpec {
        name: "trace_id",
        definition: "TEXT NOT NULL DEFAULT ''",
    },
    ColumnSpec {
        name: "pid",
        definition: "INTEGER NOT NULL",
    },
    ColumnSpec {
        name: "uid",
        definition: "INTEGER NOT NULL",
    },
    ColumnSpec {
        name: "session_id",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "run_id",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "call_id",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "tool_call_id",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "verdict",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "details",
        definition: "TEXT NOT NULL",
    },
];

/// Indexes in v1 `__table_args__` order.
const INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_event_type",
        columns: &["event_type"],
    },
    IndexSpec {
        name: "idx_category_epoch",
        columns: &["category", "timestamp_epoch"],
    },
    IndexSpec {
        name: "idx_trace_id",
        columns: &["trace_id"],
    },
    IndexSpec {
        name: "idx_timestamp_epoch",
        columns: &["timestamp_epoch"],
    },
    IndexSpec {
        name: "idx_verdict_timestamp_epoch",
        columns: &["verdict", "timestamp_epoch"],
    },
    IndexSpec {
        name: "idx_session_id_timestamp_epoch",
        columns: &["session_id", "timestamp_epoch"],
    },
    IndexSpec {
        name: "idx_run_id_timestamp_epoch",
        columns: &["run_id", "timestamp_epoch"],
    },
    IndexSpec {
        name: "idx_session_run_timestamp_epoch",
        columns: &["session_id", "run_id", "timestamp_epoch"],
    },
];

/// v1 `__schema_columns__`.
///
/// `verdict` appears here *and* in the rev-3 migrator on purpose: the migrator
/// covers 2 → 3, while this list is what lifts a rev-1 database that never had
/// the three correlation columns.
const EXTRA_COLUMNS: &[ExtraColumn] = &[
    ExtraColumn {
        name: "run_id",
        definition: "TEXT",
    },
    ExtraColumn {
        name: "call_id",
        definition: "TEXT",
    },
    ExtraColumn {
        name: "tool_call_id",
        definition: "TEXT",
    },
    ExtraColumn {
        name: "verdict",
        definition: "TEXT",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use asc_sqlite_kernel::is_valid_identifier;

    #[test]
    fn there_is_exactly_one_table() {
        assert_eq!(SECURITY_EVENTS_TABLES.len(), 1);
        assert_eq!(SECURITY_EVENTS_TABLES[0].name, "security_events");
    }

    #[test]
    fn column_order_matches_v1() {
        let names: Vec<&str> = COLUMNS.iter().map(|column| column.name).collect();
        assert_eq!(
            names,
            vec![
                "event_id",
                "event_type",
                "category",
                "result",
                "timestamp",
                "timestamp_epoch",
                "trace_id",
                "pid",
                "uid",
                "session_id",
                "run_id",
                "call_id",
                "tool_call_id",
                "verdict",
                "details",
            ]
        );
    }

    #[test]
    fn index_names_and_column_order_match_v1() {
        let rendered: Vec<String> = INDEXES
            .iter()
            .map(|index| format!("{}({})", index.name, index.columns.join(",")))
            .collect();
        assert_eq!(
            rendered,
            vec![
                "idx_event_type(event_type)",
                "idx_category_epoch(category,timestamp_epoch)",
                "idx_trace_id(trace_id)",
                "idx_timestamp_epoch(timestamp_epoch)",
                "idx_verdict_timestamp_epoch(verdict,timestamp_epoch)",
                "idx_session_id_timestamp_epoch(session_id,timestamp_epoch)",
                "idx_run_id_timestamp_epoch(run_id,timestamp_epoch)",
                "idx_session_run_timestamp_epoch(session_id,run_id,timestamp_epoch)",
            ]
        );
    }

    #[test]
    fn every_extra_column_passes_the_kernel_identifier_rule() {
        for column in EXTRA_COLUMNS {
            assert!(
                is_valid_identifier(column.name),
                "{} would be rejected by the ALTER TABLE allowlist",
                column.name
            );
        }
    }

    #[test]
    fn extra_columns_are_a_subset_of_the_declared_columns() {
        for extra in EXTRA_COLUMNS {
            assert!(
                COLUMNS.iter().any(|column| column.name == extra.name),
                "{} is converged but never declared",
                extra.name
            );
        }
    }

    #[test]
    fn ddl_renders_the_v1_defaults() {
        let sql = SECURITY_EVENTS_TABLES[0].create_table_sql();
        assert!(sql.contains("result TEXT NOT NULL DEFAULT 'succeeded'"));
        assert!(sql.contains("trace_id TEXT NOT NULL DEFAULT ''"));
        assert!(sql.contains("timestamp_epoch FLOAT NOT NULL"));
    }
}
