//! Frozen v1 fixtures: schema drift and migration regressions, without Python.
//!
//! Each fixture is a database as some past release wrote it, paired with the
//! projection v1 produces after upgrading it. This suite hands the fixture to v2
//! and requires the same projection back.
//!
//! Why this exists alongside `make test-db-compat`: the live matrix is the
//! stronger check but needs a working v1 environment, so it cannot run in a
//! Rust-only CI job or on a machine without `uv`. These fixtures cover the part
//! that matters most for a data migration — the upgrade path — with `cargo test`
//! alone.
//!
//! The projection below intentionally mirrors `dump-schema` in
//! `asc-event-sink/examples/db_probe.rs`. Two small `PRAGMA` dumps are cheaper
//! than a shared crate for test-only code, and if they ever drift this suite
//! fails loudly against the committed oracle rather than silently passing.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asc_observability::OBSERVABILITY_SQLITE_SCHEMA_VERSION;
use asc_persistence_sqlite::observability::OBSERVABILITY_TABLES;
use asc_persistence_sqlite::security_events::{
    EventFilters, SECURITY_EVENTS_TABLES, SecurityEventsMigrator, SqliteEventReader,
};
use asc_security_events::{SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, SecurityEvent};
use asc_sqlite_kernel::{SchemaMigrator, SqliteStore, TableSpec};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1")
}

/// Copies a fixture into a scratch directory so the committed file stays pristine.
fn staged(name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let target = dir.path().join(format!("{name}.db"));
    fs::copy(fixture_dir().join(format!("{name}.db")), &target).expect("copy fixture");
    (dir, target)
}

fn expected(name: &str, suffix: &str) -> Value {
    let path = fixture_dir().join(format!("{name}.{suffix}.json"));
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}; rerun scripts/gen-v1-db-fixtures.sh",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("the oracle is valid JSON")
}

/// Opens the fixture read-write, which is what triggers schema convergence.
fn open_security(path: &Path) -> SqliteStore {
    SqliteStore::new(
        path,
        false,
        SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
        SECURITY_EVENTS_TABLES,
        Some(Arc::new(SecurityEventsMigrator) as Arc<dyn SchemaMigrator>),
        "[security_events]",
    )
    .expect("store")
}

fn open_observability(path: &Path) -> SqliteStore {
    SqliteStore::new(
        path,
        false,
        OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        OBSERVABILITY_TABLES,
        None,
        "[observability]",
    )
    .expect("store")
}

// --------------------------------------------------------------------------
// projection
// --------------------------------------------------------------------------

fn project(conn: &Connection, tables: &[TableSpec]) -> Value {
    let projected: Vec<Value> = tables
        .iter()
        .map(|table| {
            json!({
                "name": table.name,
                "columns": columns_of(conn, table.name),
                "indexes": indexes_of(conn, table.name),
            })
        })
        .collect();
    json!({
        "exists": true,
        "tables": projected,
        "user_version": scalar::<u32>(conn, "PRAGMA user_version"),
        "auto_vacuum": scalar::<u32>(conn, "PRAGMA auto_vacuum"),
        "journal_mode": scalar::<String>(conn, "PRAGMA journal_mode"),
        "sqlite_master": master_names(conn),
    })
}

fn columns_of(conn: &Connection, table: &str) -> Vec<Value> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("table_info");
    statement
        .query_map([], |row| {
            Ok(json!({
                "cid": row.get::<_, i64>(0)?,
                "name": row.get::<_, String>(1)?,
                "type": row.get::<_, String>(2)?,
                "notnull": row.get::<_, i64>(3)?,
                "dflt_value": row.get::<_, Option<String>>(4)?,
                "pk": row.get::<_, i64>(5)?,
            }))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

fn indexes_of(conn: &Connection, table: &str) -> Vec<Value> {
    let mut statement = conn
        .prepare(&format!("PRAGMA index_list({table})"))
        .expect("index_list");
    let listed = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");

    let mut indexes: Vec<Value> = listed
        .into_iter()
        .map(|(name, unique, origin)| {
            let mut info = conn
                .prepare(&format!("PRAGMA index_info({name})"))
                .expect("index_info");
            let mut columns = info
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(2)?))
                })
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows");
            columns.sort_by_key(|(seqno, _)| *seqno);
            json!({
                "name": name,
                "unique": unique,
                "origin": origin,
                "columns": columns.into_iter().map(|(_, name)| name).collect::<Vec<_>>(),
            })
        })
        .collect();
    indexes.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    indexes
}

fn scalar<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |row| row.get::<_, T>(0))
        .expect("pragma")
}

fn master_names(conn: &Connection) -> Vec<String> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master ORDER BY name")
        .expect("sqlite_master");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

fn row_projection(event: &SecurityEvent) -> Value {
    let mut value = Map::new();
    value.insert("event_id".to_owned(), json!(event.event_id));
    value.insert("event_type".to_owned(), json!(event.event_type));
    value.insert("category".to_owned(), json!(event.category));
    value.insert(
        "result".to_owned(),
        serde_json::to_value(event.result).expect("result"),
    );
    value.insert("timestamp".to_owned(), json!(event.timestamp));
    value.insert("trace_id".to_owned(), json!(event.trace_id));
    value.insert("session_id".to_owned(), json!(event.session_id));
    value.insert("run_id".to_owned(), json!(event.run_id));
    value.insert("call_id".to_owned(), json!(event.call_id));
    value.insert("tool_call_id".to_owned(), json!(event.tool_call_id));
    value.insert("details".to_owned(), Value::Object(event.details.clone()));
    Value::Object(value)
}

fn read_all(path: &Path) -> Value {
    let reader = SqliteEventReader::new(path).expect("reader");
    let events = reader.query(&EventFilters::default(), 1_000_000, 0);
    Value::Array(events.iter().map(row_projection).collect())
}

/// Upgrades a security fixture and compares both oracles.
fn assert_security_fixture(name: &str) {
    let (_dir, path) = staged(name);
    let store = open_security(&path);
    let projected = store
        .with_connection(false, |conn| Ok(project(conn, SECURITY_EVENTS_TABLES)))
        .expect("connection")
        .expect("store not disabled");

    assert_eq!(
        projected,
        expected(name, "expected"),
        "{name}: the upgraded schema differs from what v1 produces"
    );
    assert_eq!(
        read_all(&path),
        expected(name, "expected-rows"),
        "{name}: the rows read back differ from what v1 reads"
    );
}

// --------------------------------------------------------------------------
// the suite
// --------------------------------------------------------------------------

#[test]
fn a_revision_one_database_converges_to_the_current_schema() {
    // No run_id/call_id/tool_call_id/verdict: the generic column convergence path.
    assert_security_fixture("security_events_v1");
}

#[test]
fn a_revision_two_database_gains_a_backfilled_verdict() {
    assert_security_fixture("security_events_v2");
}

#[test]
fn adversarial_details_are_skipped_without_stalling_the_backfill() {
    assert_security_fixture("security_events_v2_mixed");

    // The oracle above pins the read path; this pins the column the backfill
    // wrote, which the reader does not surface. Only the first row carries an
    // extractable verdict — the array, the malformed text and the empty string
    // must be left NULL, and the batch cursor must still have advanced past them
    // or this test would never return.
    let (_dir, path) = staged("security_events_v2_mixed");
    let store = open_security(&path);
    let verdicts = store
        .with_connection(false, |conn| {
            let mut statement =
                conn.prepare("SELECT event_id, verdict FROM security_events ORDER BY event_id")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .expect("connection")
        .expect("store not disabled");

    assert_eq!(
        verdicts,
        vec![
            ("mix-1".to_owned(), Some("deny".to_owned())),
            ("mix-2".to_owned(), None),
            ("mix-3".to_owned(), None),
            ("mix-4".to_owned(), None),
            ("mix-5".to_owned(), None),
        ]
    );
}

#[test]
fn a_newer_schema_version_is_left_untouched() {
    let (_dir, path) = staged("security_events_v4_future");
    let before = fs::read(&path).expect("read fixture");

    let store = open_security(&path);
    let projected = store
        .with_connection(false, |conn| Ok(project(conn, SECURITY_EVENTS_TABLES)))
        .expect("connection")
        .expect("store not disabled");

    assert_eq!(
        projected,
        expected("security_events_v4_future", "expected"),
        "a database from a newer release must keep its own schema"
    );
    assert_eq!(
        projected["user_version"], 4,
        "the version must not be rewritten downwards"
    );
    drop(store);

    // Byte equality is the strict form of "left untouched". It holds because
    // nothing wrote: no `ALTER TABLE`, no `PRAGMA user_version`, and the
    // read-mostly open leaves the header alone.
    assert_eq!(
        fs::read(&path).expect("read fixture"),
        before,
        "opening a future database must not modify the file"
    );
}

#[test]
fn an_observability_fixture_keeps_its_only_revision() {
    let (_dir, path) = staged("observability_v1");
    let store = open_observability(&path);
    let projected = store
        .with_connection(false, |conn| Ok(project(conn, OBSERVABILITY_TABLES)))
        .expect("connection")
        .expect("store not disabled");

    assert_eq!(projected, expected("observability_v1", "expected"));
}
