//! End-to-end tests of the observability import (#6605 phase 5).
//!
//! Everything runs as the current user with `chown`-able files, mirroring
//! `state_migrator.rs`. Observability sources are seeded with the v1 table
//! contract (revision 1, no owner column) and a `JSONL` recovery stream.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::{Path, PathBuf};

use asc_sqlite_kernel::current_epoch;
use asc_state_migrator::discovery::{DiscoveryOptions, OwnerMap};
use asc_state_migrator::import::{self, ApplyOptions};
use asc_state_migrator::journal;
use rusqlite::Connection;

const DAY: f64 = 86_400.0;

const OBS_V1_SCHEMA: &str = "CREATE TABLE observability_events (id INTEGER NOT NULL PRIMARY \
     KEY, hook TEXT NOT NULL, observed_at TEXT NOT NULL, observed_at_epoch FLOAT NOT NULL, \
     session_id TEXT NOT NULL, run_id TEXT NOT NULL, metrics_json TEXT NOT NULL, metadata_json \
     TEXT NOT NULL, call_id TEXT, tool_call_id TEXT)";

const SEC_V1_SCHEMA: &str = "CREATE TABLE security_events (event_id TEXT NOT NULL PRIMARY KEY, \
     event_type TEXT NOT NULL, category TEXT NOT NULL, result TEXT NOT NULL DEFAULT 'succeeded', \
     timestamp TEXT NOT NULL, timestamp_epoch FLOAT NOT NULL, trace_id TEXT NOT NULL DEFAULT '', \
     pid INTEGER NOT NULL, uid INTEGER NOT NULL, session_id TEXT, run_id TEXT, call_id TEXT, \
     tool_call_id TEXT, verdict TEXT, details TEXT NOT NULL)";

fn iso_of(epoch: f64) -> String {
    asc_security_events::timestamp::epoch_to_utc_iso(epoch).unwrap()
}

/// The `observed_at` spelling a v1 writer actually stored: pydantic's JSON
/// mode renders a zero offset as `Z`, while the security-events timestamp
/// helper renders `+00:00`. Both spell the same instant; a v1 database keeps
/// the `Z` form, so the seeds do too.
fn obs_iso_of(epoch: f64) -> String {
    iso_of(epoch).replace("+00:00", "Z")
}

/// One seeded observability row: `(hook, epoch, session, run, metrics, call_id)`.
type ObsRow = (
    &'static str,
    f64,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

fn metadata_of(session: &str, run: &str) -> String {
    format!("{{\"sessionId\":\"{session}\",\"runId\":\"{run}\"}}")
}

fn insert_obs_row(conn: &Connection, row: &ObsRow) {
    let (hook, epoch, session, run, metrics, call_id) = *row;
    conn.execute(
        "INSERT INTO observability_events (hook, observed_at, observed_at_epoch, session_id, \
         run_id, metrics_json, metadata_json, call_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            hook,
            obs_iso_of(epoch),
            epoch,
            session,
            run,
            metrics,
            metadata_of(session, run),
            call_id,
        ],
    )
    .unwrap();
}

/// Ages a file beyond the writer grace window using `touch`.
fn make_old(path: &Path) {
    let status = std::process::Command::new("touch")
        .arg("-d")
        .arg("@1700000000")
        .arg(path)
        .status()
        .expect("touch runs");
    assert!(status.success(), "touch {} failed", path.display());
}

/// Seeds one v1 source directory owned by `uid` with observability streams.
fn seed_observability_source(
    dir: &Path,
    uid: u32,
    obs_rows: &[ObsRow],
    obs_jsonl: &str,
    with_security: bool,
) {
    fs::create_dir_all(dir).unwrap();
    let db = dir.join("observability.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(OBS_V1_SCHEMA).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    for row in obs_rows {
        insert_obs_row(&conn, row);
    }
    drop(conn);

    if with_security {
        let sec = dir.join("security-events.db");
        let conn = Connection::open(&sec).unwrap();
        conn.execute_batch(SEC_V1_SCHEMA).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute(
            "INSERT INTO security_events (event_id, event_type, category, result, timestamp, \
             timestamp_epoch, trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, \
             verdict, details) VALUES ('e1', 'sandbox_prehook', 'exec', 'succeeded', \
             '2026-10-01T00:00:00Z', 1790000000.0, '', 4242, ?1, NULL, NULL, NULL, NULL, NULL, \
             '{}')",
            rusqlite::params![i64::from(uid)],
        )
        .unwrap();
        drop(conn);
    }

    if !obs_jsonl.is_empty() {
        fs::write(dir.join("observability.jsonl"), obs_jsonl).unwrap();
    }

    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        chown(&path, Some(uid), None).unwrap();
        make_old(&path);
    }
    chown(dir, Some(uid), None).unwrap();
    make_old(dir);
}

fn discovery_for(destination: &Path, sources: &[PathBuf]) -> DiscoveryOptions {
    DiscoveryOptions {
        explicit: sources.to_vec(),
        owner_map: OwnerMap::default(),
        homes_root: None,
        tmp_root: None,
        destination_dir: destination.parent().unwrap().to_path_buf(),
    }
}

fn scan_all(options: &DiscoveryOptions) -> Vec<asc_state_migrator::source::SourceScan> {
    let (scans, rejected, _) = import::scan_sources(options, true, 300, current_epoch(), true);
    assert!(
        rejected.is_empty(),
        "force-scan must accept the seeded sources: {rejected:?}"
    );
    scans
}

fn apply_options(
    retention_days: Option<u32>,
    observability_retention_days: Option<u32>,
    observability_jsonl_recovery: bool,
) -> ApplyOptions {
    ApplyOptions {
        retention_days,
        jsonl_recovery: true,
        observability_retention_days,
        observability_jsonl_recovery,
        now_epoch: current_epoch(),
    }
}

/// `(hook, session, run, owner)` of every observability row, id order.
fn observability_rows(destination: &Path) -> Vec<(String, String, String, i64)> {
    let conn = Connection::open(destination).unwrap();
    conn.prepare("SELECT hook, session_id, run_id, owner FROM observability_events ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn columns_of(destination: &Path, table: &str) -> Vec<String> {
    let conn = Connection::open(destination).unwrap();
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn apply_imports_observability_under_the_verified_owner() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

    let now = current_epoch();
    let source_a = home.path().join("alice/.agent-sec-core");
    seed_observability_source(
        &source_a,
        1001,
        &[
            (
                "before_agent_run",
                now,
                "s-alice",
                "r-alice",
                "{\"user_input\":\"list files\"}",
                None,
            ),
            (
                "before_agent_run",
                now + 5.0,
                "s-alice",
                "r-alice",
                "{\"user_input\":\"read config\"}",
                Some("c-1"),
            ),
        ],
        "",
        true,
    );
    let source_b = home.path().join("bob/.agent-sec-core");
    seed_observability_source(
        &source_b,
        1002,
        &[(
            "before_agent_run",
            now,
            "s-bob",
            "r-bob",
            "{\"user_input\":\"list files\"}",
            None,
        )],
        "",
        false,
    );

    let options = discovery_for(&destination, &[source_a.clone(), source_b.clone()]);
    let report = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();

    // The security stream still imports (phase 1) and the observability
    // totals name the new stream.
    assert_eq!(report.totals.imported, 1);
    assert_eq!(report.totals.observability.imported, 3);

    // The destination is the sibling system store, with the owner column.
    assert!(observability_destination.exists());
    let columns = columns_of(&observability_destination, "observability_events");
    assert_eq!(columns.len(), 11, "the ten v1 columns plus owner");
    assert_eq!(columns.last(), Some(&"owner".to_owned()));

    // Rows land under the verified owner, content preserved.
    let rows = observability_rows(&observability_destination);
    assert_eq!(rows.len(), 3);
    for (hook, session, _run, owner) in &rows {
        let expected = if session == "s-bob" { 1002 } else { 1001 };
        assert_eq!(
            *owner, expected,
            "{hook} of {session} must carry the verified owner"
        );
    }

    // Content moves verbatim, including the optional correlation column.
    let conn = Connection::open(&observability_destination).unwrap();
    let (metrics, metadata, call_id): (String, String, Option<String>) = conn
        .query_row(
            "SELECT metrics_json, metadata_json, call_id FROM observability_events \
             WHERE session_id = 's-alice' ORDER BY id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(metrics, "{\"user_input\":\"list files\"}");
    assert_eq!(
        metadata,
        "{\"sessionId\":\"s-alice\",\"runId\":\"r-alice\"}"
    );
    assert_eq!(call_id, None);

    // The journal carries destination row ids and the run verifies.
    let records = journal::load(&journal::journal_path(&destination)).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].imported_observability_rowids.len(), 3);
    assert_eq!(
        records[0].sources[0]
            .observability
            .as_ref()
            .unwrap()
            .sqlite_rows_read,
        2
    );
    let verification = import::verify(&destination, None).unwrap();
    assert!(
        verification.runs.iter().all(|run| run.ok),
        "{verification:?}"
    );
    assert_eq!(verification.observability_quick_check, "ok");
}

#[test]
fn identical_content_under_different_owners_is_not_a_duplicate() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

    let now = current_epoch();
    let source_a = home.path().join("a/.agent-sec-core");
    seed_observability_source(
        &source_a,
        1001,
        &[(
            "before_agent_run",
            now,
            "s-shared",
            "r-shared",
            "{\"user_input\":\"same\"}",
            None,
        )],
        "",
        false,
    );
    let source_b = home.path().join("b/.agent-sec-core");
    seed_observability_source(
        &source_b,
        1002,
        &[(
            "before_agent_run",
            now,
            "s-shared",
            "r-shared",
            "{\"user_input\":\"same\"}",
            None,
        )],
        "",
        false,
    );

    let options = discovery_for(&destination, &[source_a, source_b]);
    let report = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert_eq!(
        report.totals.observability.imported, 2,
        "the owner is part of the content identity, so both users keep their row"
    );
    assert_eq!(report.totals.observability.duplicates_cross_source, 0);

    let rows = observability_rows(&observability_destination);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].3, 1001);
    assert_eq!(rows[1].3, 1002);
}

#[test]
fn observability_import_is_idempotent_across_reruns_and_rolls_back_exactly() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_observability_source(
        &source,
        1001,
        &[
            (
                "before_agent_run",
                now,
                "s-1",
                "r-1",
                "{\"user_input\":\"x\"}",
                None,
            ),
            (
                "before_agent_run",
                now + 1.0,
                "s-1",
                "r-1",
                "{\"user_input\":\"y\"}",
                None,
            ),
        ],
        "",
        true,
    );

    let options = discovery_for(&destination, &[source.clone()]);
    let first = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert_eq!(first.totals.observability.imported, 2);

    // A rerun must not duplicate: there is no stable id, so rows are matched
    // by content, owner included.
    let second = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert_eq!(second.totals.observability.imported, 0);
    assert_eq!(second.totals.observability.duplicates_existing, 2);
    assert_eq!(observability_rows(&observability_destination).len(), 2);

    // Rollback removes exactly what the runs imported, on both streams.
    let rollback = import::rollback(&destination, None, true).unwrap();
    assert_eq!(rollback.runs.len(), 2);
    assert_eq!(rollback.runs[0].observability_removed, 2);
    assert!(observability_rows(&observability_destination).is_empty());
    assert_eq!(
        sqlite_count(&destination, "security_events"),
        0,
        "the security rows of the same runs are gone too"
    );

    // Sources are untouched, ownership unchanged.
    let db = source.join("observability.db");
    assert!(db.exists());
    assert_eq!(fs::metadata(&db).unwrap().uid(), 1001);
    assert_eq!(sqlite_count(&db, "observability_events"), 2);

    let verification = import::verify(&destination, None).unwrap();
    assert!(
        verification
            .runs
            .iter()
            .all(|run| run.ok && run.rolled_back),
        "{verification:?}"
    );
}

fn sqlite_count(path: &Path, table: &str) -> u64 {
    let conn = Connection::open(path).unwrap();
    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap();
    u64::try_from(count).unwrap()
}

#[test]
fn observability_jsonl_recovery_is_explicit_and_fills_gaps() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    // The JSONL stream carries the SQLite row (dual write) plus one gap row
    // and one malformed tail line.
    let jsonl = format!(
        "{{\"hook\": \"before_agent_run\", \"observedAt\": \"{}\", \"metadata\": \
         {{\"sessionId\": \"s-1\", \"runId\": \"r-1\"}}, \"metrics\": {{\"user_input\": \"db \
         row\"}}}}\n{{\"hook\": \"before_agent_run\", \"observedAt\": \"{}\", \"metadata\": \
         {{\"sessionId\": \"s-1\", \"runId\": \"r-1\"}}, \"metrics\": {{\"user_input\": \"jsonl \
         only\"}}}}\n{{\"broken\": \n",
        iso_of(now),
        iso_of(now + 10.0),
    );
    seed_observability_source(
        &source,
        1001,
        &[(
            "before_agent_run",
            now,
            "s-1",
            "r-1",
            "{\"user_input\":\"db row\"}",
            None,
        )],
        &jsonl,
        false,
    );

    let options = discovery_for(&destination, &[source.clone()]);

    // Default: SQLite is the source; JSONL recovery is opt-in.
    let without_recovery = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert_eq!(without_recovery.totals.observability.imported, 1);
    assert_eq!(
        observability_rows(&observability_destination).len(),
        1,
        "the JSONL-only row must wait for the explicit recovery pass"
    );

    // Explicit recovery fills the gap and counts the malformed tail.
    let with_recovery = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), true),
    )
    .unwrap();
    assert_eq!(
        with_recovery.totals.observability.imported, 1,
        "the gap row"
    );
    assert_eq!(with_recovery.totals.observability.duplicates_existing, 1);
    assert_eq!(with_recovery.totals.observability.malformed_jsonl, 1);
    assert_eq!(observability_rows(&observability_destination).len(), 2);

    // The recovered row carries the verified owner and the record's own
    // serialization of metrics and metadata.
    let conn = Connection::open(&observability_destination).unwrap();
    let (metrics, owner): (String, i64) = conn
        .query_row(
            "SELECT metrics_json, owner FROM observability_events \
             ORDER BY id LIMIT 1 OFFSET 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(metrics, "{\"user_input\":\"jsonl only\"}");
    assert_eq!(owner, 1001);
}

#[test]
fn observability_retention_uses_its_own_window() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_observability_source(
        &source,
        1001,
        &[
            (
                "before_agent_run",
                now - 10.0 * DAY,
                "s-old",
                "r-old",
                "{\"user_input\":\"old\"}",
                None,
            ),
            (
                "before_agent_run",
                now,
                "s-new",
                "r-new",
                "{\"user_input\":\"new\"}",
                None,
            ),
        ],
        "",
        false,
    );

    let options = discovery_for(&destination, &[source]);

    // Default window: 7 days drops the 10-day-old observability row.
    let windowed = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert_eq!(windowed.totals.observability.imported, 1);
    assert_eq!(windowed.totals.observability.retention_skipped, 1);

    // Disabling the cutoff centralizes archival rows too.
    let uncut = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), None, false),
    )
    .unwrap();
    assert_eq!(
        uncut.totals.observability.imported, 1,
        "the old row imports now"
    );
    assert_eq!(observability_rows(&observability_destination).len(), 2);
}

#[test]
fn plan_counts_observability_streams_without_writing() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_observability_source(
        &source,
        1001,
        &[(
            "before_agent_run",
            now,
            "s-1",
            "r-1",
            "{\"user_input\":\"x\"}",
            None,
        )],
        "{\"hook\": \"before_agent_run\", \"observedAt\": \"2026-10-01T00:00:00Z\", \
         \"metadata\": {\"sessionId\": \"s-1\", \"runId\": \"r-1\"}, \"metrics\": \
         {\"user_input\": \"y\"}}}\n",
        false,
    );

    let options = discovery_for(&destination, &[source]);
    let report = import::plan(&options, &destination, Some(30), true, 300, true).unwrap();
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].observability_sqlite_rows, Some(1));
    assert_eq!(report.sources[0].observability_schema, Some(1));
    assert_eq!(report.sources[0].observability_jsonl_records, Some(1));
    assert_eq!(
        report.observability_destination,
        home.path()
            .join("dest/observability.db")
            .display()
            .to_string()
    );
    assert!(
        !destination.exists() && !home.path().join("dest/observability.db").exists(),
        "plan never creates either destination"
    );
}

#[test]
fn an_untrusted_observability_file_rejects_the_source() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_observability_source(
        &source,
        1001,
        &[(
            "before_agent_run",
            now,
            "s-1",
            "r-1",
            "{\"user_input\":\"x\"}",
            None,
        )],
        "",
        true,
    );

    let db = source.join("observability.db");
    let mut perms = fs::metadata(&db).unwrap().permissions();
    perms.set_mode(0o666);
    fs::set_permissions(&db, perms).unwrap();

    let options = discovery_for(&destination, &[source.clone()]);
    let (scans, rejected, _) = import::scan_sources(&options, true, 300, current_epoch(), true);
    assert!(scans.is_empty(), "the whole source is untrusted");
    assert!(
        rejected[0].reason.contains("world-writable"),
        "{}",
        rejected[0].reason
    );
}

#[test]
fn a_source_with_only_observability_streams_migrates() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

    let now = current_epoch();
    let source = home.path().join("only-obs/.agent-sec-core");
    seed_observability_source(
        &source,
        1001,
        &[(
            "before_agent_run",
            now,
            "s-1",
            "r-1",
            "{\"user_input\":\"x\"}",
            None,
        )],
        "",
        false,
    );

    let options = discovery_for(&destination, &[source]);
    let report = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert_eq!(report.totals.observability.imported, 1);
    assert_eq!(report.totals.imported, 0, "no security stream to import");
    assert_eq!(observability_rows(&observability_destination).len(), 1);

    let verification = import::verify(&destination, None).unwrap();
    assert!(
        verification.runs.iter().all(|run| run.ok),
        "{verification:?}"
    );
}

#[test]
fn a_v1_shaped_observability_destination_converges_without_losing_rows() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    // A pre-existing v1-shaped system observability store (for example left
    // by an earlier deployment): revision 1, no owner column, one row.
    let observability_destination = home.path().join("dest/observability.db");
    {
        let conn = Connection::open(&observability_destination).unwrap();
        conn.execute_batch(OBS_V1_SCHEMA).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        insert_obs_row(
            &conn,
            &(
                "before_agent_run",
                1_790_000_000.0,
                "s-pre",
                "r-pre",
                "{\"user_input\":\"pre\"}",
                None,
            ),
        );
        drop(conn);
    }

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_observability_source(
        &source,
        1001,
        &[(
            "before_agent_run",
            now,
            "s-1",
            "r-1",
            "{\"user_input\":\"x\"}",
            None,
        )],
        "",
        false,
    );

    let options = discovery_for(&destination, &[source]);
    let report = import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(None, None, false),
    )
    .unwrap();
    assert_eq!(report.totals.observability.imported, 1);

    let conn = Connection::open(&observability_destination).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        version,
        asc_persistence_sqlite::observability::SYSTEM_OBSERVABILITY_SQLITE_SCHEMA_VERSION
    );
    let owners: Vec<Option<i64>> = conn
        .prepare("SELECT owner FROM observability_events ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(owners.len(), 2, "the pre-existing row survived convergence");
    assert_eq!(owners[0], None, "the v1 row keeps its NULL owner");
    assert_eq!(owners[1], Some(1001));
}
