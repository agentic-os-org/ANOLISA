//! End-to-end tests of the observability import (#6605 phase 5).
//!
//! Fixtures own their files as the requested uid when the suite runs as
//! root and as the current user otherwise, mirroring `state_migrator.rs`,
//! so the suite also passes at an unprivileged uid. Genuinely two-owner
//! scenarios express their owners through the explicit owner map
//! (production `--map-owner`) instead of `CAP_CHOWN`. Observability sources
//! are seeded with the v1 table contract (revision 1, no owner column) and
//! a `JSONL` recovery stream.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::{Path, PathBuf};

use asc_sqlite_kernel::current_epoch;
use asc_state_migrator::discovery::{DiscoveryOptions, OwnerMap};
use asc_state_migrator::import::{self, ApplyOptions};
use asc_state_migrator::journal;
use rusqlite::Connection;

const DAY: f64 = 86_400.0;

/// The uid the fixtures can actually chown files to: the requested uid when
/// the suite runs as root, otherwise the current (unprivileged) user.
fn fixture_uid(wanted: u32) -> u32 {
    if is_root() { wanted } else { current_uid() }
}

fn is_root() -> bool {
    current_uid() == 0
}

fn current_uid() -> u32 {
    let probe = tempfile::tempdir().unwrap();
    let file = probe.path().join("uid-probe");
    fs::write(&file, b"uid").unwrap();
    fs::metadata(&file).unwrap().uid()
}

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
            rusqlite::params![i64::from(fixture_uid(uid))],
        )
        .unwrap();
        drop(conn);
    }

    if !obs_jsonl.is_empty() {
        fs::write(dir.join("observability.jsonl"), obs_jsonl).unwrap();
    }

    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        chown(&path, Some(fixture_uid(uid)), None).unwrap();
        make_old(&path);
    }
    chown(dir, Some(fixture_uid(uid)), None).unwrap();
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

/// Discovery with an explicit owner map, mirroring production `--map-owner`:
/// the mapped uid is the verified owner, so genuinely two-owner scenarios
/// stay testable at an unprivileged uid (the fixtures cannot `chown` to a
/// foreign uid without `CAP_CHOWN`).
fn discovery_for_owners(
    destination: &Path,
    sources: &[PathBuf],
    owners: &[(&Path, u32)],
) -> DiscoveryOptions {
    let specs: Vec<String> = owners
        .iter()
        .map(|(dir, uid)| format!("{}={}", dir.display(), uid))
        .collect();
    let mut options = discovery_for(destination, sources);
    options.owner_map = OwnerMap::parse(&specs).expect("owner map");
    options
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
#[allow(clippy::too_many_lines)] // One cohesive end-to-end scenario; splitting would hide the flow.
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

    let options = discovery_for_owners(
        &destination,
        &[source_a.clone(), source_b.clone()],
        &[(&source_a, 1001), (&source_b, 1002)],
    );
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

    let options = discovery_for_owners(
        &destination,
        &[source_a.clone(), source_b.clone()],
        &[(&source_a, 1001), (&source_b, 1002)],
    );
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

    let options = discovery_for(&destination, std::slice::from_ref(&source));
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
    assert_eq!(fs::metadata(&db).unwrap().uid(), fixture_uid(1001));
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

    let options = discovery_for(&destination, std::slice::from_ref(&source));

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
    assert_eq!(owner, i64::from(fixture_uid(1001)));
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

    let options = discovery_for(&destination, std::slice::from_ref(&source));
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
    assert_eq!(owners[1], Some(i64::from(fixture_uid(1001))));
}

fn obs_verify_cli(destination: &Path) -> asc_state_migrator::cli::Cli {
    use asc_state_migrator::cli::{Cli, Command, CommonArgs};
    Cli {
        command: Command::Verify { run_id: None },
        common: CommonArgs {
            destination: Some(destination.to_path_buf()),
            sources: Vec::new(),
            map_owner: Vec::new(),
            discover_homes: None,
            no_discover_homes: true,
            discover_tmp: None,
            no_discover_tmp: true,
            retention_days: 30,
            observability_retention_days: 7,
            no_retention_cutoff: false,
            sqlite_only: false,
            recover_observability_jsonl: false,
            force: false,
            writer_grace: 300,
            json: false,
        },
    }
}

#[test]
fn verify_needs_an_existing_observability_destination_and_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

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
            "{\"user_input\":\"list files\"}",
            None,
        )],
        "",
        true,
    );

    let options = discovery_for(&destination, &[source]);
    import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();
    assert!(observability_destination.is_file());

    fs::remove_file(&observability_destination).unwrap();

    match asc_state_migrator::run(&obs_verify_cli(&destination)) {
        Err(asc_state_migrator::error::MigratorError::DestinationUnusable { path, reason }) => {
            assert!(path.ends_with("observability.db"), "got {path}");
            assert!(reason.contains("does not exist"), "got {reason}");
        }
        other => panic!("expected DestinationUnusable, got {other:?}"),
    }
    assert!(
        !observability_destination.exists(),
        "verify is read-only: the observability store must not reappear"
    );
    assert!(destination.is_file(), "the security store is untouched");
}

#[test]
fn verify_fails_the_exit_status_when_the_observability_store_is_corrupt() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let observability_destination = home.path().join("dest/observability.db");

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
            "{\"user_input\":\"list files\"}",
            None,
        )],
        "",
        true,
    );

    let options = discovery_for(&destination, &[source]);
    import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();

    // A rogue table on its own page lets quick_check fail while every
    // observability_events page stays intact, so only the integrity verdict
    // can fail the command.
    let conn = Connection::open(&observability_destination).unwrap();
    conn.execute_batch("CREATE TABLE rogue_probe(t); INSERT INTO rogue_probe VALUES (1);")
        .unwrap();
    let root: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'rogue_probe'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let page_size: usize = {
        let conn = Connection::open(&observability_destination).unwrap();
        let raw: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        usize::try_from(raw).unwrap()
    };
    let mut bytes = fs::read(&observability_destination).unwrap();
    let root_page = usize::try_from(root).unwrap();
    let offset = root_page.saturating_sub(1).saturating_mul(page_size);
    // An invalid b-tree page type: quick_check reports it, queries that do
    // not touch the page still succeed.
    bytes[offset] = 0;
    fs::write(&observability_destination, bytes).unwrap();

    let verification = import::verify(&destination, None).unwrap();
    assert_ne!(
        verification.observability_quick_check, "ok",
        "the corrupted observability page must be reported"
    );
    assert_eq!(
        verification.quick_check, "ok",
        "the security store is intact"
    );
    assert!(
        verification.runs.iter().all(|run| run.ok),
        "the journaled rows are still present: {:?}",
        verification.runs
    );

    match asc_state_migrator::run(&obs_verify_cli(&destination)) {
        Err(asc_state_migrator::error::MigratorError::Usage(message)) => {
            assert!(
                message.contains("integrity"),
                "the observability quick_check verdict must fail the command, got {message}"
            );
        }
        other => panic!("expected Usage error, got {other:?}"),
    }
}

#[test]
fn verify_reports_observability_sources_that_grew_in_place_after_the_run() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let jsonl = format!(
        "{{\"hook\": \"before_agent_run\", \"observedAt\": \"{}\", \"metadata\": \
         {{\"sessionId\": \"s-1\", \"runId\": \"r-1\"}}, \"metrics\": {{\"user_input\": \
         \"db row\"}}}}\n",
        iso_of(now),
    );
    let source = home.path().join("a/.agent-sec-core");
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

    let options = discovery_for(&destination, std::slice::from_ref(&source));
    import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();

    // An in-place append keeps dev/ino but grows the file and bumps mtime.
    let obs_jsonl = source.join("observability.jsonl");
    let before = fs::symlink_metadata(&obs_jsonl).unwrap();
    {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&obs_jsonl)
            .unwrap();
        file.write_all(&[0u8; 16]).unwrap();
    }
    let after = fs::symlink_metadata(&obs_jsonl).unwrap();
    assert_eq!(before.dev(), after.dev());
    assert_eq!(before.ino(), after.ino());
    assert!(after.size() > before.size());

    let verification = import::verify(&destination, None).unwrap();
    assert_eq!(verification.runs.len(), 1);
    assert!(
        verification.runs[0].sources[0]
            .observability_jsonl_unchanged
            .is_some_and(|unchanged| !unchanged),
        "an in-place append is source drift and must be reported: {:?}",
        verification.runs[0].sources
    );
    assert!(
        verification.runs[0].ok,
        "source drift is reported, not a destination mismatch"
    );
}

#[test]
fn a_fresh_observability_wal_sidecar_marks_the_source_live() {
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
            "{\"user_input\":\"list files\"}",
            None,
        )],
        "",
        false,
    );
    // The v1 observability writer shares the security store's WAL-mode
    // SqliteStore: active commits land in `observability.db-wal` while the
    // main database keeps its aged timestamp.
    let wal = source.join("observability.db-wal");
    fs::write(&wal, b"fresh writer evidence").unwrap();
    // v1 writes the sidecar as the directory's owner; every stream file the
    // migrator reads is held to that same ownership contract.
    chown(&wal, Some(fixture_uid(1001)), None).unwrap();

    let options = discovery_for(&destination, std::slice::from_ref(&source));
    let (scans, rejected, _) = import::scan_sources(&options, false, 300, current_epoch(), true);
    assert!(scans.is_empty(), "the live source must not scan");
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert!(
        rejected[0].reason.contains("stop v1 writers"),
        "grace rejection: {}",
        rejected[0].reason
    );

    // --force still reaches the data.
    let (scans, _, _) = import::scan_sources(&options, true, 300, current_epoch(), true);
    assert_eq!(scans.len(), 1);
}

#[test]
fn apply_reads_the_validated_observability_database_not_a_replacement() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

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
            "{\"user_input\":\"legit\"}",
            None,
        )],
        "",
        false,
    );

    let options = discovery_for(&destination, std::slice::from_ref(&source));
    let scans = scan_all(&options);

    // Between validation and the import passes the directory owner replaces
    // the validated observability database wholesale - with one that even
    // fails the per-file checks. The import must read the object that passed
    // validation, not the path's new tenant.
    let replacement = home.path().join("replacement-observability.db");
    {
        let conn = Connection::open(&replacement).unwrap();
        conn.execute_batch(OBS_V1_SCHEMA).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        insert_obs_row(
            &conn,
            &(
                "before_agent_run",
                now,
                "s-swapped",
                "r-swapped",
                "{\"user_input\":\"swapped in\"}",
                None,
            ),
        );
    }
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o666)).unwrap();
    fs::rename(&replacement, source.join("observability.db")).unwrap();

    import::apply(
        &scans,
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();

    let rows = observability_rows(&home.path().join("dest/observability.db"));
    assert_eq!(
        rows,
        vec![(
            "before_agent_run".to_owned(),
            "s-1".to_owned(),
            "r-1".to_owned(),
            i64::from(fixture_uid(1001))
        )],
        "the import must read the validated database, not the replacement: {rows:?}"
    );
}

#[test]
fn observability_jsonl_recovery_reads_the_validated_log_not_a_replacement() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    let jsonl = format!(
        "{{\"hook\": \"before_agent_run\", \"observedAt\": \"{}\", \"metadata\": \
         {{\"sessionId\": \"s-1\", \"runId\": \"r-1\"}}, \"metrics\": {{\"user_input\": \
         \"legit recovery\"}}}}\n",
        iso_of(now),
    );
    seed_observability_source(&source, 1001, &[], &jsonl, false);

    let options = discovery_for(&destination, std::slice::from_ref(&source));
    let scans = scan_all(&options);

    // The recovery stream is replaced after validation, this time by a
    // symlink to another user's data, which a pathname open would follow.
    let victim = home.path().join("victim-observability.jsonl");
    fs::write(
        &victim,
        format!(
            "{{\"hook\": \"before_agent_run\", \"observedAt\": \"{}\", \"metadata\": \
             {{\"sessionId\": \"s-swapped\", \"runId\": \"r-swapped\"}}, \"metrics\": \
             {{\"user_input\": \"swapped in\"}}}}\n",
            iso_of(now + 10.0),
        ),
    )
    .unwrap();
    let log = source.join("observability.jsonl");
    fs::remove_file(&log).unwrap();
    std::os::unix::fs::symlink(&victim, &log).unwrap();

    import::apply(
        &scans,
        &[],
        &destination,
        &apply_options(Some(30), Some(7), true),
    )
    .unwrap();

    let rows = observability_rows(&home.path().join("dest/observability.db"));
    assert_eq!(
        rows,
        vec![(
            "before_agent_run".to_owned(),
            "s-1".to_owned(),
            "r-1".to_owned(),
            i64::from(fixture_uid(1001))
        )],
        "recovery must read the validated log, not the replacement: {rows:?}"
    );
}

/// `--sqlite-only` must not let a damaged observability `JSONL` log reject
/// the source either: the scan skips it, and the import uses the
/// observability `SQLite` stream.
#[test]
fn obs_sqlite_only_scans_ignore_a_damaged_observability_jsonl() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

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
            "{\"user_input\":\"row\"}",
            None,
        )],
        "",
        true,
    );
    let foreign = home.path().join("foreign.jsonl");
    fs::write(&foreign, "{}\n").unwrap();
    std::os::unix::fs::symlink(&foreign, source.join("observability.jsonl")).unwrap();

    let options = discovery_for(&destination, &[source]);
    let (scans, rejected, _) = import::scan_sources(&options, true, 300, current_epoch(), false);
    assert!(
        rejected.is_empty(),
        "a sqlite-only scan must skip the observability JSONL stream: {rejected:?}"
    );
    let observability = scans[0]
        .observability
        .as_ref()
        .expect("obs streams scanned");
    assert!(
        observability.jsonl.is_none(),
        "a sqlite-only scan must not open the observability JSONL stream"
    );
    assert!(observability.sqlite.is_some());

    let mut sqlite_only = apply_options(Some(30), Some(7), false);
    sqlite_only.jsonl_recovery = false;
    import::apply(&scans, &[], &destination, &sqlite_only).unwrap();
    let rows = observability_rows(&home.path().join("dest/observability.db"));
    assert_eq!(rows.len(), 1, "the observability SQLite stream imports");
}

/// A post-run writer that commits only to the observability `WAL` keeps the
/// main observability database byte-identical, so the run must journal (and
/// verify) the `WAL` identity it snapshotted.
#[test]
fn an_observability_wal_only_append_after_the_run_reports_drift() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

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
            "{\"user_input\":\"main\"}",
            None,
        )],
        "",
        true,
    );
    // Leave un-checkpointed frames behind so the run journals the
    // observability WAL identity alongside the main database.
    {
        let conn = Connection::open(source.join("observability.db")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        insert_obs_row(
            &conn,
            &(
                "after_tool_call",
                now,
                "s-1",
                "r-1",
                "{\"user_input\":\"wal-only\"}",
                None,
            ),
        );
        // No clean close: the connection never checkpoints.
        std::mem::forget(conn);
    }
    let wal = source.join("observability.db-wal");
    chown(&wal, Some(fixture_uid(1001)), None).unwrap();
    make_old(&wal);
    make_old(&source.join("observability.db"));

    let options = discovery_for(&destination, std::slice::from_ref(&source));
    import::apply(
        &scan_all(&options),
        &[],
        &destination,
        &apply_options(Some(30), Some(7), false),
    )
    .unwrap();

    // Positive control before disturbing anything.
    let verification = import::verify(&destination, None).unwrap();
    assert_eq!(
        verification.runs[0].sources[0].observability_sqlite_wal_unchanged,
        Some(true),
        "an untouched observability WAL reports unchanged: {:?}",
        verification.runs[0].sources
    );

    // WAL-only append: the main observability database keeps its bytes.
    let before = fs::metadata(source.join("observability.db")).unwrap();
    {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new().append(true).open(&wal).unwrap();
        file.write_all(&[0u8; 32]).unwrap();
    }
    let after = fs::metadata(source.join("observability.db")).unwrap();
    assert_eq!(
        (before.size(), before.mtime()),
        (after.size(), after.mtime()),
        "the fixture must only disturb the observability WAL"
    );

    let verification = import::verify(&destination, None).unwrap();
    assert_eq!(
        verification.runs[0].sources[0].observability_sqlite_wal_unchanged,
        Some(false),
        "a WAL-only append after the run must report drift: {:?}",
        verification.runs[0].sources
    );
}
