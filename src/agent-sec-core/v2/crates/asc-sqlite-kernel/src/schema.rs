//! Declarative table specs plus v1's two-phase schema convergence.
//!
//! v1 derives DDL from `SQLAlchemy` metadata; v2 takes an explicit
//! [`TableSpec`]. There is deliberately **no** process-global default table
//! registry: v1's `register_orm_models` made the observability store report
//! `missing_tables=['security_events']`, and passing the spec per store removes
//! that failure mode at the type level.

use rusqlite::Connection;

use crate::error::KernelError;
use crate::migration::SchemaMigrator;

/// One column of a table, rendered in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSpec {
    /// Column name.
    pub name: &'static str,
    /// Full type and constraint text, e.g. `TEXT NOT NULL DEFAULT 'succeeded'`.
    pub definition: &'static str,
}

/// One index, created with `IF NOT EXISTS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexSpec {
    /// Index name; must match v1 byte for byte.
    pub name: &'static str,
    /// Indexed columns, in order.
    pub columns: &'static [&'static str],
}

/// A column that older databases may lack and that convergence adds.
///
/// This is v1's `__schema_columns__`. The column name is interpolated into
/// `ALTER TABLE` text, so it is validated against
/// [`is_valid_identifier`] first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtraColumn {
    /// Column name.
    pub name: &'static str,
    /// Type text appended after the name.
    pub definition: &'static str,
}

/// A complete table contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSpec {
    /// Table name.
    pub name: &'static str,
    /// Columns in `CREATE TABLE` order.
    pub columns: &'static [ColumnSpec],
    /// Indexes to converge.
    pub indexes: &'static [IndexSpec],
    /// Columns added by `ALTER TABLE` on older databases.
    pub extra_columns: &'static [ExtraColumn],
}

impl TableSpec {
    /// Renders the `CREATE TABLE IF NOT EXISTS` statement.
    #[must_use]
    pub fn create_table_sql(&self) -> String {
        let body = self
            .columns
            .iter()
            .map(|column| format!("{} {}", column.name, column.definition))
            .collect::<Vec<_>>()
            .join(", ");
        format!("CREATE TABLE IF NOT EXISTS {} ({body})", self.name)
    }

    /// Renders the `CREATE INDEX IF NOT EXISTS` statement for `index`.
    #[must_use]
    pub fn create_index_sql(&self, index: &IndexSpec) -> String {
        format!(
            "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
            index.name,
            self.name,
            index.columns.join(", ")
        )
    }
}

/// Returns whether `name` passes v1 `_IDENTIFIER_RE`, i.e. `^[a-z_]+$`.
///
/// Lowercase letters and underscores only — **digits are rejected**, so a column
/// named `run_id2` does not pass. This is intentional in v1 and preserved here.
#[must_use]
pub fn is_valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

/// Emits v1's "schema is newer than this binary" warning.
fn warn_newer_schema_version(version: i64, supported: u32, log_prefix: &str) {
    eprintln!(
        "{log_prefix} sqlite schema version {version} is newer than \
         this binary supports ({supported}); skipping schema migration"
    );
}

fn read_user_version(conn: &Connection) -> Result<i64, KernelError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn require_tables(tables: &[TableSpec]) -> Result<(), KernelError> {
    if tables.is_empty() {
        return Err(KernelError::EmptySchema);
    }
    Ok(())
}

/// Creates tables and indexes and applies convergent column migrations.
///
/// Faithful to v1 `ensure_schema`, including the parts that look redundant:
///
/// * **Phase one** (no transaction): set `journal_mode=WAL`, read
///   `user_version`, bail out with a warning when it is newer, set
///   `auto_vacuum=INCREMENTAL`, then run the migrator when the version is behind.
/// * **Phase two** (transaction): re-read `user_version` and bail out again,
///   set `auto_vacuum`, `CREATE TABLE IF NOT EXISTS`, add missing
///   `extra_columns`, `CREATE INDEX IF NOT EXISTS`, and finally write
///   `user_version` **only** when it was behind.
///
/// The version is checked once per phase because the connection is released in
/// between and another process may have upgraded the database. Collapsing the
/// two checks into one would lose that protection.
///
/// # Errors
///
/// Returns [`KernelError::EmptySchema`] for an empty `tables` slice,
/// [`KernelError::InvalidColumnName`] for an extra column that fails identifier
/// validation, or [`KernelError::Sqlite`] for any DDL failure.
pub fn ensure_schema(
    conn: &Connection,
    tables: &[TableSpec],
    schema_version: u32,
    migrator: Option<&dyn SchemaMigrator>,
    log_prefix: &str,
) -> Result<(), KernelError> {
    require_tables(tables)?;
    let expected = i64::from(schema_version);

    // ---- Phase one: journal mode, migration, no enclosing transaction -----
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let version = read_user_version(conn)?;
    if version > expected {
        warn_newer_schema_version(version, schema_version, log_prefix);
        return Ok(());
    }
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;

    if version < expected
        && let Some(migrator) = migrator
    {
        let from = u32::try_from(version.max(0)).unwrap_or(0);
        migrator.migrate(conn, from, schema_version, log_prefix)?;
    }

    // ---- Phase two: convergence inside one transaction --------------------
    conn.execute_batch("BEGIN")?;
    let result = converge(conn, tables, schema_version, expected, log_prefix);
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(err);
        }
    }
    Ok(())
}

fn converge(
    conn: &Connection,
    tables: &[TableSpec],
    schema_version: u32,
    expected: i64,
    log_prefix: &str,
) -> Result<(), KernelError> {
    let version = read_user_version(conn)?;
    if version > expected {
        warn_newer_schema_version(version, schema_version, log_prefix);
        return Ok(());
    }

    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;

    for table in tables {
        conn.execute_batch(&table.create_table_sql())?;

        if !table.extra_columns.is_empty() {
            let existing = existing_columns(conn, table.name)?;
            for extra in table.extra_columns {
                if existing.iter().any(|name| name == extra.name) {
                    continue;
                }
                if !is_valid_identifier(extra.name) {
                    return Err(KernelError::InvalidColumnName(extra.name.to_owned()));
                }
                conn.execute_batch(&format!(
                    "ALTER TABLE {} ADD COLUMN {} {}",
                    table.name, extra.name, extra.definition
                ))?;
            }
        }

        for index in table.indexes {
            conn.execute_batch(&table.create_index_sql(index))?;
        }
    }

    if version < expected {
        conn.execute_batch(&format!("PRAGMA user_version = {schema_version}"))?;
    }
    Ok(())
}

/// Returns the column names currently present on `table`.
///
/// # Errors
///
/// Returns [`KernelError::Sqlite`] when `PRAGMA table_info` fails.
pub fn existing_columns(conn: &Connection, table: &str) -> Result<Vec<String>, KernelError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// Runs full convergence only when the version changed or repair is forced.
///
/// This is the hot path taken on every open, so the fast return when
/// `version == schema_version && !force` matters: it skips all DDL. `force` comes
/// only from [`crate::store::SqliteStore::request_schema_repair`].
///
/// # Errors
///
/// Propagates whatever [`ensure_schema`] returns, plus
/// [`KernelError::EmptySchema`] for an empty slice.
pub fn ensure_schema_if_needed(
    conn: &Connection,
    tables: &[TableSpec],
    schema_version: u32,
    migrator: Option<&dyn SchemaMigrator>,
    log_prefix: &str,
    force: bool,
) -> Result<(), KernelError> {
    require_tables(tables)?;
    let expected = i64::from(schema_version);
    let version = read_user_version(conn)?;
    if version > expected {
        warn_newer_schema_version(version, schema_version, log_prefix);
        return Ok(());
    }
    if version == expected && !force {
        return Ok(());
    }
    ensure_schema(conn, tables, schema_version, migrator, log_prefix)
}

/// Warns about read-only schema drift without creating or migrating anything.
///
/// The three branches and their wording come from v1
/// `warn_readonly_schema_readiness` and must stay byte-identical:
/// newer version, behind-but-complete (and non-zero), or missing tables /
/// version zero.
///
/// # Errors
///
/// Returns [`KernelError::EmptySchema`] for an empty slice, or
/// [`KernelError::Sqlite`] when the database cannot be inspected.
pub fn warn_readonly_schema_readiness(
    conn: &Connection,
    tables: &[TableSpec],
    schema_version: u32,
    log_prefix: &str,
) -> Result<(), KernelError> {
    require_tables(tables)?;
    let expected = i64::from(schema_version);
    let version = read_user_version(conn)?;

    // v1 short-circuits the table scan when the version is newer.
    let missing = if version > expected {
        Vec::new()
    } else {
        missing_tables(conn, tables)?
    };

    if version > expected {
        warn_newer_schema_version(version, schema_version, log_prefix);
    } else if version < expected && missing.is_empty() && version != 0 {
        eprintln!(
            "{log_prefix} sqlite schema is v{version}, \
             this binary expects v{schema_version}; \
             run any write command (for example `agent-sec-cli scan-code ...`) \
             to migrate. read-only queries may return empty results until then."
        );
    } else if !missing.is_empty() || version == 0 {
        eprintln!(
            "{log_prefix} sqlite schema not ready for read-only access: \
             version={version}, expected={schema_version}, \
             missing_tables={}",
            format_python_list(&missing)
        );
    }
    Ok(())
}

/// Returns the specified tables that do not exist in the database.
///
/// # Errors
///
/// Returns [`KernelError::Sqlite`] when `sqlite_master` cannot be queried.
pub fn missing_tables(conn: &Connection, tables: &[TableSpec]) -> Result<Vec<String>, KernelError> {
    let mut missing = Vec::new();
    for table in tables {
        let present: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table.name],
            |row| row.get(0),
        )?;
        if present == 0 {
            missing.push(table.name.to_owned());
        }
    }
    Ok(missing)
}

/// Renders a string list the way Python's `repr` does, for warning parity.
fn format_python_list(items: &[String]) -> String {
    let body = items
        .iter()
        .map(|item| format!("'{item}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{body}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_rule_rejects_digits_like_v1() {
        for good in ["verdict", "run_id", "_leading", "a_b_c"] {
            assert!(is_valid_identifier(good), "{good} should pass");
        }
        for bad in ["run_id2", "Run_ID", "a-b", "a b", "a;DROP TABLE x", "", "A"] {
            assert!(!is_valid_identifier(bad), "{bad} should fail");
        }
    }

    #[test]
    fn python_list_formatting_matches_v1_warning() {
        assert_eq!(format_python_list(&[]), "[]");
        assert_eq!(
            format_python_list(&["security_events".to_owned()]),
            "['security_events']"
        );
        assert_eq!(
            format_python_list(&["a".to_owned(), "b".to_owned()]),
            "['a', 'b']"
        );
    }

    const TEST_TABLE: TableSpec = TableSpec {
        name: "widgets",
        columns: &[
            ColumnSpec {
                name: "id",
                definition: "TEXT PRIMARY KEY",
            },
            ColumnSpec {
                name: "label",
                definition: "TEXT NOT NULL DEFAULT 'x'",
            },
        ],
        indexes: &[IndexSpec {
            name: "idx_widgets_label",
            columns: &["label"],
        }],
        extra_columns: &[],
    };

    #[test]
    fn ddl_rendering_keeps_declaration_order() {
        assert_eq!(
            TEST_TABLE.create_table_sql(),
            "CREATE TABLE IF NOT EXISTS widgets \
             (id TEXT PRIMARY KEY, label TEXT NOT NULL DEFAULT 'x')"
        );
        assert_eq!(
            TEST_TABLE.create_index_sql(&TEST_TABLE.indexes[0]),
            "CREATE INDEX IF NOT EXISTS idx_widgets_label ON widgets (label)"
        );
    }

    #[test]
    fn multi_column_index_keeps_column_order() {
        let index = IndexSpec {
            name: "idx_multi",
            columns: &["session_id", "run_id", "timestamp_epoch"],
        };
        assert_eq!(
            TEST_TABLE.create_index_sql(&index),
            "CREATE INDEX IF NOT EXISTS idx_multi ON widgets \
             (session_id, run_id, timestamp_epoch)"
        );
    }

    /// `auto_vacuum` stays at `NONE`, and that is the v1-equivalent outcome.
    ///
    /// `SQLite` only honours a `none` → `incremental` switch on a database that
    /// is still new; setting `journal_mode=WAL` first ends that window. v1's
    /// `ensure_schema` issues the two `PRAGMA`s in exactly that order, so v1's
    /// databases read back `0` too and its `auto_vacuum` line has never had an
    /// effect. Reordering the two statements here would *fix* the intent but
    /// break equivalence, and the differential harness would then flag the
    /// header as divergent — so the current order is deliberate and this test
    /// exists to stop a well-meaning reordering.
    #[test]
    fn auto_vacuum_stays_none_exactly_as_in_v1() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let conn = crate::connection::open_connection(&path, false).expect("connection");
        ensure_schema(&conn, &[TEST_TABLE], 1, None, "[test]").expect("schema");

        let mode: u32 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(
            mode, 0,
            "a change here means v2 diverged from v1's database header"
        );
    }
}
