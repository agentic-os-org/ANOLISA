//! System-daemon observability schema with trusted UID ownership and legacy upgrades.

use std::path::Path;

use asc_observability::{OBSERVABILITY_LOG_PREFIX, ObservabilityRecord};
use asc_sqlite_kernel::{ColumnSpec, ExtraColumn, IndexSpec, KernelError, SqliteStore, TableSpec};
use rusqlite::Connection;

use super::table::OBSERVABILITY_TABLES;

/// Adds trusted ownership to revision 1; historical rows keep an unknown owner.
pub const OWNED_OBSERVABILITY_VERSION: u32 = 2;

const OWNER_COLUMN: ColumnSpec = ColumnSpec {
    name: "uid",
    definition: "INTEGER CHECK(uid BETWEEN 0 AND 4294967295)",
};

const _: () = assert!(OBSERVABILITY_TABLES[0].columns.len() == 10);

/// Record columns plus a trusted storage-only owner; JSONL omits the owner.
pub const OWNED_OBSERVABILITY_TABLES: &[TableSpec] = &[TableSpec {
    name: "observability_events",
    strict: OBSERVABILITY_TABLES[0].strict,
    constraints: OBSERVABILITY_TABLES[0].constraints,
    columns: &[
        OBSERVABILITY_TABLES[0].columns[0],
        OBSERVABILITY_TABLES[0].columns[1],
        OBSERVABILITY_TABLES[0].columns[2],
        OBSERVABILITY_TABLES[0].columns[3],
        OBSERVABILITY_TABLES[0].columns[4],
        OBSERVABILITY_TABLES[0].columns[5],
        OBSERVABILITY_TABLES[0].columns[6],
        OBSERVABILITY_TABLES[0].columns[7],
        OBSERVABILITY_TABLES[0].columns[8],
        OBSERVABILITY_TABLES[0].columns[9],
        OWNER_COLUMN,
    ],
    indexes: &[
        OBSERVABILITY_TABLES[0].indexes[0],
        OBSERVABILITY_TABLES[0].indexes[1],
        OBSERVABILITY_TABLES[0].indexes[2],
        OBSERVABILITY_TABLES[0].indexes[3],
        IndexSpec {
            name: "idx_observability_owner_time",
            columns: &["uid", "observed_at_epoch", "id"],
        },
        IndexSpec {
            name: "idx_observability_owner_session_run_time",
            columns: &["uid", "session_id", "run_id", "observed_at_epoch", "id"],
        },
    ],
    // The kernel adds this nullable column, indexes and revision in one transaction.
    // TODO(sec-core): evaluate an explicit SQLite migration phase at daemon startup
    // instead of writer schema convergence, which warm_owned currently triggers at startup.
    extra_columns: &[ExtraColumn {
        name: OWNER_COLUMN.name,
        definition: OWNER_COLUMN.definition,
    }],
}];

/// Strict daemon writer: every new row carries its owner in the same INSERT.
#[derive(Debug)]
pub struct OwnedObservabilityWriter {
    store: SqliteStore,
}

impl OwnedObservabilityWriter {
    /// Configures the system schema and legacy upgrade without opening a database.
    ///
    /// # Errors
    /// Returns invalid-path errors from the `SQLite` kernel.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        Ok(Self {
            store: SqliteStore::new(
                path,
                false,
                OWNED_OBSERVABILITY_VERSION,
                OWNED_OBSERVABILITY_TABLES,
                None,
                OBSERVABILITY_LOG_PREFIX,
            )?,
        })
    }

    /// Initializes or validates the database before queries are admitted.
    ///
    /// # Errors
    /// Unavailable storage and unsupported schema versions fail explicitly.
    pub fn probe(&self) -> Result<(), KernelError> {
        self.store
            .with_connection(true, check_schema)?
            .ok_or(KernelError::Disabled)
    }

    /// Commits owner and record atomically, without changing the record's public shape.
    ///
    /// # Errors
    /// Propagates storage and serialization errors; no unowned row is inserted on failure.
    pub fn write(&self, record: &ObservabilityRecord, uid: u32) -> Result<(), KernelError> {
        let metrics = record
            .metrics()
            .to_json_string()
            .map_err(|_| KernelError::Malformed("invalid metrics".into()))?;
        let metadata = record
            .metadata()
            .to_json_string()
            .map_err(|_| KernelError::Malformed("invalid metadata".into()))?;
        self.store.with_connection(true, |conn| {
            check_schema(conn)?;
            let labels = record.metadata();
            conn.execute("INSERT INTO observability_events (hook,observed_at,observed_at_epoch,session_id,run_id,metrics_json,metadata_json,call_id,tool_call_id,uid) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", rusqlite::params![record.hook().as_str(), record.observed_at_iso(), record.observed_at_epoch(), labels.session_id, labels.run_id, metrics, metadata, labels.call_id, labels.tool_call_id, uid])?;
            Ok(())
        })?.ok_or(KernelError::Disabled)
    }

    /// Runs existing retention maintenance and releases the writer handle.
    pub fn close(&self) {
        use asc_sqlite_kernel::RecordRepository;
        let now = asc_sqlite_kernel::current_epoch();
        let _ = asc_sqlite_kernel::run_sqlite_maintenance_if_due(
            self.store.path(),
            None,
            Some(now),
            || {
                self.store
                    .with_connection(true, |conn| {
                        super::ObservabilityEventRepository.prune(
                            conn,
                            asc_observability::DEFAULT_OBSERVABILITY_RETENTION_DAYS,
                            now,
                        )?;
                        Ok(())
                    })?
                    .ok_or(KernelError::Disabled)
            },
        );
        self.store.close();
    }
}

fn check_schema(conn: &Connection) -> Result<(), KernelError> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != OWNED_OBSERVABILITY_VERSION {
        return Err(KernelError::Malformed(
            "unsupported observability schema".into(),
        ));
    }
    Ok(())
}
