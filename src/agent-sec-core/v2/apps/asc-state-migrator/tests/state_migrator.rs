//! End-to-end tests of the state migrator over real source directories and a
//! real destination store.
//!
//! Everything runs as the current user with `chown`-able files, which the v2
//! test environment provides. Source databases are seeded with the v1 table
//! contract, including an older revision without the correlation columns.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::{Path, PathBuf};

use asc_security_events::SecurityEvent;
use asc_sqlite_kernel::current_epoch;
use asc_state_migrator::discovery::{DiscoveryOptions, OwnerMap};
use asc_state_migrator::error::MigratorError;
use asc_state_migrator::import::{self, ApplyOptions};
use asc_state_migrator::journal;
use rusqlite::Connection;

const DAY: f64 = 86_400.0;

fn full_v1_schema() -> String {
    "CREATE TABLE security_events (event_id TEXT NOT NULL PRIMARY KEY, event_type TEXT NOT \
     NULL, category TEXT NOT NULL, result TEXT NOT NULL DEFAULT 'succeeded', timestamp TEXT \
     NOT NULL, timestamp_epoch FLOAT NOT NULL, trace_id TEXT NOT NULL DEFAULT '', pid INTEGER \
     NOT NULL, uid INTEGER NOT NULL, session_id TEXT, run_id TEXT, call_id TEXT, tool_call_id \
     TEXT, verdict TEXT, details TEXT NOT NULL)"
        .to_owned()
}

fn old_v1_schema() -> String {
    "CREATE TABLE security_events (event_id TEXT NOT NULL PRIMARY KEY, event_type TEXT NOT \
     NULL, category TEXT NOT NULL, result TEXT NOT NULL DEFAULT 'succeeded', timestamp TEXT \
     NOT NULL, timestamp_epoch FLOAT NOT NULL, pid INTEGER NOT NULL, uid INTEGER NOT NULL, \
     details TEXT NOT NULL)"
        .to_owned()
}

fn iso_of(epoch: f64) -> String {
    asc_security_events::timestamp::epoch_to_utc_iso(epoch).unwrap()
}

fn insert_source_row(conn: &Connection, id: &str, epoch: f64, recorded_uid: i64) {
    conn.execute(
        "INSERT INTO security_events (event_id, event_type, category, result, timestamp, \
         timestamp_epoch, trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, \
         verdict, details) VALUES (?1, 'sandbox_prehook', 'exec', 'succeeded', ?2, ?3, '', \
         4242, ?4, NULL, NULL, NULL, NULL, NULL, '{\"k\": \"v\"}')",
        rusqlite::params![id, iso_of(epoch), epoch, recorded_uid],
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

/// Seeds one v1 source directory owned by `uid`.
fn seed_source(
    dir: &Path,
    uid: u32,
    db_rows: &[(&str, f64, i64)],
    jsonl_events: &[SecurityEvent],
    old_schema: bool,
) {
    fs::create_dir_all(dir).unwrap();
    let db = dir.join("security-events.db");
    let conn = Connection::open(&db).unwrap();
    let schema = if old_schema {
        old_v1_schema()
    } else {
        full_v1_schema()
    };
    conn.execute_batch(&schema).unwrap();
    conn.pragma_update(None, "user_version", if old_schema { 2 } else { 3 })
        .unwrap();
    for (id, epoch, recorded_uid) in db_rows {
        if old_schema {
            conn.execute(
                "INSERT INTO security_events (event_id, event_type, category, result, \
                 timestamp, timestamp_epoch, pid, uid, details) VALUES (?1, 'sandbox_prehook', \
                 'exec', 'succeeded', ?2, ?3, 4242, ?4, '{\"k\": \"v\"}')",
                rusqlite::params![id, iso_of(*epoch), epoch, recorded_uid],
            )
            .unwrap();
        } else {
            insert_source_row(&conn, id, *epoch, *recorded_uid);
        }
    }
    drop(conn);

    if !jsonl_events.is_empty() {
        let jsonl = dir.join("security-events.jsonl");
        let mut content = String::new();
        for event in jsonl_events {
            content.push_str(&serde_json::to_string(event).unwrap());
            content.push('\n');
        }
        fs::write(&jsonl, content).unwrap();
    }

    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        chown(&path, Some(uid), None).unwrap();
        make_old(&path);
    }
    chown(dir, Some(uid), None).unwrap();
    make_old(dir);
}

fn jsonl_event(id: &str, epoch: f64, recorded_uid: u32) -> SecurityEvent {
    let mut event = SecurityEvent::new("sandbox_prehook", "exec", serde_json::Map::new());
    id.clone_into(&mut event.event_id);
    event.timestamp = iso_of(epoch);
    event.pid = 4242;
    event.uid = recorded_uid;
    event
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
    let (scans, rejected, _) = import::scan_sources(options, true, 300, current_epoch());
    assert!(
        rejected.is_empty(),
        "force-scan must accept the seeded sources: {rejected:?}"
    );
    scans
}

fn apply_options(retention_days: Option<u32>) -> ApplyOptions {
    ApplyOptions {
        retention_days,
        jsonl_recovery: true,
        now_epoch: current_epoch(),
    }
}

fn destination_row_uids(destination: &Path) -> Vec<(String, i64)> {
    let conn = Connection::open(destination).unwrap();
    conn.prepare("SELECT event_id, uid FROM security_events ORDER BY event_id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn apply_imports_under_the_verified_owner_and_journals_the_run() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source_a = home.path().join("alice/.agent-sec-core");
    seed_source(
        &source_a,
        1001,
        &[("a1", now, 1001), ("a2", now, 9999)],
        &[],
        false,
    );
    let source_b = home.path().join("bob/.agent-sec-core");
    seed_source(&source_b, 1002, &[("b1", now, 1002)], &[], false);

    let options = discovery_for(&destination, &[source_a.clone(), source_b.clone()]);
    let scans = scan_all(&options);
    assert_eq!(scans.len(), 2);

    let report = import::apply(&scans, &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(report.totals.imported, 3);
    assert_eq!(report.totals.uid_conflicts, 1, "a2 records uid 9999");

    let rows = destination_row_uids(&destination);
    assert_eq!(rows.len(), 3);
    for (id, uid) in &rows {
        let expected = if id.starts_with('a') { 1001 } else { 1002 };
        assert_eq!(*uid, expected, "row {id} must carry the verified owner");
    }

    // Sources are untouched and the journal verifies.
    let records = journal::load(&journal::journal_path(&destination)).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, report.run_id);
    assert_eq!(records[0].imported_event_ids.len(), 3);
    let verification = import::verify(&destination, None).unwrap();
    assert!(
        verification.runs.iter().all(|run| run.ok),
        "{verification:?}"
    );
    assert_eq!(verification.quick_check, "ok");
}

#[test]
fn apply_deduplicates_by_event_id_across_sources_and_reruns() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source_a = home.path().join("a/.agent-sec-core");
    seed_source(
        &source_a,
        1001,
        &[("shared", now, 1001), ("only-a", now, 1001)],
        &[],
        false,
    );
    let source_b = home.path().join("b/.agent-sec-core");
    seed_source(
        &source_b,
        1002,
        &[("shared", now, 1002), ("only-b", now, 1002)],
        &[],
        false,
    );

    let options = discovery_for(&destination, &[source_a.clone(), source_b.clone()]);
    let first = import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(first.totals.imported, 3, "three distinct ids");
    assert_eq!(
        first.totals.duplicates_cross_source, 1,
        "the second source's 'shared' is a cross-source duplicate"
    );

    let second =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(second.totals.imported, 0, "the rerun imports nothing");
    assert_eq!(second.totals.duplicates_existing, 3);

    let records = journal::load(&journal::journal_path(&destination)).unwrap();
    assert_eq!(records.len(), 2);

    let rollback = import::rollback(&destination, None, true).unwrap();
    assert_eq!(rollback.runs.len(), 2);
    assert!(
        destination_row_uids(&destination).is_empty(),
        "everything is gone"
    );
    for source in [&source_a, &source_b] {
        assert!(
            source.join("security-events.db").exists(),
            "rollback never touches sources"
        );
        let uid = fs::metadata(source.join("security-events.db"))
            .unwrap()
            .uid();
        assert!(uid == 1001 || uid == 1002, "source ownership unchanged");
    }
}

#[test]
fn apply_fills_jsonl_gaps_and_tolerates_malformed_lines() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    let in_both = jsonl_event("in-both", now, 1001);
    let jsonl_only = jsonl_event("jsonl-only", now, 1001);
    seed_source(
        &source,
        1001,
        &[("in-both", now, 1001)],
        &[in_both.clone(), jsonl_only.clone()],
        false,
    );
    // A crash-truncated tail must not stop the recovery pass.
    let jsonl = source.join("security-events.jsonl");
    fs::write(
        &jsonl,
        format!(
            "{}\n{}\n{{\"broken\": \n",
            serde_json::to_string(&in_both).unwrap(),
            serde_json::to_string(&jsonl_only).unwrap()
        ),
    )
    .unwrap();
    chown(&jsonl, Some(1001), None).unwrap();
    make_old(&jsonl);

    let options = discovery_for(&destination, &[source]);
    let report =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(report.totals.imported, 2, "db row + jsonl gap");
    assert_eq!(report.totals.malformed_jsonl, 1);
    let ids: Vec<String> = destination_row_uids(&destination)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(ids.contains(&"jsonl-only".to_owned()));
}

#[test]
fn retention_cuts_at_import_time_and_can_be_disabled() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(
        &source,
        1001,
        &[("old", now - 40.0 * DAY, 1001), ("fresh", now, 1001)],
        &[],
        false,
    );

    let options = discovery_for(&destination, &[source.clone()]);
    let with_cutoff =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(with_cutoff.totals.imported, 1);
    assert_eq!(with_cutoff.totals.retention_skipped, 1);

    let without_cutoff =
        import::apply(&scan_all(&options), &destination, &apply_options(None)).unwrap();
    assert_eq!(without_cutoff.totals.imported, 1, "the old row imports now");
    assert_eq!(without_cutoff.totals.retention_skipped, 0);
    assert_eq!(destination_row_uids(&destination).len(), 2);
}

#[test]
fn recent_source_writes_are_refused_without_force() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);
    // Re-write the db so its mtime is right now.
    fs::write(
        source.join("security-events.db"),
        fs::read(source.join("security-events.db")).unwrap(),
    )
    .unwrap();

    let options = discovery_for(&destination, &[source]);
    let (scans, rejected, _) = import::scan_sources(&options, false, 300, current_epoch());
    assert!(scans.is_empty());
    assert!(rejected.len() == 1);
    assert!(
        rejected[0].reason.contains("--force"),
        "the remedy must be in the message: {}",
        rejected[0].reason
    );

    let (forced, _, _) = import::scan_sources(&options, true, 300, current_epoch());
    assert_eq!(forced.len(), 1, "--force accepts the fresh source");
}

#[test]
fn world_writable_and_hardlinked_stream_files_are_rejected() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    let db = source.join("security-events.db");
    let mut perms = fs::metadata(&db).unwrap().permissions();
    perms.set_mode(0o666);
    fs::set_permissions(&db, perms).unwrap();

    let options = discovery_for(&destination, &[source.clone()]);
    let (scans, rejected, _) = import::scan_sources(&options, true, 300, current_epoch());
    assert!(scans.is_empty());
    assert!(rejected[0].reason.contains("world-writable"));

    // Repair the mode, then add a hard link.
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&db, home.path().join("hardlink.db")).unwrap();

    let (scans, rejected, _) = import::scan_sources(&options, true, 300, current_epoch());
    assert!(scans.is_empty());
    assert!(rejected[0].reason.contains("hard links"));
}

#[test]
fn the_destination_directory_is_never_a_source() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let options = DiscoveryOptions {
        explicit: vec![destination.parent().unwrap().to_path_buf()],
        owner_map: OwnerMap::default(),
        homes_root: Some(home.path().to_path_buf()),
        tmp_root: None,
        destination_dir: destination.parent().unwrap().to_path_buf(),
    };
    let discovery = asc_state_migrator::discovery::discover(&options);
    assert!(discovery.sources.is_empty());
    assert_eq!(
        discovery.system_owned.len(),
        1,
        "the destination is skipped"
    );
}

#[test]
fn admin_owner_mapping_overrides_directory_ownership() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    assert!(
        OwnerMap::parse(&[format!("{}=2000", source.display())])
            .unwrap()
            .owner_of(&source)
            .is_some()
    );
    let options = DiscoveryOptions {
        explicit: vec![source.clone()],
        owner_map: OwnerMap::parse(&[format!("{}=2000", source.display())]).unwrap(),
        homes_root: None,
        tmp_root: None,
        destination_dir: destination.parent().unwrap().to_path_buf(),
    };
    let scans = scan_all(&options);
    assert_eq!(scans[0].owner_uid, 2000);
    assert!(scans[0].admin_mapped);

    let report = import::apply(&scans, &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(
        report.totals.uid_conflicts, 1,
        "recorded 1001 vs mapped 2000"
    );
    assert_eq!(destination_row_uids(&destination)[0].1, 2000);
}

#[test]
fn older_revisions_import_with_null_correlation_columns() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("rev2", now, 1001)], &[], true);

    let options = discovery_for(&destination, &[source]);
    let report =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(report.totals.imported, 1);

    let conn = Connection::open(&destination).unwrap();
    let (session_id, verdict, details): (Option<String>, Option<String>, String) = conn
        .query_row(
            "SELECT session_id, verdict, details FROM security_events WHERE event_id = 'rev2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(session_id, None);
    assert_eq!(verdict, None);
    assert_eq!(details, "{\"k\": \"v\"}", "details move verbatim");
}

#[test]
fn rollback_is_exact_once_per_run_and_reports_unknown_ids() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    let options = discovery_for(&destination, &[source]);
    let report =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(destination_row_uids(&destination).len(), 1);

    import::rollback(&destination, Some(&report.run_id), false).unwrap();
    assert!(destination_row_uids(&destination).is_empty());

    match import::rollback(&destination, Some(&report.run_id), false) {
        Err(MigratorError::AlreadyRolledBack(id)) => assert_eq!(id, report.run_id),
        other => panic!("expected AlreadyRolledBack, got {other:?}"),
    }
    match import::rollback(&destination, Some("does-not-exist"), false) {
        Err(MigratorError::RunNotFound(id)) => assert_eq!(id, "does-not-exist"),
        other => panic!("expected RunNotFound, got {other:?}"),
    }
    assert!(import::rollback(&destination, None, false).is_err());
}

#[test]
fn verify_detects_rows_that_vanished_after_the_run() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(
        &source,
        1001,
        &[("e1", now, 1001), ("e2", now, 1001)],
        &[],
        false,
    );

    let options = discovery_for(&destination, &[source]);
    import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();

    let conn = Connection::open(&destination).unwrap();
    conn.execute("DELETE FROM security_events WHERE event_id = 'e1'", [])
        .unwrap();
    drop(conn);

    let verification = import::verify(&destination, None).unwrap();
    assert_eq!(verification.runs.len(), 1);
    assert!(
        !verification.runs[0].ok,
        "the vanished row must be reported"
    );
    assert_eq!(verification.runs[0].present, 1);
    assert_eq!(verification.runs[0].expected_present, 2);
}

#[test]
fn plan_counts_without_touching_the_destination() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    let options = discovery_for(&destination, &[source]);
    let report = import::plan(&options, &destination, Some(30), true, 300).unwrap();
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].sqlite_rows, Some(1));
    assert!(!destination.exists(), "plan never creates the destination");
}

#[test]
fn apply_creates_and_converges_a_missing_destination() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("brand/new/security-events.db");

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    let options = discovery_for(&destination, &[source]);
    let report =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(report.totals.imported, 1);
    assert!(destination.exists());
    let dir_mode = fs::metadata(destination.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "the created destination dir is private");
}

fn verify_cli(destination: &Path) -> asc_state_migrator::cli::Cli {
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
            no_retention_cutoff: false,
            sqlite_only: false,
            force: false,
            writer_grace: 300,
            json: false,
        },
    }
}

#[test]
fn verify_needs_an_existing_destination_and_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");

    match asc_state_migrator::run(&verify_cli(&destination)) {
        Err(MigratorError::DestinationUnusable { path, reason }) => {
            assert!(path.ends_with("security-events.db"), "got {path}");
            assert!(reason.contains("does not exist"), "got {reason}");
        }
        other => panic!("expected DestinationUnusable, got {other:?}"),
    }
    assert!(
        !destination.parent().unwrap().exists(),
        "verify is read-only: neither the database nor its directory may appear"
    );
}

#[test]
fn verify_fails_the_exit_status_when_quick_check_reports_corruption() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    let options = discovery_for(&destination, &[source]);
    import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();

    // A rogue table on its own page lets quick_check fail while every
    // security_events page stays intact, so only the integrity verdict can
    // fail the command.
    let conn = Connection::open(&destination).unwrap();
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
        let conn = Connection::open(&destination).unwrap();
        let raw: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        usize::try_from(raw).unwrap()
    };
    let mut bytes = fs::read(&destination).unwrap();
    let root_page = usize::try_from(root).unwrap();
    let offset = root_page.saturating_sub(1).saturating_mul(page_size);
    // An invalid b-tree page type: quick_check reports it, queries that do
    // not touch the page still succeed.
    bytes[offset] = 0;
    fs::write(&destination, bytes).unwrap();

    let verification = import::verify(&destination, None).unwrap();
    assert_ne!(
        verification.quick_check, "ok",
        "the corrupted page must be reported"
    );
    assert!(
        verification.runs.iter().all(|run| run.ok),
        "the journaled rows are still present: {:?}",
        verification.runs
    );

    match asc_state_migrator::run(&verify_cli(&destination)) {
        Err(MigratorError::Usage(message)) => {
            assert!(
                message.contains("integrity"),
                "the quick_check verdict must fail the command, got {message}"
            );
        }
        other => panic!("expected Usage error, got {other:?}"),
    }
}

#[test]
fn verify_reports_sources_that_grew_in_place_after_the_run() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(&source, 1001, &[("e1", now, 1001)], &[], false);

    let options = discovery_for(&destination, &[source.clone()]);
    import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();

    // An in-place append keeps dev/ino but grows the file and bumps mtime.
    let source_db = source.join("security-events.db");
    let before = fs::symlink_metadata(&source_db).unwrap();
    {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&source_db)
            .unwrap();
        file.write_all(&[0u8; 16]).unwrap();
    }
    let after = fs::symlink_metadata(&source_db).unwrap();
    assert_eq!(before.dev(), after.dev());
    assert_eq!(before.ino(), after.ino());
    assert!(after.size() > before.size());

    let verification = import::verify(&destination, None).unwrap();
    assert_eq!(verification.runs.len(), 1);
    assert!(
        verification.runs[0].sources[0]
            .sqlite_unchanged
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
fn bare_verify_checks_the_latest_run_only() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("dest/security-events.db");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();

    let now = current_epoch();
    let source = home.path().join("a/.agent-sec-core");
    seed_source(
        &source,
        1001,
        &[("e1", now, 1001), ("e2", now, 1001)],
        &[],
        false,
    );

    let options = discovery_for(&destination, &[source.clone()]);
    let first = import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(first.totals.imported, 2);

    // Rescan after removing e2 and adding e3: the second run's responsibility
    // set is {e1, e3} — it never covers e2.
    let conn = Connection::open(source.join("security-events.db")).unwrap();
    conn.execute("DELETE FROM security_events WHERE event_id = 'e2'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO security_events (event_id, event_type, category, result, timestamp, \
         timestamp_epoch, trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, \
         verdict, details) VALUES ('e3', 'sandbox_prehook', 'exec', 'succeeded', ?1, ?2, '', \
         4242, 1001, NULL, NULL, NULL, NULL, NULL, '{\"k\": \"v\"}')",
        rusqlite::params![iso_of(now), now],
    )
    .unwrap();
    drop(conn);
    make_old(&source.join("security-events.db"));

    let second =
        import::apply(&scan_all(&options), &destination, &apply_options(Some(30))).unwrap();
    assert_eq!(second.totals.imported, 1, "only e3 is new");

    // Break the first run only: e2 is absent from the second run's set.
    let conn = Connection::open(&destination).unwrap();
    conn.execute("DELETE FROM security_events WHERE event_id = 'e2'", [])
        .unwrap();
    drop(conn);

    let records = journal::load(&journal::journal_path(&destination)).unwrap();
    assert_eq!(records.len(), 2);

    let verification = import::verify(&destination, None).unwrap();
    assert_eq!(
        verification.runs.len(),
        1,
        "a bare verify selects the most recent run"
    );
    assert_eq!(verification.runs[0].run_id, second.run_id);
    assert!(verification.runs[0].ok, "the latest run is intact");

    asc_state_migrator::run(&verify_cli(&destination)).unwrap();

    // The older run is still reachable — and still broken.
    let older = import::verify(&destination, Some(&first.run_id)).unwrap();
    assert!(!older.runs[0].ok, "run 1 lost e2");
}
