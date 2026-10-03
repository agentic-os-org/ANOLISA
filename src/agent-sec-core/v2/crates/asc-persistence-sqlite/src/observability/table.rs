//! The `observability_events` table contract.
//!
//! Transcribed from v1 `observability/models.py`. The `extra_columns` list is
//! empty on purpose: this stream has only ever had revision 1, so there is
//! nothing to converge.

use asc_sqlite_kernel::{ColumnSpec, IndexSpec, TableSpec};

/// The single table this stream writes.
pub const OBSERVABILITY_TABLES: &[TableSpec] = &[TableSpec {
    name: "observability_events",
    columns: COLUMNS,
    indexes: INDEXES,
    extra_columns: &[],
}];

/// Columns in v1 `ObservabilityEventRecord` declaration order.
///
/// The primary key is a plain `INTEGER PRIMARY KEY`, i.e. a rowid alias, which is
/// why `list_runs` can break ties on `id` when two records share an
/// `observed_at_epoch`.
///
/// Two spellings here are dictated by v1 rather than by preference:
///
/// - **No `AUTOINCREMENT`.** v1 declares `autoincrement=True`, but `SQLAlchemy`
///   only emits the `SQLite` keyword when a table asks for `sqlite_autoincrement`,
///   which v1 does not. Emitting it would create a `sqlite_sequence` table that
///   v1 databases do not have, and would change id reuse after a retention prune.
/// - **`FLOAT`, not `REAL`.** Both carry `REAL` affinity, but the differential
///   harness compares `PRAGMA table_info` textually.
///
/// The explicit `NOT NULL` matches the one `SQLAlchemy` adds to every primary key.
const COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "id",
        definition: "INTEGER NOT NULL PRIMARY KEY",
    },
    ColumnSpec {
        name: "hook",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "observed_at",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "observed_at_epoch",
        definition: "FLOAT NOT NULL",
    },
    ColumnSpec {
        name: "session_id",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "run_id",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "metrics_json",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "metadata_json",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "call_id",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "tool_call_id",
        definition: "TEXT",
    },
];

/// Indexes in v1 `__table_args__` order.
const INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_observability_observed_at_epoch",
        columns: &["observed_at_epoch"],
    },
    IndexSpec {
        name: "idx_observability_hook_observed_at_epoch",
        columns: &["hook", "observed_at_epoch"],
    },
    IndexSpec {
        name: "idx_observability_session_observed_at_epoch",
        columns: &["session_id", "observed_at_epoch"],
    },
    IndexSpec {
        name: "idx_observability_session_run_observed_at_epoch",
        columns: &["session_id", "run_id", "observed_at_epoch"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_exactly_one_table() {
        assert_eq!(OBSERVABILITY_TABLES.len(), 1);
        assert_eq!(OBSERVABILITY_TABLES[0].name, "observability_events");
    }

    #[test]
    fn column_order_matches_v1() {
        let names: Vec<&str> = COLUMNS.iter().map(|column| column.name).collect();
        assert_eq!(
            names,
            vec![
                "id",
                "hook",
                "observed_at",
                "observed_at_epoch",
                "session_id",
                "run_id",
                "metrics_json",
                "metadata_json",
                "call_id",
                "tool_call_id",
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
                "idx_observability_observed_at_epoch(observed_at_epoch)",
                "idx_observability_hook_observed_at_epoch(hook,observed_at_epoch)",
                "idx_observability_session_observed_at_epoch(session_id,observed_at_epoch)",
                "idx_observability_session_run_observed_at_epoch(session_id,run_id,observed_at_epoch)",
            ]
        );
    }

    #[test]
    fn there_are_no_convergent_columns() {
        assert!(
            OBSERVABILITY_TABLES[0].extra_columns.is_empty(),
            "this stream has only ever had revision 1"
        );
    }
}
