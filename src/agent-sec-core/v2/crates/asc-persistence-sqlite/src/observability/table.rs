//! The `observability_events` table contract.
//!
//! Transcribed from v1 `observability/models.py`. The `extra_columns` list is
//! empty on purpose: this stream has only ever had revision 1, so there is
//! nothing to converge.
//!
//! The v2 **system** store adds one convergent column of its own through
//! [`SYSTEM_OBSERVABILITY_TABLES`]: the verified `owner` the state migrator
//! stamps on every imported row (#6605). The v1-shaped contract above stays
//! byte-identical because a v1 process and a v2 per-user writer must keep
//! converging shared databases to the same shape.

use asc_sqlite_kernel::{ColumnSpec, ExtraColumn, IndexSpec, TableSpec};

/// The single table this stream writes.
pub const OBSERVABILITY_TABLES: &[TableSpec] = &[TableSpec {
    name: "observability_events",
    columns: COLUMNS,
    indexes: INDEXES,
    extra_columns: &[],
}];

/// Schema revision of the v2 system observability store.
///
/// Revision 2 is the system store's owner-aware shape: revision 1 (the only
/// revision v1 ever wrote) plus the `owner` column. The number must stay above
/// [`asc_observability::OBSERVABILITY_SQLITE_SCHEMA_VERSION`] so that opening a
/// v1-shaped database through the system spec forces full convergence instead
/// of taking the kernel's version-match fast path.
pub const SYSTEM_OBSERVABILITY_SQLITE_SCHEMA_VERSION: u32 = 2;

/// The owner column the system store converges onto the v1 shape.
const OWNER_COLUMN: ExtraColumn = ExtraColumn {
    name: "owner",
    definition: "INTEGER",
};

/// The single table of the v2 system observability store.
///
/// The v2 system store is a different database from the v1 per-user streams:
/// it centralizes many users' history, so every row carries the verified
/// `owner` the migration (or, later, the daemon) stamped (#6605). Columns and
/// the v1 indexes are the shared wire contract; the `owner` column converges
/// onto any older (v1-shaped) file, and one extra index serves the
/// owner-scoped reads the query slice will open once `QueryScope` lands
/// (#6608).
pub const SYSTEM_OBSERVABILITY_TABLES: &[TableSpec] = &[TableSpec {
    name: "observability_events",
    columns: COLUMNS,
    indexes: SYSTEM_INDEXES,
    extra_columns: &[OWNER_COLUMN],
}];

/// The system store's indexes: v1's four plus the owner-scoped read path.
const SYSTEM_INDEXES: &[IndexSpec] = &[
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
    IndexSpec {
        name: "idx_observability_owner_observed_at_epoch",
        columns: &["owner", "observed_at_epoch"],
    },
];

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

    #[test]
    fn the_system_spec_keeps_the_v1_columns_and_adds_only_owner() {
        let system = &SYSTEM_OBSERVABILITY_TABLES[0];
        assert_eq!(system.name, "observability_events");
        assert_eq!(
            system.columns, COLUMNS,
            "the system store must create the v1 wire shape"
        );
        let extras: Vec<&str> = system.extra_columns.iter().map(|c| c.name).collect();
        assert_eq!(extras, vec!["owner"], "owner is the only convergent column");
        assert_eq!(
            system.extra_columns[0].definition, "INTEGER",
            "owner stores the verified uid"
        );
    }

    #[test]
    fn the_system_spec_extends_the_v1_index_set() {
        let v1: Vec<&str> = INDEXES.iter().map(|index| index.name).collect();
        let system: Vec<&str> = SYSTEM_INDEXES.iter().map(|index| index.name).collect();
        assert!(system.len() == v1.len() + 1);
        assert!(
            system[..v1.len()] == v1[..],
            "the v1 indexes keep their names and order"
        );
        assert_eq!(
            system.last().copied(),
            Some("idx_observability_owner_observed_at_epoch")
        );
    }

    #[test]
    fn the_system_schema_version_is_newer_than_the_v1_stream_revision() {
        // Both revisions are pinned: the system store must stay above the
        // stream revision so a v1-shaped database converges instead of taking
        // the kernel's version-match fast path.
        assert_eq!(
            SYSTEM_OBSERVABILITY_SQLITE_SCHEMA_VERSION,
            asc_observability::OBSERVABILITY_SQLITE_SCHEMA_VERSION + 1
        );
    }
}
