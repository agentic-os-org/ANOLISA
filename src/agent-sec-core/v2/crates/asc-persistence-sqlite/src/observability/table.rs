//! The `observability_events` table contract.
//!
//! Revision 2 adds trusted UID ownership. Every new write supplies a concrete
//! UID, but the column stays nullable so revision-1 stores converge by
//! `ALTER TABLE ADD COLUMN` and keep their pre-ownership rows as unknown (`NULL`).

use asc_sqlite_kernel::{ColumnSpec, ExtraColumn, IndexSpec, TableSpec};

/// Trusted-owner column definition shared by the fresh-create and legacy paths.
///
/// Nullable on purpose: revision-1 databases gain this column through
/// `ALTER TABLE ADD COLUMN`, which cannot backfill an owner for historical rows.
/// The text must be byte-identical in [`COLUMNS`] and [`EXTRA_COLUMNS`] because
/// the differential harness compares `PRAGMA table_info` across both paths.
const OWNER_COLUMN: &str = "INTEGER CHECK(uid BETWEEN 0 AND 4294967295)";

/// The single table this stream writes.
pub const OBSERVABILITY_TABLES: &[TableSpec] = &[TableSpec {
    name: "observability_events",
    strict: false,
    constraints: &[],
    columns: COLUMNS,
    indexes: INDEXES,
    extra_columns: EXTRA_COLUMNS,
}];

/// Revision-1 databases converge by adding the nullable owner column.
const EXTRA_COLUMNS: &[ExtraColumn] = &[ExtraColumn {
    name: "uid",
    definition: OWNER_COLUMN,
}];

/// V1 business columns followed by nullable daemon UID ownership.
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
    ColumnSpec {
        name: "uid",
        definition: OWNER_COLUMN,
    },
];

/// Business lookup indexes followed by owner-scoped query indexes.
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
    IndexSpec {
        name: "idx_observability_owner_time",
        columns: &["uid", "observed_at_epoch", "id"],
    },
    IndexSpec {
        name: "idx_observability_owner_session_run_time",
        columns: &["uid", "session_id", "run_id", "observed_at_epoch", "id"],
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
    fn columns_include_nullable_uid() {
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
                "uid",
            ]
        );
        let uid = COLUMNS
            .iter()
            .find(|column| column.name == "uid")
            .expect("uid");
        assert!(
            !uid.definition.contains("NOT NULL"),
            "owner column must stay nullable for legacy convergence"
        );
    }

    #[test]
    fn indexes_include_owner_scope() {
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
                "idx_observability_owner_time(uid,observed_at_epoch,id)",
                "idx_observability_owner_session_run_time(uid,session_id,run_id,observed_at_epoch,id)",
            ]
        );
    }

    #[test]
    fn legacy_convergence_adds_owner_column() {
        let extra: Vec<&str> = OBSERVABILITY_TABLES[0]
            .extra_columns
            .iter()
            .map(|column| column.name)
            .collect();
        assert_eq!(extra, vec!["uid"]);
        // Both paths must render the identical column definition so a fresh
        // create and a legacy ALTER produce the same `PRAGMA table_info`.
        let created = COLUMNS
            .iter()
            .find(|column| column.name == "uid")
            .expect("uid");
        let converged = &OBSERVABILITY_TABLES[0].extra_columns[0];
        assert_eq!(created.definition, converged.definition);
    }
}
