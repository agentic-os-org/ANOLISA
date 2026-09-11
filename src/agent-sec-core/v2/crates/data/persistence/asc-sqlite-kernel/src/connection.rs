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
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?
    } else {
        Connection::open(path)?
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
}
