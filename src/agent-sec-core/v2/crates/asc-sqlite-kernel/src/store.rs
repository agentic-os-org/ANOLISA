//! Connection lifecycle, corruption recovery and the repair flag.
//!
//! Migrated from v1 `SqliteStore`. v1 holds a pooled `SQLAlchemy` engine; v2 holds
//! a single [`Connection`] behind a mutex, because `rusqlite::Connection` is not
//! `Sync` and v1's write path is serialized by a `threading.Lock` anyway.
//!
//! The type is `Send` on purpose, asserted at compile time in
//! [`crate::assert_send`]. `AGENT_SEC_RUST_MIGRATION_zh.md` records "synchronous
//! `SQLite` reads block the event loop" as a known v1 gap, and
//! `DAEMON_PROTOCOL_V1_zh.md` requires synchronous `SQLite` not to block the
//! socket runtime's accept loop. Keeping the store `Send` is what will allow it
//! to move into `spawn_blocking` when the daemon is wired up.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::connection::open_connection;
use crate::error::{KernelError, is_corruption};
use crate::migration::SchemaMigrator;
use crate::path::{normalize_sqlite_path, sqlite_database_files};
use crate::schema::{TableSpec, ensure_schema_if_needed, warn_readonly_schema_readiness};

/// Mode applied to a created parent directory.
const PRIVATE_DIR_MODE: u32 = 0o700;
/// Mode applied to the database file itself.
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Default)]
struct Inner {
    connection: Option<Connection>,
    /// `(st_dev, st_ino)` of the file behind the cached read-only connection.
    db_identity: Option<(u64, u64)>,
    disabled: bool,
    force_schema_convergence: bool,
}

/// Shared `SQLite` connection lifecycle for typed repositories.
pub struct SqliteStore {
    path: PathBuf,
    read_only: bool,
    schema_version: u32,
    tables: &'static [TableSpec],
    migrator: Option<Arc<dyn SchemaMigrator>>,
    log_prefix: String,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore")
            .field("path", &self.path)
            .field("read_only", &self.read_only)
            .field("schema_version", &self.schema_version)
            .field("tables", &self.tables.len())
            .field("has_migrator", &self.migrator.is_some())
            .field("log_prefix", &self.log_prefix)
            .field("inner", &self.inner)
            .finish()
    }
}

impl SqliteStore {
    /// Creates a store for `path`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::EmptySchema`] when `tables` is empty. v1 rejects an
    /// empty model tuple in `_require_models`; with no global default registry
    /// the check lives here.
    pub fn new(
        path: impl AsRef<Path>,
        read_only: bool,
        schema_version: u32,
        tables: &'static [TableSpec],
        migrator: Option<Arc<dyn SchemaMigrator>>,
        log_prefix: impl Into<String>,
    ) -> Result<Self, KernelError> {
        if tables.is_empty() {
            return Err(KernelError::EmptySchema);
        }
        Ok(Self {
            path: normalize_sqlite_path(path),
            read_only,
            schema_version,
            tables,
            migrator,
            log_prefix: log_prefix.into(),
            inner: Mutex::new(Inner::default()),
        })
    }

    /// Returns the normalized database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns whether this store was opened read-only.
    #[must_use]
    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the expected schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the table contract.
    #[must_use]
    pub const fn tables(&self) -> &'static [TableSpec] {
        self.tables
    }

    /// Returns the diagnostic prefix.
    #[must_use]
    pub fn log_prefix(&self) -> &str {
        &self.log_prefix
    }

    /// Returns whether corruption cleanup failed and writes are disabled.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.lock().disabled
    }

    /// Returns whether a connection is currently cached.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.lock().connection.is_some()
    }

    /// Returns whether the next open will run full convergence.
    #[must_use]
    pub fn repair_requested(&self) -> bool {
        self.lock().force_schema_convergence
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `operation` against a live connection.
    ///
    /// Returns `Ok(None)` when no connection is available, mirroring v1
    /// `session_factory()` returning `None`: the store is disabled, the
    /// read-only target vanished, or initialization failed while
    /// `raise_on_error` is false.
    ///
    /// # Errors
    ///
    /// Propagates whatever `operation` returns, and — when `raise_on_error` is
    /// true — connection/schema initialization failures.
    pub fn with_connection<T>(
        &self,
        raise_on_error: bool,
        operation: impl FnOnce(&Connection) -> Result<T, KernelError>,
    ) -> Result<Option<T>, KernelError> {
        let mut inner = self.lock();
        if inner.disabled {
            if raise_on_error {
                return Err(KernelError::Disabled);
            }
            return Ok(None);
        }

        if self
            .prepare_connection(&mut inner, raise_on_error)?
            .is_none()
        {
            return Ok(None);
        }

        // `prepare_connection` guarantees the slot is filled; the `else` arm is
        // kept instead of an `expect` so a future refactor degrades rather than
        // panics.
        let Some(conn) = inner.connection.as_ref() else {
            return Ok(None);
        };
        operation(conn).map(Some)
    }

    /// Ensures `inner.connection` holds a usable connection.
    ///
    /// Reproduces v1 `session_factory`'s branching, including the read-only
    /// identity check and the single corruption rebuild attempt.
    fn prepare_connection(
        &self,
        inner: &mut Inner,
        raise_on_error: bool,
    ) -> Result<Option<()>, KernelError> {
        let mut identity = None;
        if self.read_only {
            // A replaced file (new inode) must not be served from a stale
            // handle, so the identity is compared on every access.
            let Some(current) = self.current_db_identity() else {
                dispose_inner(inner);
                return Ok(None);
            };
            identity = Some(current);
            if inner.connection.is_some() && inner.db_identity == identity {
                return Ok(Some(()));
            }
            dispose_inner(inner);
        } else if inner.connection.is_some() {
            return Ok(Some(()));
        }

        match self.open_and_prepare(inner, identity) {
            Ok(()) => Ok(Some(())),
            Err(err) => {
                if raise_on_error {
                    return Err(err);
                }
                if self.read_only || !is_corruption(&err) {
                    eprintln!("{} schema init failure: {err}", self.log_prefix);
                    return Ok(None);
                }
                self.handle_corruption_locked(inner, &err);
                if inner.disabled {
                    return Ok(None);
                }
                match self.open_and_prepare(inner, None) {
                    Ok(()) => Ok(Some(())),
                    Err(rebuild_err) => {
                        eprintln!(
                            "{} corruption rebuild failed: {rebuild_err}",
                            self.log_prefix
                        );
                        Ok(None)
                    }
                }
            }
        }
    }

    fn open_and_prepare(
        &self,
        inner: &mut Inner,
        identity: Option<(u64, u64)>,
    ) -> Result<(), KernelError> {
        let force = inner.force_schema_convergence;
        if !self.read_only {
            self.ensure_write_parent()?;
        }

        let conn = open_connection(&self.path, self.read_only)?;
        let prepared = if self.read_only {
            warn_readonly_schema_readiness(
                &conn,
                self.tables,
                self.schema_version,
                &self.log_prefix,
            )
        } else {
            ensure_schema_if_needed(
                &conn,
                self.tables,
                self.schema_version,
                self.migrator.as_deref(),
                &self.log_prefix,
                force,
            )
        };
        if let Err(err) = prepared {
            drop(conn);
            return Err(err);
        }

        inner.connection = Some(conn);
        inner.db_identity = identity;
        if !self.read_only {
            // Best effort, exactly as v1: a chmod failure must not fail the open.
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(PRIVATE_FILE_MODE));
        }
        inner.force_schema_convergence = false;
        Ok(())
    }

    /// Drops the cached connection and its identity.
    pub fn dispose(&self) {
        dispose_inner(&mut self.lock());
    }

    /// Alias of [`SqliteStore::dispose`], matching v1 `close()`.
    pub fn close(&self) {
        self.dispose();
    }

    /// Forces full schema convergence on the next open.
    ///
    /// The flag survives a concurrent open because it is only cleared inside the
    /// same critical section that performs the convergence.
    pub fn request_schema_repair(&self) {
        let mut inner = self.lock();
        inner.force_schema_convergence = true;
        dispose_inner(&mut inner);
    }

    /// Deletes a corrupt, expendable query index and clears cached state.
    ///
    /// When deletion fails the store becomes permanently `disabled`, which is how
    /// v1 stops a hopeless write loop.
    pub fn handle_corruption(&self, err: &KernelError) {
        let mut inner = self.lock();
        self.handle_corruption_locked(&mut inner, err);
    }

    fn handle_corruption_locked(&self, inner: &mut Inner, err: &KernelError) {
        eprintln!("{} corrupt DB detected, recreating: {err}", self.log_prefix);
        dispose_inner(inner);
        for db_file in sqlite_database_files(&self.path) {
            match fs::remove_file(&db_file) {
                Ok(()) => {}
                Err(remove_err) if remove_err.kind() == std::io::ErrorKind::NotFound => {}
                Err(remove_err) => {
                    inner.disabled = true;
                    eprintln!(
                        "{} cannot delete corrupt db, writer disabled: {remove_err}",
                        self.log_prefix
                    );
                    return;
                }
            }
        }
    }

    /// Creates the parent directory with mode `0o700`.
    ///
    /// v1 additionally chmods every directory it had to create, because
    /// `mkdir(mode=...)` is subject to the umask.
    fn ensure_write_parent(&self) -> Result<(), KernelError> {
        let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) else {
            return Ok(());
        };

        let mut created: Vec<PathBuf> = Vec::new();
        let mut current = parent;
        while !current.exists() {
            created.push(current.to_path_buf());
            match current.parent() {
                Some(next) if next != current => current = next,
                _ => break,
            }
        }

        fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR_MODE)
            .create(parent)
            .map_err(|err| KernelError::io("create directory", parent, err))?;

        for directory in created {
            let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(PRIVATE_DIR_MODE));
        }
        Ok(())
    }

    fn current_db_identity(&self) -> Option<(u64, u64)> {
        let metadata = fs::metadata(&self.path).ok()?;
        Some((metadata.dev(), metadata.ino()))
    }
}

fn dispose_inner(inner: &mut Inner) {
    inner.connection = None;
    inner.db_identity = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnSpec, IndexSpec};
    use tempfile::TempDir;

    const WIDGETS: &[TableSpec] = &[TableSpec {
        name: "widgets",
        columns: &[
            ColumnSpec {
                name: "id",
                definition: "TEXT PRIMARY KEY",
            },
            ColumnSpec {
                name: "label",
                definition: "TEXT NOT NULL",
            },
        ],
        indexes: &[IndexSpec {
            name: "idx_widgets_label",
            columns: &["label"],
        }],
        extra_columns: &[],
    }];

    fn store_at(path: &Path, read_only: bool) -> SqliteStore {
        SqliteStore::new(path, read_only, 1, WIDGETS, None, "[test]").expect("store")
    }

    #[test]
    fn rejects_an_empty_table_slice() {
        let dir = TempDir::new().expect("temp dir");
        let err = SqliteStore::new(dir.path().join("a.db"), false, 1, &[], None, "[test]")
            .expect_err("empty schema must be rejected");
        assert!(matches!(err, KernelError::EmptySchema));
    }

    #[test]
    fn writable_store_creates_schema_and_tightens_modes() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nested/deeper/widgets.db");
        let store = store_at(&path, false);

        let version = store
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('a', 'b')", [])?;
                Ok(conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?)
            })
            .expect("connection")
            .expect("value");
        assert_eq!(version, 1);

        assert_eq!(
            fs::metadata(store.path()).expect("db").mode() & 0o7777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().expect("parent"))
                .expect("dir")
                .mode()
                & 0o077,
            0
        );
    }

    #[test]
    fn writable_store_caches_the_connection() {
        let dir = TempDir::new().expect("temp dir");
        let store = store_at(&dir.path().join("widgets.db"), false);
        assert!(!store.is_open());
        store
            .with_connection(true, |_| Ok(()))
            .expect("connection")
            .expect("value");
        assert!(store.is_open());
        store.dispose();
        assert!(!store.is_open());
    }

    #[test]
    fn read_only_store_never_creates_the_database() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let store = store_at(&path, true);
        let outcome = store
            .with_connection(false, |_| Ok(()))
            .expect("no hard error");
        assert!(outcome.is_none());
        assert!(!path.exists(), "read-only access must not create the file");
    }

    #[test]
    fn read_only_store_reopens_when_the_file_is_replaced() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        store_at(&path, false)
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('a', 'first')", [])?;
                Ok(())
            })
            .expect("write")
            .expect("value");

        let reader = store_at(&path, true);
        let first = reader
            .with_connection(false, |conn| {
                Ok(conn.query_row("SELECT label FROM widgets", [], |row| {
                    row.get::<_, String>(0)
                })?)
            })
            .expect("read")
            .expect("value");
        assert_eq!(first, "first");

        // Replace the database with a different inode carrying different data.
        let replacement = dir.path().join("replacement.db");
        store_at(&replacement, false)
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('a', 'second')", [])?;
                Ok(())
            })
            .expect("write")
            .expect("value");
        fs::rename(&replacement, &path).expect("replace");

        let second = reader
            .with_connection(false, |conn| {
                Ok(conn.query_row("SELECT label FROM widgets", [], |row| {
                    row.get::<_, String>(0)
                })?)
            })
            .expect("read")
            .expect("value");
        assert_eq!(second, "second", "a replaced inode must be reopened");
    }

    #[test]
    fn corruption_handling_deletes_all_three_files() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let store = store_at(&path, false);
        store
            .with_connection(true, |_| Ok(()))
            .expect("open")
            .expect("value");
        for file in sqlite_database_files(store.path()) {
            let _ = fs::write(&file, b"x");
        }

        store.handle_corruption(&KernelError::Malformed("test".to_owned()));
        for file in sqlite_database_files(store.path()) {
            assert!(!file.exists(), "{} should be gone", file.display());
        }
        assert!(!store.is_disabled());
        assert!(!store.is_open());
    }

    #[test]
    fn a_corrupt_database_is_rebuilt_on_open() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        fs::write(&path, b"this is definitely not a sqlite database").expect("seed");

        let store = store_at(&path, false);
        let opened = store
            .with_connection(false, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('a', 'b')", [])?;
                Ok(())
            })
            .expect("no hard error");
        assert!(opened.is_some(), "the store should rebuild and succeed");
        assert!(!store.is_disabled());
    }

    #[test]
    fn repair_request_forces_convergence_and_survives_until_used() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let store = store_at(&path, false);
        store
            .with_connection(true, |_| Ok(()))
            .expect("open")
            .expect("value");

        // Drop the table behind the store's back, then request a repair.
        store
            .with_connection(true, |conn| {
                conn.execute_batch("DROP TABLE widgets")?;
                Ok(())
            })
            .expect("drop")
            .expect("value");
        store.request_schema_repair();
        assert!(store.repair_requested());
        assert!(!store.is_open(), "repair must drop the cached connection");

        store
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('a', 'b')", [])?;
                Ok(())
            })
            .expect("repaired")
            .expect("value");
        assert!(!store.repair_requested(), "the flag clears after use");
    }

    #[test]
    fn disabled_store_returns_none_or_errors() {
        let dir = TempDir::new().expect("temp dir");
        let store = store_at(&dir.path().join("widgets.db"), false);
        {
            let mut inner = store.lock();
            inner.disabled = true;
        }
        assert!(store.is_disabled());
        assert!(
            store
                .with_connection(false, |_| Ok(()))
                .expect("soft")
                .is_none()
        );
        assert!(matches!(
            store.with_connection(true, |_| Ok(())),
            Err(KernelError::Disabled)
        ));
    }

    #[test]
    fn read_only_store_does_not_migrate_an_unready_schema() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        {
            // A database that exists but has no table and version 0.
            let conn = open_connection(&path, false).expect("open");
            conn.execute_batch("CREATE TABLE unrelated (a INTEGER)")
                .expect("create");
        }

        let reader = store_at(&path, true);
        reader
            .with_connection(false, |_| Ok(()))
            .expect("read-only open")
            .expect("connection");

        let conn = open_connection(&path, false).expect("reopen");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 0, "read-only access must not stamp the version");
        let tables = crate::schema::missing_tables(&conn, WIDGETS).expect("inspect");
        assert_eq!(
            tables,
            vec!["widgets".to_owned()],
            "read-only access must not create tables"
        );
    }
}
