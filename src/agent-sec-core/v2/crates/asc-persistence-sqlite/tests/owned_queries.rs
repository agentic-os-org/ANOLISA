//! Real-store acceptance for schema upgrades, owner isolation and query failures.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use asc_daemon_core::{
    PeerCredentials,
    query::{
        ObservabilityQueries, ObservabilityQueryService, QueryControl, QueryError, QueryPage,
        QueryScope, QueryWindow,
    },
};
use asc_observability::ObservabilityRecord;
use asc_persistence_sqlite::{
    observability::owned::OwnedObservabilityWriter,
    query::{SqliteObservabilityQueries, SqliteSecurityQueries},
    security_events::writer::SqliteEventWriter,
};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

fn control() -> QueryControl {
    QueryControl::new(Instant::now() + Duration::from_secs(5), || false)
}
fn page(offset: i64) -> QueryPage {
    QueryPage { limit: 1, offset }
}

fn record(session: &str, hook: &str, second: u32) -> ObservabilityRecord {
    ObservabilityRecord::from_json_value(&json!({
        "hook":hook, "observedAt":format!("2030-01-01T00:00:{second:02}Z"),
        "metadata":{"sessionId":session,"runId":"shared-run","toolCallId":"shared-tool"},
        "metrics":if hook=="before_tool_call" { json!({"tool_name":"exec","parameters":{"command":"echo x"}}) } else { json!({"user_input":"hello"}) }
    })).unwrap()
}

fn seed() -> (
    TempDir,
    ObservabilityQueryService,
    SqliteObservabilityQueries,
) {
    let directory = TempDir::new().unwrap();
    let obs_path = directory.path().join("obs.db");
    let sec_path = directory.path().join("sec.db");
    let writer = OwnedObservabilityWriter::new(&obs_path).unwrap();
    for uid in [1000, 2000] {
        writer
            .write(&record("shared-session", "before_agent_run", 1), uid)
            .unwrap();
        writer
            .write(&record("shared-session", "before_tool_call", 2), uid)
            .unwrap();
    }
    let security = SqliteEventWriter::new(&sec_path).unwrap();
    security.probe().unwrap();
    security.prepare_query_indexes().unwrap();
    let conn = Connection::open(sec_path.clone()).unwrap();
    for uid in [1000, 2000] {
        let event = json!({"request":{"code":"echo x"},"result":{"verdict":"deny"}});
        conn.execute("INSERT INTO security_events(event_id,event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,uid,session_id,run_id,tool_call_id,details) VALUES (?1,'scan','code_scan','succeeded','2030-01-01T00:00:02Z',1893456002,'',1,?2,'shared-session','shared-run','shared-tool',?3)",rusqlite::params![format!("event-{uid}"),uid,event.to_string()]).unwrap();
        // An uncorrelated failure belongs in the report's category/result counts.
        conn.execute("INSERT INTO security_events(event_id,event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,uid,session_id,run_id,details) VALUES (?1,'scan','code_scan','failed','2030-01-01T00:00:03Z',1893456003,'',1,?2,'shared-session','shared-run','{}')",rusqlite::params![format!("unmatched-{uid}"),uid]).unwrap();
    }
    let reader = SqliteObservabilityQueries::new(obs_path.clone());
    let service = ObservabilityQueryService::new(
        SqliteObservabilityQueries::new(obs_path),
        SqliteSecurityQueries::new(sec_path),
    );
    (directory, service, reader)
}

/// QRY-003/007/008: executable query-contract coverage.
#[test]
fn root_and_user_scopes_do_not_merge_colliding_sessions_or_correlations() {
    let (_directory, service, _) = seed();
    for uid in [1000, 2000] {
        let scope = QueryScope::from_peer(PeerCredentials::new(uid, uid, 1));
        let sessions = service
            .sessions(scope, None, QueryWindow::default(), page(0), &control())
            .unwrap();
        assert_eq!(sessions["total"], 1);
        assert_eq!(sessions["items"][0]["uid"], uid);
        assert_eq!(sessions["items"][0]["security_event_count"], 2);
        assert_eq!(
            sessions["items"][0]["security_by_category_result"]["code_scan"],
            json!({"failed":1,"succeeded":1})
        );
        let events = service
            .timeline(
                scope,
                "shared-session",
                "shared-run",
                QueryWindow::default(),
                page(1),
                true,
                &control(),
            )
            .unwrap();
        assert_eq!(events["total"], 2);
        assert_eq!(events["items"].as_array().unwrap().len(), 2);
        assert!(events["next_offset"].is_null());
        let security = events["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "security")
            .unwrap();
        assert_eq!(security["event"]["uid"], uid);
        assert_eq!(security["event"]["event_id"], format!("event-{uid}"));
        assert_eq!(security["match"]["reason"], "tool_call_id");
    }
    let root = QueryScope::from_peer(PeerCredentials::new(0, 0, 1));
    assert_eq!(root, QueryScope::All);
    let result = service
        .sessions(root, None, QueryWindow::default(), page(0), &control())
        .unwrap();
    assert_eq!(result["total"], 2);
    assert_eq!(result["next_offset"], 1);
    assert!(matches!(
        service.runs(
            root,
            "shared-session",
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::InvalidArgument)
    ));
    assert!(matches!(
        service.timeline(
            root,
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(0),
            true,
            &control()
        ),
        Err(QueryError::InvalidArgument)
    ));
    assert_eq!(
        QueryScope::from_peer(PeerCredentials::new(1000, 1000, 1)),
        QueryScope::Own(1000)
    );
}

/// QRY-007: executable query-contract coverage.
#[test]
fn totals_and_half_open_time_windows_precede_pagination() {
    let (_directory, service, _) = seed();
    let scope = QueryScope::Own(1000);
    let result = service
        .timeline(
            scope,
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(0),
            true,
            &control(),
        )
        .unwrap();
    assert_eq!(result["next_offset"], 1);
    let result = service
        .timeline(
            scope,
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(i64::MAX),
            true,
            &control(),
        )
        .unwrap();
    assert_eq!(result["total"], 2);
    assert_eq!(result["items"], json!([]));
    let result = service
        .timeline(
            scope,
            "shared-session",
            "shared-run",
            QueryWindow {
                since: Some(1_893_456_001.0),
                until: Some(1_893_456_002.0),
            },
            page(0),
            false,
            &control(),
        )
        .unwrap();
    assert_eq!(result["total"], 1);
    assert_eq!(result["items"][0]["hook"], "before_agent_run");
    let result = service
        .runs(
            scope,
            "shared-session",
            QueryWindow::default(),
            page(0),
            &control(),
        )
        .unwrap();
    assert_eq!(result["items"][0]["user_input_preview"], "hello");
    assert_eq!(result["items"][0]["security_event_count"], 2);
}

/// QRY-005: new databases use revision 2 and retain trusted ownership.
#[test]
fn initial_schema_is_revision_two_with_uid() {
    let (directory, _, _) = seed();
    let path = directory.path().join("obs.db");
    let writer = OwnedObservabilityWriter::new(&path).unwrap();
    writer.probe().unwrap();
    writer.probe().unwrap();
    let conn = Connection::open(path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM pragma_table_info('observability_events') WHERE name='uid'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM observability_events WHERE uid IN (1000,2000)",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        4
    );
}

/// QRY-005: executable query-contract coverage.
#[test]
fn unknown_rows_stay_isolated_after_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("obs.db");
    let writer = OwnedObservabilityWriter::new(&path).unwrap();
    writer.probe().unwrap();
    for (session, time) in [("unknown", 1), ("known", 2), ("other-unknown", 3)] {
        writer
            .write(&record(session, "before_agent_run", time), 1000)
            .unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE observability_events SET uid=NULL WHERE session_id != 'known'",
        [],
    )
    .unwrap();
    drop(conn);
    drop(writer);
    OwnedObservabilityWriter::new(&path)
        .unwrap()
        .probe()
        .unwrap();
    let reader = SqliteObservabilityQueries::new(path.clone());
    let result = reader
        .sessions(
            QueryScope::Unknown,
            None,
            QueryWindow::default(),
            QueryPage {
                limit: 100,
                offset: 0,
            },
            &control(),
        )
        .unwrap();
    assert_eq!(result.total, 2);
    assert!(result.items.iter().all(|row| row.uid.is_none()));
    assert_eq!(
        reader
            .sessions(
                QueryScope::Own(1000),
                None,
                QueryWindow::default(),
                page(0),
                &control()
            )
            .unwrap()
            .total,
        1
    );
    let service = ObservabilityQueryService::new(
        reader,
        SqliteSecurityQueries::new(directory.path().join("absent-security.db")),
    );
    let result = service
        .timeline(
            QueryScope::Unknown,
            "unknown",
            "shared-run",
            QueryWindow::default(),
            page(0),
            true,
            &control(),
        )
        .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert!(result["items"][0]["uid"].is_null());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM observability_events WHERE uid IS NULL",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        2
    );
}

/// QRY-010: executable query-contract coverage.
#[test]
fn missing_corrupt_unready_and_malformed_stores_are_not_empty_successes() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("obs.db");
    let reader = SqliteObservabilityQueries::new(path.clone());
    assert!(matches!(
        reader.sessions(
            QueryScope::Own(1000),
            None,
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::Unavailable)
    ));
    assert!(!path.exists());
    std::fs::write(&path, b"not sqlite").unwrap();
    assert!(matches!(
        reader.sessions(
            QueryScope::All,
            None,
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::Internal)
    ));
    assert!(
        OwnedObservabilityWriter::new(&path)
            .unwrap()
            .probe()
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"not sqlite");
    let path = directory.path().join("unready.db");
    let conn = Connection::open(&path).unwrap();
    let reader = SqliteObservabilityQueries::new(path.clone());
    assert!(matches!(
        reader.sessions(
            QueryScope::All,
            None,
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::Unavailable)
    ));
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    OwnedObservabilityWriter::new(&path)
        .unwrap()
        .write(&record("s", "before_agent_run", 1), 1000)
        .unwrap();
    conn.execute(
        "UPDATE observability_events SET metrics_json='not json'",
        [],
    )
    .unwrap();
    assert!(matches!(
        reader.observations(
            QueryScope::Own(1000),
            "s",
            "shared-run",
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::Internal)
    ));
}

/// QRY-011: executable query-contract coverage.
#[test]
fn live_cancellation_interrupts_sql_instead_of_only_checking_ingress() {
    let (directory, _, reader) = seed();
    let conn = Connection::open(directory.path().join("obs.db")).unwrap();
    conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<200000) INSERT INTO observability_events(hook,observed_at,observed_at_epoch,session_id,run_id,metrics_json,metadata_json,uid) SELECT 'before_agent_run','2030-01-01T00:00:01Z',1893456001,printf('s-%d',x),'r','{}','{}',1000 FROM n").unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let control = QueryControl::new(Instant::now() + Duration::from_secs(5), move || {
        count.fetch_add(1, Ordering::Relaxed) > 10
    });
    assert!(matches!(
        reader.sessions(
            QueryScope::All,
            None,
            QueryWindow::default(),
            page(0),
            &control
        ),
        Err(QueryError::DeadlineExceeded)
    ));
    assert!(calls.load(Ordering::Relaxed) > 10);
    let flag = Arc::new(AtomicBool::new(false));
    let signal = flag.clone();
    let control = QueryControl::new(Instant::now() + Duration::from_secs(5), move || {
        signal.load(Ordering::Acquire)
    });
    flag.store(true, Ordering::Release);
    assert!(matches!(control.check(), Err(QueryError::DeadlineExceeded)));
}

/// QRY-011: executable query-contract coverage.
#[test]
fn oversized_payloads_fail_without_partial_timeline_results() {
    let (directory, service, _) = seed();
    let conn = Connection::open(directory.path().join("obs.db")).unwrap();
    conn.execute(
        "UPDATE observability_events SET metrics_json=?1 WHERE uid=1000",
        [json!({"user_input":"x".repeat(4*1024*1024)}).to_string()],
    )
    .unwrap();
    assert!(matches!(
        service.timeline(
            QueryScope::Own(1000),
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(0),
            true,
            &control()
        ),
        Err(QueryError::ResourceExhausted)
    ));
    // Another owner's large rows must not make this user's page fail.
    assert!(
        service
            .timeline(
                QueryScope::Own(2000),
                "shared-session",
                "shared-run",
                QueryWindow::default(),
                page(0),
                false,
                &control()
            )
            .is_ok()
    );
}

/// QRY-003/007: exact session filtering precedes grouping, totals and pagination.
#[test]
fn exact_session_filter_preserves_scope_totals_and_time_window() {
    let (directory, service, reader) = seed();
    let writer = OwnedObservabilityWriter::new(&directory.path().join("obs.db")).unwrap();
    for index in 0..205 {
        writer
            .write(
                &record(&format!("unrelated-{index}"), "before_agent_run", 3),
                1000,
            )
            .unwrap();
    }
    for (scope, total) in [
        (QueryScope::All, 2),
        (QueryScope::Own(1000), 1),
        (QueryScope::Own(3000), 0),
    ] {
        let result = service
            .sessions(
                scope,
                Some("shared-session"),
                QueryWindow::default(),
                page(0),
                &control(),
            )
            .unwrap();
        assert_eq!(result["total"], total);
        for row in result["items"].as_array().unwrap() {
            assert_eq!(
                row["session_id"],
                if scope == QueryScope::All {
                    format!("{}_shared-session", row["uid"])
                } else {
                    "shared-session".into()
                }
            );
        }
    }
    let second = reader
        .sessions(
            QueryScope::All,
            Some("shared-session"),
            QueryWindow::default(),
            page(1),
            &control(),
        )
        .unwrap();
    assert_eq!(second.total, 2);
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].uid, Some(2000));
    for (session, window) in [
        ("missing", QueryWindow::default()),
        (
            "shared-session",
            QueryWindow {
                since: Some(1_893_456_003.0),
                until: None,
            },
        ),
    ] {
        let result = reader
            .sessions(QueryScope::All, Some(session), window, page(0), &control())
            .unwrap();
        assert_eq!(result.total, 0);
        assert!(result.items.is_empty());
    }
}

/// QRY-004/005: a cached connection must still reject unsupported schema revisions.
#[test]
fn owned_write_rejects_future_revision_without_inserting() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("obs.db");
    let writer = OwnedObservabilityWriter::new(&path).unwrap();
    writer.probe().unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    assert!(
        writer
            .write(&record("s", "before_agent_run", 1), 1000)
            .is_err()
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM observability_events", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

/// QRY-003: root resource labels disambiguate owners without accepting an identity parameter.
#[test]
fn root_qualified_sessions_preserve_owner_isolation() {
    let (_directory, service, _) = seed();
    for uid in [1000, 2000] {
        let id = format!("{uid}_shared-session");
        let selected = service
            .sessions(
                QueryScope::All,
                Some(&id),
                QueryWindow::default(),
                page(0),
                &control(),
            )
            .unwrap();
        assert_eq!(selected["total"], 1);
        assert_eq!(selected["items"][0]["session_id"], id);
        assert_eq!(selected["items"][0]["security_event_count"], 2);
        let runs = service
            .runs(
                QueryScope::All,
                &id,
                QueryWindow::default(),
                page(0),
                &control(),
            )
            .unwrap();
        assert_eq!(runs["uid"], uid);
        assert_eq!(runs["session_id"], id);
        let timeline = service
            .timeline(
                QueryScope::All,
                &id,
                "shared-run",
                QueryWindow::default(),
                page(1),
                true,
                &control(),
            )
            .unwrap();
        for item in timeline["items"].as_array().unwrap() {
            assert_eq!(item["uid"], uid);
            if item["kind"] == "security" {
                assert_eq!(item["event"]["uid"], uid);
            }
        }
        // A resource label cannot change the authenticated non-root scope.
        let other = service
            .runs(
                QueryScope::Own(3000),
                &id,
                QueryWindow::default(),
                page(0),
                &control(),
            )
            .unwrap();
        assert_eq!(other["total"], 0);
    }
}

/// QRY-003: ordinary IDs stay unchanged; literal/qualified name collisions fail closed.
#[test]
fn unique_sessions_keep_their_ids_and_ambiguous_qualified_names_fail() {
    let (directory, service, _) = seed();
    let writer = OwnedObservabilityWriter::new(&directory.path().join("obs.db")).unwrap();
    let unique = "a6ee17df-a6b0-4ab2-a97f-0bfc879d2401";
    writer
        .write(&record(unique, "before_agent_run", 3), 3000)
        .unwrap();
    let selected = service
        .sessions(
            QueryScope::All,
            Some(unique),
            QueryWindow::default(),
            page(0),
            &control(),
        )
        .unwrap();
    assert_eq!(selected["items"][0]["session_id"], unique);
    assert_eq!(
        service
            .runs(
                QueryScope::All,
                unique,
                QueryWindow::default(),
                page(0),
                &control()
            )
            .unwrap()["uid"],
        3000
    );
    // A literal ID may itself look like another row's qualified ID. Never guess.
    writer
        .write(&record("1000_shared-session", "before_agent_run", 3), 3000)
        .unwrap();
    assert!(matches!(
        service.runs(
            QueryScope::All,
            "1000_shared-session",
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::InvalidArgument)
    ));
    assert!(matches!(
        service.sessions(
            QueryScope::All,
            Some("1000_shared-session"),
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::InvalidArgument)
    ));
}

/// QRY-005: legacy records survive upgrade without acquiring another user's identity.
#[test]
fn legacy_schema_upgrade_preserves_unknown_rows_and_admits_owned_writes() {
    let (directory, _, _) = seed();
    let path = directory.path().join("legacy.db");
    let legacy =
        asc_persistence_sqlite::observability::ObservabilitySqliteWriter::new(&path).unwrap();
    legacy
        .write_or_raise(&record("shared-session", "before_tool_call", 1))
        .unwrap();
    drop(legacy);
    let conn = Connection::open(&path).unwrap();
    let original: String = conn
        .query_row("SELECT metrics_json FROM observability_events", [], |r| {
            r.get(0)
        })
        .unwrap();
    let reader = SqliteObservabilityQueries::new(path.clone());
    assert!(matches!(
        reader.sessions(
            QueryScope::All,
            None,
            QueryWindow::default(),
            page(0),
            &control()
        ),
        Err(QueryError::Unavailable)
    ));
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    let writer = OwnedObservabilityWriter::new(&path).unwrap();
    writer.probe().unwrap();
    writer.probe().unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT uid,metrics_json FROM observability_events WHERE id=1",
            [],
            |r| Ok((r.get::<_, Option<u32>>(0)?, r.get::<_, String>(1)?))
        )
        .unwrap(),
        (None, original)
    );
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('idx_observability_owner_time','idx_observability_owner_session_run_time')", [], |r| r.get::<_, u32>(0)).unwrap(), 2);
    writer
        .write(&record("shared-session", "before_tool_call", 2), 1000)
        .unwrap();
    drop(writer);
    OwnedObservabilityWriter::new(&path)
        .unwrap()
        .probe()
        .unwrap();
    let service = ObservabilityQueryService::new(
        reader,
        SqliteSecurityQueries::new(directory.path().join("sec.db")),
    );
    let own = service
        .sessions(
            QueryScope::Own(1000),
            None,
            QueryWindow::default(),
            page(0),
            &control(),
        )
        .unwrap();
    assert_eq!(own["total"], 1);
    assert_eq!(own["items"][0]["observability_event_count"], 1);
    let historical = service
        .timeline(
            QueryScope::All,
            "unknown_shared-session",
            "shared-run",
            QueryWindow::default(),
            page(0),
            true,
            &control(),
        )
        .unwrap();
    assert_eq!(historical["items"].as_array().unwrap().len(), 1);
    assert!(historical["items"][0]["uid"].is_null());
    assert_eq!(historical["items"][0]["kind"], "observability");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM observability_events", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        2
    );
}

/// QRY-005: revision-1 databases that already carry UIDs must retain them unchanged.
#[test]
fn upgrading_owned_revision_one_preserves_existing_owners() {
    let (directory, _, _) = seed();
    let path = directory.path().join("obs.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("UPDATE observability_events SET uid=0 WHERE id=1; UPDATE observability_events SET uid=NULL WHERE id=2; PRAGMA user_version=1").unwrap();
    let owners = || {
        conn.prepare("SELECT uid FROM observability_events ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, Option<u32>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let original = owners();
    let writer = OwnedObservabilityWriter::new(&path).unwrap();
    writer.probe().unwrap();
    writer.probe().unwrap();
    assert_eq!(owners(), original);
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
}

/// QRY-005: failed index creation rolls back the column and version, then allows retry.
#[test]
fn legacy_schema_upgrade_failure_rolls_back_and_can_be_retried() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("obs.db");
    let legacy =
        asc_persistence_sqlite::observability::ObservabilitySqliteWriter::new(&path).unwrap();
    legacy
        .write_or_raise(&record("s", "before_agent_run", 1))
        .unwrap();
    drop(legacy);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE idx_observability_owner_time (id INTEGER)")
        .unwrap();
    let writer = OwnedObservabilityWriter::new(&path).unwrap();
    assert!(writer.probe().is_err());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('observability_events') WHERE name='uid'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM observability_events", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        1
    );
    conn.execute_batch("DROP TABLE idx_observability_owner_time")
        .unwrap();
    writer.probe().unwrap();
    writer
        .write(&record("s", "before_agent_run", 2), 1000)
        .unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM observability_events WHERE uid IS NULL",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM observability_events WHERE uid=1000",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
}

/// QRY-008: V1 reads the first 1000 candidates without rejecting a larger match set.
#[test]
fn correlation_candidates_keep_v1_order_and_limit_without_failing_timeline() {
    use asc_daemon_core::query::SecurityQueries;

    let (directory, service, reader) = seed();
    let path = directory.path().join("sec.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 0 UNION ALL SELECT x+1 FROM n WHERE x<999) INSERT INTO security_events(event_id,event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,uid,session_id,run_id,tool_call_id,details) SELECT printf('candidate-%04d',x),'scan','code_scan','succeeded','2030-01-01T00:00:03Z',1893456003+x,'',1,1000,'shared-session','shared-run','shared-tool','{}' FROM n").unwrap();
    let record = reader
        .observations(
            QueryScope::Own(1000),
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(1),
            &control(),
        )
        .unwrap()
        .items
        .remove(0);
    let security = SqliteSecurityQueries::new(path);
    let candidates = security
        .candidates(QueryScope::Own(1000), &record, &control())
        .unwrap();
    assert_eq!(candidates.len(), 1000);
    assert_eq!(candidates[0].event.event_id, "event-1000");
    assert_eq!(candidates.last().unwrap().event.event_id, "candidate-0998");
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let cancelled = QueryControl::new(Instant::now() + Duration::from_secs(5), move || {
        checks.fetch_add(1, Ordering::Relaxed) > 10
    });
    assert!(matches!(
        security.candidates(QueryScope::Own(1000), &record, &cancelled),
        Err(QueryError::DeadlineExceeded)
    ));
    let timeline = service
        .timeline(
            QueryScope::Own(1000),
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(1),
            true,
            &control(),
        )
        .unwrap();
    let items = timeline["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|item| item["kind"] == "observability"));
    assert!(
        items
            .iter()
            .any(|item| item["event"]["event_id"] == "event-1000")
    );
}

/// QRY-008: bad candidates are skipped while other same-owner matches remain visible.
#[test]
fn malformed_candidates_do_not_hide_valid_correlations_or_observations() {
    let (directory, service, _) = seed();
    let conn = Connection::open(directory.path().join("sec.db")).unwrap();
    conn.execute("INSERT INTO security_events(event_id,event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,uid,session_id,run_id,call_id,tool_call_id,details) SELECT 'valid-event',event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,1000,session_id,run_id,call_id,tool_call_id,details FROM security_events WHERE event_id='event-2000'", []).unwrap();
    for (details, result, pid) in [
        ("not json", "succeeded", 1_i64),
        ("[]", "succeeded", 1),
        ("{}", "invalid", 1),
        ("{}", "succeeded", -1),
        ("{}", "succeeded", i64::from(u32::MAX) + 1),
    ] {
        conn.execute(
            "UPDATE security_events SET details=?1,result=?2,pid=?3 WHERE event_id='event-1000'",
            rusqlite::params![details, result, pid],
        )
        .unwrap();
        let timeline = service
            .timeline(
                QueryScope::Own(1000),
                "shared-session",
                "shared-run",
                QueryWindow::default(),
                page(1),
                true,
                &control(),
            )
            .unwrap();
        let items = timeline["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item["kind"] == "observability"));
        assert!(
            items
                .iter()
                .any(|item| item["event"]["event_id"] == "valid-event")
        );
        assert!(items.iter().all(|item| item["uid"] == 1000));
    }
}

/// QRY-008/011: optional correlation storage faults preserve the observation page.
#[test]
fn candidate_storage_failures_do_not_fail_the_observation_page() {
    for fault in [
        "missing",
        "corrupt",
        "future",
        "oversized",
        "oversized-page",
    ] {
        let (directory, service, _) = seed();
        let path = directory.path().join("sec.db");
        match fault {
            "missing" => std::fs::rename(&path, path.with_extension("backup")).unwrap(),
            "corrupt" => std::fs::write(&path, b"not sqlite").unwrap(),
            "future" => Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", 99)
                .unwrap(),
            "oversized" => {
                Connection::open(&path)
                    .unwrap()
                    .execute(
                        "UPDATE security_events SET details=?1 WHERE event_id='event-1000'",
                        [json!({"large":"x".repeat(4*1024*1024)}).to_string()],
                    )
                    .unwrap();
            }
            _ => {
                let conn = Connection::open(&path).unwrap();
                conn.execute(
                    "UPDATE security_events SET details=?1 WHERE event_id='event-1000'",
                    [json!({"large":"x".repeat(2*1024*1024)}).to_string()],
                )
                .unwrap();
                conn.execute("INSERT INTO security_events(event_id,event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,uid,session_id,run_id,tool_call_id,details) SELECT 'another-large-event',event_type,category,result,timestamp,timestamp_epoch,trace_id,pid,uid,session_id,run_id,tool_call_id,details FROM security_events WHERE event_id='event-1000'", []).unwrap();
            }
        }
        let timeline = service
            .timeline(
                QueryScope::Own(1000),
                "shared-session",
                "shared-run",
                QueryWindow::default(),
                page(1),
                true,
                &control(),
            )
            .unwrap();
        let items = timeline["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{fault}");
        assert_eq!(items[0]["kind"], "observability", "{fault}");
        assert_eq!(items[0]["uid"], 1000);
    }
}

/// QRY-003/011: best-effort correlation never hides authorization or cancellation errors.
#[test]
fn candidate_fault_tolerance_preserves_scope_and_deadline_errors() {
    use asc_daemon_core::query::SecurityQueries;

    let (directory, _, reader) = seed();
    let record = reader
        .observations(
            QueryScope::Own(1000),
            "shared-session",
            "shared-run",
            QueryWindow::default(),
            page(1),
            &control(),
        )
        .unwrap()
        .items
        .remove(0);
    let security = SqliteSecurityQueries::new(directory.path().join("absent.db"));
    assert!(matches!(
        security.candidates(QueryScope::Own(2000), &record, &control()),
        Err(QueryError::PermissionDenied)
    ));
    let cancelled = QueryControl::new(Instant::now() + Duration::from_secs(5), || true);
    assert!(matches!(
        security.candidates(QueryScope::Own(1000), &record, &cancelled),
        Err(QueryError::DeadlineExceeded)
    ));
}
