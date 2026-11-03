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
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;
use rustix::fs::{Dev, FileType, lstat};
use rustix::io::Errno;

use crate::connection::open_connection;
use crate::error::{KernelError, is_corruption};
use crate::migration::SchemaMigrator;
use crate::path::{normalize_sqlite_path, sqlite_database_files};
use crate::schema::{TableSpec, ensure_schema_if_needed, warn_readonly_schema_readiness};

/// Mode applied to a created parent directory.
const PRIVATE_DIR_MODE: u32 = 0o700;
/// Mode applied to the database file itself.
const PRIVATE_FILE_MODE: u32 = 0o600;

type DbIdentity = (Dev, u64);

#[derive(Debug, Default)]
struct Inner {
    connection: Option<Connection>,
    /// Native `(st_dev, st_ino)` of the cached read-only database.
    db_identity: Option<DbIdentity>,
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
            let current = match self.current_db_identity() {
                Ok(Some(current)) => current,
                Ok(None) => {
                    dispose_inner(inner);
                    return Ok(None);
                }
                Err(err) => {
                    dispose_inner(inner);
                    if raise_on_error {
                        return Err(err);
                    }
                    tracing::warn!(target: "asc_process_diagnostic", "{} read-only database path rejected: {err}", self.log_prefix);
                    return Ok(None);
                }
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
                if matches!(err, KernelError::UnsafePath(_)) {
                    tracing::warn!(target: "asc_process_diagnostic",
                        "{} refusing unsafe database path: {err}", self.log_prefix);
                    return Ok(None);
                }
                if self.read_only || !is_corruption(&err) {
                    tracing::warn!(target: "asc_process_diagnostic", "{} schema init failure: {err}", self.log_prefix);
                    return Ok(None);
                }
                self.handle_corruption_locked(inner, &err);
                if inner.disabled {
                    return Ok(None);
                }
                match self.open_and_prepare(inner, None) {
                    Ok(()) => Ok(Some(())),
                    Err(rebuild_err) => {
                        tracing::warn!(target: "asc_process_diagnostic",
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
        identity: Option<DbIdentity>,
    ) -> Result<(), KernelError> {
        let force = inner.force_schema_convergence;
        if !self.read_only {
            self.ensure_write_parent()?;
            // The writable open is gated like the read-only identity check: a
            // symlink, FIFO, directory or device node standing in for the
            // database is refused before `SQLite` opens it. Without the gate
            // the open follows a symlink (the daemon appends to the link
            // target, and the post-open `set_permissions` tightens the
            // *target's* mode), creates the target of a dangling link, and
            // reports a FIFO as a disk I/O error while every write is silently
            // dropped. A legitimate rename replacement is still a regular
            // file and keeps opening. The check covers the corruption-rebuild
            // retry too, because that path re-enters here.
            //
            // The gate and the open are still two steps, so the defense is
            // layered: this `lstat` classifies what it sees (missing = the
            // legitimate create-on-open case, link/FIFO/directory/device =
            // refused, with a message that names the entry), while
            // `SQLITE_OPEN_NOFOLLOW` refuses a symlink swapped into the
            // window atomically at the open, and the open failure is
            // re-examined below so the race reports `UnsafePath` too.
            if self.writable_path_is_unsafe() {
                return Err(KernelError::UnsafePath(self.path.clone()));
            }
            #[cfg(test)]
            if let Some(interpose) = GATE_TO_OPEN_INTERPOSE.lock().unwrap().as_ref() {
                interpose(&self.path);
            }
        }

        let conn =
            open_connection(&self.path, self.read_only).map_err(|err| self.unsafe_path_or(err))?;
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
        tracing::warn!(target: "asc_process_diagnostic", "{} corrupt DB detected, recreating: {err}", self.log_prefix);
        dispose_inner(inner);
        for db_file in sqlite_database_files(&self.path) {
            match fs::remove_file(&db_file) {
                Ok(()) => {}
                Err(remove_err) if remove_err.kind() == std::io::ErrorKind::NotFound => {}
                Err(remove_err) => {
                    inner.disabled = true;
                    tracing::warn!(target: "asc_process_diagnostic",
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

    fn current_db_identity(&self) -> Result<Option<DbIdentity>, KernelError> {
        // lstat avoids opening the path: opening a second database descriptor
        // can interfere with SQLite's POSIX advisory locks.
        let metadata = match lstat(&self.path) {
            Ok(metadata) => metadata,
            Err(Errno::NOENT) => return Ok(None),
            Err(err) => return Err(KernelError::io("inspect", &self.path, err.into())),
        };
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
            return Err(KernelError::UnsafePath(self.path.clone()));
        }
        Ok(Some((metadata.st_dev as Dev, metadata.st_ino as u64)))
    }

    /// Returns whether the writable database path names an unsafe entry.
    ///
    /// `lstat` inspects the path the caller named rather than the file it
    /// might point to, so a symlink is refused even when its target is a
    /// perfectly regular database, matching the read-only identity check's
    /// discrimination of the entry itself.
    fn writable_path_is_unsafe(&self) -> bool {
        match lstat(&self.path) {
            // Anything that is not a regular file - a symlink, a FIFO, a
            // directory, a device node - must not be opened for writing.
            Ok(metadata) => FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile,
            // A missing path is the legitimate create-on-open case, and any
            // other inspection failure is surfaced by the open itself.
            Err(_) => false,
        }
    }

    /// Re-examines the path after a writable open refused it.
    ///
    /// The pre-open gate classifies what it saw, but the check and the open
    /// are two steps: a symlink swapped in between them is refused atomically
    /// by `SQLITE_OPEN_NOFOLLOW` at the open. This second look names what was
    /// refused - the race reports [`KernelError::UnsafePath`] instead of an
    /// opaque "unable to open database file". A path that is still a regular
    /// file keeps its original error: the classification only ever runs
    /// after a refusal.
    fn unsafe_path_or(&self, err: KernelError) -> KernelError {
        if self.read_only || !self.writable_path_is_unsafe() {
            return err;
        }
        KernelError::UnsafePath(self.path.clone())
    }
}

fn dispose_inner(inner: &mut Inner) {
    inner.connection = None;
    inner.db_identity = None;
}

/// The closure a regression test installs in [`GATE_TO_OPEN_INTERPOSE`].
#[cfg(test)]
type GateInterpose = Box<dyn Fn(&Path) + Send>;

/// Test seam between the writable path gate and the open.
///
/// The check-then-open race - a symlink swapped in after the `lstat` gate
/// passed but before `SQLite` opened the path - cannot be scheduled from a
/// test thread, so a regression test installs a path-matched closure here to
/// plant that swap inside the window deterministically. Compiled out of
/// non-test builds, and inert for any path it does not match.
#[cfg(test)]
static GATE_TO_OPEN_INTERPOSE: Mutex<Option<GateInterpose>> = Mutex::new(None);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnSpec, IndexSpec};
    use std::os::unix::fs::{MetadataExt, symlink};
    use tempfile::TempDir;

    const WIDGETS: &[TableSpec] = &[TableSpec {
        name: "widgets",
        strict: false,
        constraints: &[],
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

    fn seed_with_label(path: &Path, label: &str) {
        store_at(path, false)
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('a', ?1)", [label])?;
                Ok(())
            })
            .expect("seed")
            .expect("value");
    }

    fn all_labels(path: &Path) -> Vec<String> {
        store_at(path, true)
            .with_connection(true, |conn| {
                let mut stmt = conn.prepare("SELECT label FROM widgets ORDER BY id")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .expect("read")
            .expect("rows")
    }

    #[test]
    fn a_symlink_present_before_construction_is_refused_for_writing_too() {
        // Normalization resolves the longest existing prefix, and v1 followed
        // that rule for the final component as well: a link planted *before*
        // the store was built used to be redirected to its target at
        // `SqliteStore::new`, where no gate could see it. Normalization now
        // preserves the final link, so the writable gate refuses it exactly
        // like a post-construction swap.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let target = dir.path().join("elsewhere.db");
        seed_with_label(&target, "foreign");
        symlink(&target, &path).expect("plant the symlink before construction");

        let writer = store_at(&path, false);
        let outcome = writer.with_connection(true, |conn| {
            conn.execute("INSERT INTO widgets (id, label) VALUES ('b', 'own')", [])?;
            Ok(())
        });
        assert!(
            matches!(outcome, Err(KernelError::UnsafePath(_))),
            "a link planted before construction must not be written through, got {outcome:?}"
        );
        assert_eq!(
            all_labels(&target),
            vec!["foreign".to_owned()],
            "the link target must stay untouched"
        );
    }

    #[test]
    fn a_writable_store_does_not_write_through_a_symlinked_path() {
        // The writable open must not write *through* a symlink. Following the
        // link appends the daemon rows to an arbitrary target, and the
        // post-open `set_permissions` tightens the target mode through
        // the link.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let target = dir.path().join("elsewhere.db");
        seed_with_label(&target, "foreign");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644))
            .expect("widen the target mode");
        symlink(&target, &path).expect("plant the symlink");

        let writer = store_at(&path, false);
        let outcome = writer.with_connection(true, |conn| {
            conn.execute("INSERT INTO widgets (id, label) VALUES ('b', 'own')", [])?;
            Ok(())
        });
        assert!(
            matches!(outcome, Err(KernelError::UnsafePath(_))),
            "a raising writer must refuse the unsafe path, got {outcome:?}"
        );
        assert_eq!(
            all_labels(&target),
            vec!["foreign".to_owned()],
            "the link target must stay untouched"
        );
        let mode = fs::metadata(&target).expect("target").mode() & 0o7777;
        assert_eq!(
            mode, 0o644,
            "the target mode must not be tightened through the link"
        );
    }

    #[test]
    fn a_degraded_writer_drops_the_write_instead_of_following_the_link() {
        // The fire-and-forget sink shape (`raise_on_error = false`) degrades
        // fail-closed: no error, no write through the link, the store simply
        // stays closed - consistent with the v1 sink's never-fail contract.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let target = dir.path().join("elsewhere.db");
        seed_with_label(&target, "foreign");
        symlink(&target, &path).expect("plant the symlink");

        let writer = store_at(&path, false);
        let outcome = writer.with_connection(false, |conn| {
            conn.execute("INSERT INTO widgets (id, label) VALUES ('b', 'own')", [])?;
            Ok(())
        });
        assert!(
            matches!(outcome, Ok(None)),
            "the degraded shape stays fail-closed, got {outcome:?}"
        );
        assert_eq!(
            all_labels(&target),
            vec!["foreign".to_owned()],
            "no row may be written through the link"
        );
    }

    #[test]
    fn a_dangling_symlink_is_not_created_through() {
        // `SQLite` creates a missing database on open, so without the gate a
        // dangling link materializes its target as this process user.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let target = dir.path().join("nowhere.db");
        symlink(&target, &path).expect("dangling symlink");

        let writer = store_at(&path, false);
        let outcome = writer.with_connection(true, |_| Ok(()));
        assert!(
            matches!(outcome, Err(KernelError::UnsafePath(_))),
            "a dangling link must not be created through, got {outcome:?}"
        );
        assert!(!target.exists(), "the link target must not be created");
        assert!(
            fs::symlink_metadata(&path)
                .expect("link")
                .file_type()
                .is_symlink(),
            "the link itself stays as the caller left it"
        );
    }

    #[test]
    fn a_symlink_swapped_into_the_gate_to_open_window_is_refused() {
        // The `lstat` gate and the open are two steps, and an attacker who
        // can write the data directory can replace the path between them:
        // the gate sees a regular file, the open follows the swapped-in
        // link. The interposition seam plants that swap deterministically,
        // so only the open - not the gate - can still refuse the link:
        // `SQLITE_OPEN_NOFOLLOW` refuses it atomically, and the open-failure
        // re-examination keeps the refusal classified as `UnsafePath`.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        let target = dir.path().join("elsewhere.db");
        seed_with_label(&target, "foreign");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644))
            .expect("widen the target mode");
        fs::write(&path, b"").expect("a regular file the gate will pass on");
        let swap_path = path.clone();
        let swap_target = target.clone();

        {
            let mut interpose = GATE_TO_OPEN_INTERPOSE.lock().unwrap();
            *interpose = Some(Box::new(move |opened: &Path| {
                if opened == swap_path {
                    let stash = opened.with_extension("stashed");
                    fs::rename(opened, &stash).expect("stash the regular file");
                    symlink(&swap_target, opened).expect("plant the symlink inside the window");
                }
            }));
        }

        let writer = store_at(&path, false);
        let outcome = writer.with_connection(true, |conn| {
            conn.execute("INSERT INTO widgets (id, label) VALUES ('b', 'own')", [])?;
            Ok(())
        });

        GATE_TO_OPEN_INTERPOSE.lock().unwrap().take();

        assert!(
            matches!(outcome, Err(KernelError::UnsafePath(_))),
            "a link swapped in after the gate must still be refused, got {outcome:?}"
        );
        assert_eq!(
            all_labels(&target),
            vec!["foreign".to_owned()],
            "the race must not write through the swapped-in link"
        );
        let mode = fs::metadata(&target).expect("target").mode() & 0o7777;
        assert_eq!(
            mode, 0o644,
            "the race must not tighten the target's mode through the link"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_fifo_at_the_database_path_is_refused_before_opening() {
        // A FIFO does not hang a writable open (`SQLite` opens read-write,
        // which does not block on a FIFO), but the open degenerates into a
        // misleading disk I/O error while every write is silently dropped.
        // The gate names the actual problem instead.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .expect("fifo");

        let writer = store_at(&path, false);
        let outcome = writer.with_connection(true, |_| Ok(()));
        assert!(
            matches!(outcome, Err(KernelError::UnsafePath(_))),
            "a FIFO must not be opened as a database, got {outcome:?}"
        );
    }

    #[test]
    fn a_directory_at_the_database_path_is_refused_for_writing() {
        let dir = TempDir::new().expect("temp dir");
        let nested = dir.path().join("as-a-directory");
        fs::create_dir(&nested).expect("directory");

        let writer = store_at(&nested, false);
        let outcome = writer.with_connection(true, |_| Ok(()));
        assert!(
            matches!(outcome, Err(KernelError::UnsafePath(_))),
            "a directory must not be opened as a database, got {outcome:?}"
        );
    }

    #[test]
    fn a_writable_store_reopens_when_the_file_is_replaced_by_a_rename() {
        // The gate only refuses non-regular entries: a legitimate replace
        // (a new inode, still a regular file) keeps opening and writable.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        seed_with_label(&path, "first");

        let replacement = dir.path().join("replacement.db");
        seed_with_label(&replacement, "second");
        fs::remove_file(&path).expect("move the original aside");
        fs::rename(&replacement, &path).expect("swap in the replacement");

        let writer = store_at(&path, false);
        writer
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id, label) VALUES ('b', 'own')", [])?;
                Ok(())
            })
            .expect("write")
            .expect("value");
        assert_eq!(
            all_labels(&path),
            vec!["second".to_owned(), "own".to_owned()],
            "a renamed-in regular file must be written to, not refused"
        );
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
