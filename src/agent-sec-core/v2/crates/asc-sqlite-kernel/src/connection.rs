//! Connection opening with v1's exact `PRAGMA` sequence.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::error::KernelError;

/// Opens a connection to `path`.
///
/// The `PRAGMA` order is taken verbatim from v1 `create_sqlite_engine`'s
/// `connect` listener: `busy_timeout`, `foreign_keys`, then either `query_only`
/// (read-only) or `synchronous` + `wal_autocheckpoint` (writable).
///
/// Read-only mode uses a `file:` URI with `mode=ro`, as v1 does, so a missing
/// file fails rather than being created.
///
/// # Errors
///
/// Returns [`KernelError::Sqlite`] if the connection cannot be opened or a
/// `PRAGMA` is rejected.
pub fn open_connection(path: &Path, read_only: bool) -> Result<Connection, KernelError> {
    let conn = if read_only {
        let uri = format!("file:{}?mode=ro", encode_uri_path(path));
        Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    } else {
        // The writable open carries `SQLITE_OPEN_NOFOLLOW` (SQLite >= 3.36,
        // unix): the kernel refuses a symlinked final component atomically at
        // the open, closing the window between the store's pre-open `lstat`
        // gate and this call. The flag set is `Connection::open`'s default
        // (read-write, create, no mutex, URI) plus that refusal.
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    };

    conn.pragma_update(None, "busy_timeout", 200)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if read_only {
        conn.pragma_update(None, "query_only", "ON")?;
    } else {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "wal_autocheckpoint", 100)?;
    }
    Ok(conn)
}

/// Percent-encodes the characters that would otherwise break a `file:` URI.
///
/// `?` and `#` start the query and fragment, and `%` must be escaped first so
/// the other replacements are not double-decoded.
fn encode_uri_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scalar_pragma(conn: &Connection, name: &str) -> i64 {
        conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .expect("pragma must be readable")
    }

    #[test]
    fn writable_connection_applies_the_v1_pragma_set() {
        let dir = TempDir::new().expect("temp dir");
        let conn = open_connection(&dir.path().join("events.db"), false).expect("open");
        assert_eq!(scalar_pragma(&conn, "busy_timeout"), 200);
        assert_eq!(scalar_pragma(&conn, "foreign_keys"), 1);
        assert_eq!(scalar_pragma(&conn, "synchronous"), 1, "NORMAL");
        assert_eq!(scalar_pragma(&conn, "wal_autocheckpoint"), 100);
        assert_eq!(scalar_pragma(&conn, "query_only"), 0);
    }

    #[test]
    fn read_only_connection_sets_query_only_and_rejects_writes() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        {
            let conn = open_connection(&path, false).expect("open writable");
            conn.execute_batch("CREATE TABLE t (a INTEGER)")
                .expect("create");
        }

        let conn = open_connection(&path, true).expect("open read-only");
        assert_eq!(scalar_pragma(&conn, "query_only"), 1);
        assert_eq!(scalar_pragma(&conn, "busy_timeout"), 200);
        conn.execute_batch("INSERT INTO t (a) VALUES (1)")
            .expect_err("read-only connections must reject writes");
    }

    #[test]
    fn read_only_open_fails_for_a_missing_file() {
        let dir = TempDir::new().expect("temp dir");
        open_connection(&dir.path().join("absent.db"), true)
            .expect_err("mode=ro must not create the database");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_open_rejects_a_symlinked_database() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("target.db");
        open_connection(&target, false).expect("create target");
        let link = dir.path().join("linked.db");
        symlink(&target, &link).expect("symlink");

        assert!(open_connection(&link, true).is_err());
    }

    #[test]
    fn writable_open_creates_the_file() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let _conn = open_connection(&path, false).expect("open");
        assert!(path.exists());
    }

    #[test]
    fn uri_special_characters_are_escaped() {
        assert_eq!(
            encode_uri_path(Path::new("/tmp/a?b#c%d")),
            "/tmp/a%3fb%23c%25d"
        );
    }

    #[test]
    fn read_only_open_works_for_a_path_with_a_question_mark() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("odd?name.db");
        {
            let conn = open_connection(&path, false).expect("open writable");
            conn.execute_batch("CREATE TABLE t (a INTEGER)")
                .expect("create");
        }
        let conn = open_connection(&path, true).expect("open read-only");
        assert_eq!(scalar_pragma(&conn, "query_only"), 1);
    }

    #[test]
    fn a_writable_open_refuses_a_final_component_symlink() {
        // `SQLITE_OPEN_NOFOLLOW` is the atomic refusal behind the store's
        // pre-open `lstat` gate: whatever lands on the path between that
        // check and this call, the open itself must not follow a
        // final-component symlink. A link to an existing database and a
        // dangling link are both refused at the open, with neither target
        // touched.
        use std::fs;
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("elsewhere.db");
        {
            let conn = open_connection(&target, false).expect("seed target");
            conn.execute_batch("CREATE TABLE t (a INTEGER)")
                .expect("create");
        }
        let seeded_len = fs::metadata(&target).expect("target").len();

        let link = dir.path().join("events.db");
        symlink(&target, &link).expect("symlink to an existing database");
        open_connection(&link, false)
            .expect_err("the writable open must not follow a final-component symlink");
        assert_eq!(
            fs::metadata(&target).expect("target").len(),
            seeded_len,
            "the refused open must not touch the link target"
        );

        let dangling = dir.path().join("dangling.db");
        let nowhere = dir.path().join("nowhere.db");
        symlink(&nowhere, &dangling).expect("dangling symlink");
        open_connection(&dangling, false).expect_err("a dangling link must not be created through");
        assert!(
            !nowhere.exists(),
            "the dangling link's target must stay unmaterialized"
        );
    }
}
