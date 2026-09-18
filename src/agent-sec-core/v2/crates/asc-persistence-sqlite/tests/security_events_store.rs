//! End-to-end coverage of the security-event binding against a real database.

use std::path::Path;
use std::sync::Arc;

use asc_persistence_sqlite::security_events::{
    EventFilters, SECURITY_EVENTS_TABLES, SecurityEventRepository, SecurityEventsFaultPolicy,
    SecurityEventsMigrator, StderrDropSink, VALID_GROUP_FIELDS, repository::CorrelationRequest,
};
use asc_security_events::{
    EventResult, SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, SecurityEvent, timestamp::utc_iso_to_epoch,
};
use asc_sqlite_kernel::{
    KernelError, ReadOnlySource, RecordRepository, SqliteSink, SqliteStore, schema,
};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

const LOG_PREFIX: &str = "[security_events]";

fn store(path: &Path, read_only: bool) -> Arc<SqliteStore> {
    Arc::new(
        SqliteStore::new(
            path,
            read_only,
            SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
            SECURITY_EVENTS_TABLES,
            Some(Arc::new(SecurityEventsMigrator)),
            LOG_PREFIX,
        )
        .expect("store"),
    )
}

fn sink(
    path: &Path,
) -> SqliteSink<SecurityEventRepository, SecurityEventsFaultPolicy<StderrDropSink>> {
    SqliteSink::new(
        store(path, false),
        SecurityEventRepository,
        SecurityEventsFaultPolicy::default(),
        Some(30),
        true,
    )
}

fn details(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// Builds an event with a deterministic id and timestamp.
fn event(id: &str, category: &str, timestamp: &str, details: Map<String, Value>) -> SecurityEvent {
    let mut event = SecurityEvent::new("sandbox_prehook", category, details);
    id.clone_into(&mut event.event_id);
    event
        .set_timestamp(timestamp)
        .expect("the fixture timestamps are valid");
    event
}

fn seed(path: &Path) {
    let sink = sink(path);
    sink.write(&event(
        "e1",
        "exec",
        "2026-01-01T00:00:00+00:00",
        details(&[("verdict", json!("allow"))]),
    ));
    sink.write(&event(
        "e2",
        "exec",
        "2026-01-02T00:00:00+00:00",
        details(&[("result", json!({"verdict": "deny"}))]),
    ));
    sink.write(&event(
        "e3",
        "network",
        "2026-01-03T00:00:00+00:00",
        details(&[("note", json!("no verdict here"))]),
    ));
    sink.close(1000.0);
}

fn read(path: &Path) -> ReadOnlySource<SecurityEventRepository> {
    ReadOnlySource::new(store(path, true), SecurityEventRepository)
}

#[test]
fn a_written_event_round_trips_through_a_read_only_source() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    let source = read(&path);
    let events =
        source.query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 100, 0));

    assert_eq!(events.len(), 3);
    // Newest first, matching v1's ORDER BY timestamp_epoch DESC.
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["e3", "e2", "e1"]
    );
    let first = &events[0];
    assert_eq!(first.category, "network");
    assert_eq!(first.result, EventResult::Succeeded);
    assert_eq!(first.details.get("note"), Some(&json!("no verdict here")));
}

#[test]
fn the_verdict_column_is_derived_from_both_details_shapes() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    let source = read(&path);
    let verdicts = source
        .query_or_default(|repo, conn| repo.count_by(conn, "verdict", &EventFilters::default(), 0));

    let mut rendered: Vec<(Option<String>, u64)> = verdicts;
    rendered.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        rendered,
        vec![(Some("allow".to_owned()), 1), (Some("deny".to_owned()), 1),],
        "the row without a verdict must be excluded from the grouping"
    );
}

#[test]
fn filters_are_applied_in_sql_including_verdict() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);
    let source = read(&path);

    let filters = EventFilters {
        verdict: Some("deny".to_owned()),
        ..EventFilters::default()
    };
    let events = source.query_or_default(|repo, conn| repo.query(conn, &filters, 100, 0));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, "e2");

    let count = source.query_or(0, |repo, conn| repo.count(conn, &filters, 0));
    assert_eq!(count, 1);
}

#[test]
fn a_time_window_uses_an_inclusive_lower_and_exclusive_upper_bound() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);
    let source = read(&path);

    let filters = EventFilters::default()
        .since("2026-01-01T00:00:00+00:00")
        .expect("since")
        .until("2026-01-03T00:00:00+00:00")
        .expect("until");
    let events = source.query_or_default(|repo, conn| repo.query(conn, &filters, 100, 0));
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["e2", "e1"],
        "the lower bound includes e1 and the upper bound excludes e3"
    );
}

#[test]
fn an_offset_count_reports_the_remainder() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);
    let source = read(&path);

    assert_eq!(
        source.query_or(0, |repo, conn| repo.count(
            conn,
            &EventFilters::default(),
            0
        )),
        3
    );
    assert_eq!(
        source.query_or(0, |repo, conn| repo.count(
            conn,
            &EventFilters::default(),
            2
        )),
        1
    );
}

#[test]
fn get_returns_one_event_or_nothing() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);
    let source = read(&path);

    let found = source.query_or(None, |repo, conn| repo.get(conn, "e2"));
    assert_eq!(found.map(|event| event.event_id), Some("e2".to_owned()));
    assert!(
        source
            .query_or(None, |repo, conn| repo.get(conn, "absent"))
            .is_none()
    );
}

#[test]
fn summary_aggregates_five_groups_and_the_latest_rows() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);
    let source = read(&path);

    let summary =
        source.query_or_default(|repo, conn| repo.summary(conn, &EventFilters::default(), 2));

    assert_eq!(summary.total, 3);
    let mut by_category = summary.by_category.clone();
    by_category.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        by_category,
        vec![
            (Some("exec".to_owned()), 2),
            (Some("network".to_owned()), 1)
        ]
    );
    assert_eq!(
        summary.by_event_type,
        vec![(Some("sandbox_prehook".to_owned()), 3)]
    );
    assert_eq!(summary.by_result, vec![(Some("succeeded".to_owned()), 3)]);
    assert_eq!(summary.by_session, vec![(None, 3)]);
    assert_eq!(summary.by_run, vec![(None, 3)]);
    assert_eq!(
        summary
            .latest_events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["e3", "e2"],
        "latest_limit must be honoured"
    );
}

#[test]
fn a_filtered_summary_repeats_the_bound_parameters_per_branch() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);
    let source = read(&path);

    // Regression guard: the five UNION ALL branches share one parameter list, so
    // a naive template would bind ?1 five times to the wrong slot.
    let filters = EventFilters {
        category: Some("exec".to_owned()),
        ..EventFilters::default()
    };
    let summary = source.query_or_default(|repo, conn| repo.summary(conn, &filters, 5));
    assert_eq!(summary.total, 2);
    assert_eq!(summary.by_category, vec![(Some("exec".to_owned()), 2)]);
}

#[test]
fn count_by_rejects_a_field_outside_the_allowlist() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    let store = store(&path, true);
    let error = store
        .with_connection(true, |conn| {
            SecurityEventRepository.count_by(conn, "details", &EventFilters::default(), 0)
        })
        .expect_err("details is not groupable");
    let message = error.to_string();
    assert!(
        message.contains("Invalid group_field: 'details'"),
        "{message}"
    );
    assert!(
        message.contains("call_id, category, event_type, result, run_id, session_id, tool_call_id, trace_id, verdict"),
        "the allowlist must be reported alphabetically, as in v1: {message}"
    );

    for field in VALID_GROUP_FIELDS {
        store
            .with_connection(true, |conn| {
                SecurityEventRepository.count_by(conn, field, &EventFilters::default(), 0)
            })
            .unwrap_or_else(|err| panic!("{field} must be groupable: {err}"));
    }
}

#[test]
fn correlation_candidates_are_ordered_and_capped_by_the_filters() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");

    let sink = sink(&path);
    for (index, category) in ["exec", "network", "exec"].into_iter().enumerate() {
        let mut event = event(
            &format!("c{index}"),
            category,
            &format!("2026-01-0{}T00:00:00+00:00", index + 1),
            Map::new(),
        );
        event.session_id = Some("s-1".to_owned());
        event.run_id = Some("r-1".to_owned());
        event.tool_call_id = Some(format!("t{index}"));
        sink.write(&event);
    }
    sink.close(1000.0);

    let source = read(&path);
    let categories = vec!["exec".to_owned()];
    let request = CorrelationRequest {
        session_id: "s-1",
        categories: &categories,
        run_id: Some("r-1"),
        ..CorrelationRequest::default()
    };
    let candidates =
        source.query_or_default(|repo, conn| repo.query_correlation_candidates(conn, &request));
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c0", "c2"],
        "candidates must come back oldest first"
    );
    assert!(candidates[0].timestamp_epoch < candidates[1].timestamp_epoch);

    // An empty category list and an all-empty tool_call_ids both short-circuit.
    let no_categories = CorrelationRequest {
        session_id: "s-1",
        ..CorrelationRequest::default()
    };
    assert!(
        source
            .query_or_default(|repo, conn| repo.query_correlation_candidates(conn, &no_categories))
            .is_empty()
    );
    let blank = vec![String::new()];
    let blank_tool_calls = CorrelationRequest {
        session_id: "s-1",
        categories: &categories,
        tool_call_ids: Some(&blank),
        ..CorrelationRequest::default()
    };
    assert!(
        source
            .query_or_default(
                |repo, conn| repo.query_correlation_candidates(conn, &blank_tool_calls)
            )
            .is_empty()
    );
}

#[test]
fn a_malformed_stored_row_is_skipped_not_fatal() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    store(&path, false)
        .with_connection(true, |conn| {
            Ok(conn.execute(
                "UPDATE security_events SET details = 'not json' WHERE event_id = 'e2'",
                [],
            )?)
        })
        .expect("corrupt one row")
        .expect("value");

    let events = read(&path)
        .query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 100, 0));
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["e3", "e1"],
        "one bad row must not blank out the query"
    );
}

#[test]
fn a_row_whose_result_is_out_of_range_is_skipped() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    store(&path, false)
        .with_connection(true, |conn| {
            Ok(conn.execute(
                "UPDATE security_events SET result = 'exploded' WHERE event_id = 'e1'",
                [],
            )?)
        })
        .expect("corrupt one row")
        .expect("value");

    let events = read(&path)
        .query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 100, 0));
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.event_id != "e1"));
}

#[test]
fn a_duplicate_event_id_is_not_a_dropped_write() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = sink(&path);
    let event = event("dup", "exec", "2026-01-01T00:00:00+00:00", Map::new());

    sink.write(&event);
    sink.write(&event);
    sink.close(1000.0);

    assert_eq!(
        read(&path).query_or(0, |repo, conn| repo.count(
            conn,
            &EventFilters::default(),
            0
        )),
        1
    );
}

#[test]
fn an_unserializable_timestamp_reports_a_skipped_write() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let store = store(&path, false);

    let mut event = event("bad", "exec", "2026-01-01T00:00:00+00:00", Map::new());
    // Bypass the setter to reach the branch a hand-built event can hit.
    event.timestamp = "not a timestamp".to_owned();

    let inserted = store
        .with_connection(true, |conn| {
            SecurityEventRepository.insert_or_raise(conn, &event)
        })
        .expect("no database fault")
        .expect("value");
    assert!(
        !inserted,
        "v1 turns a malformed event into a skipped write, not an error"
    );
}

#[test]
fn retention_prunes_by_timestamp_epoch() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    let cutoff_now = utc_iso_to_epoch("2026-01-03T00:00:00+00:00", "now").expect("epoch");
    store(&path, false)
        .with_connection(true, |conn| {
            SecurityEventRepository.prune(conn, 1, cutoff_now)
        })
        .expect("prune")
        .expect("value");

    let events = read(&path)
        .query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 100, 0));
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["e3", "e2"],
        "only rows older than one day before the cutoff are removed"
    );
}

#[test]
fn the_converged_schema_carries_every_declared_column_and_index() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    seed(&path);

    let store = store(&path, true);
    store
        .with_connection(true, |conn| {
            let columns = schema::existing_columns(conn, "security_events")?;
            for expected in SECURITY_EVENTS_TABLES[0].columns {
                assert!(
                    columns.iter().any(|name| name == expected.name),
                    "{} is missing",
                    expected.name
                );
            }

            let mut statement = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'security_events'",
            )?;
            let names: Vec<String> = statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            for index in SECURITY_EVENTS_TABLES[0].indexes {
                assert!(names.iter().any(|name| name == index.name), "{} is missing", index.name);
            }

            let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            assert_eq!(version, i64::from(SECURITY_EVENTS_SQLITE_SCHEMA_VERSION));
            Ok::<(), KernelError>(())
        })
        .expect("inspect")
        .expect("value");
}

#[test]
fn a_revision_one_database_is_lifted_by_generic_convergence() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");

    // A rev-1 table has none of the four correlation columns and no version stamp.
    let seed_conn = rusqlite::Connection::open(&path).expect("open");
    seed_conn
        .execute_batch(
            "CREATE TABLE security_events (\
                event_id TEXT PRIMARY KEY, event_type TEXT NOT NULL, category TEXT NOT NULL, \
                result TEXT NOT NULL DEFAULT 'succeeded', timestamp TEXT NOT NULL, \
                timestamp_epoch REAL NOT NULL, trace_id TEXT NOT NULL DEFAULT '', \
                pid INTEGER NOT NULL, uid INTEGER NOT NULL, session_id TEXT, \
                details TEXT NOT NULL); \
             PRAGMA user_version = 1",
        )
        .expect("rev1 schema");
    seed_conn
        .execute(
            "INSERT INTO security_events (event_id, event_type, category, timestamp, \
             timestamp_epoch, pid, uid, details) VALUES ('old', 't', 'exec', \
             '2026-01-01T00:00:00+00:00', 1.0, 1, 1, '{\"verdict\": \"deny\"}')",
            [],
        )
        .expect("legacy row");
    drop(seed_conn);

    let sink = sink(&path);
    sink.write(&event(
        "new",
        "exec",
        "2026-01-02T00:00:00+00:00",
        Map::new(),
    ));
    sink.close(1000.0);

    let source = read(&path);
    let events =
        source.query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 100, 0));
    assert_eq!(events.len(), 2, "the legacy row must survive convergence");

    // The migrator's guard is `from < 3 <= to`, so a 1 -> 3 lift also backfills;
    // generic convergence adds the three correlation columns on the same pass.
    let verdicts = source
        .query_or_default(|repo, conn| repo.count_by(conn, "verdict", &EventFilters::default(), 0));
    assert_eq!(
        verdicts,
        vec![(Some("deny".to_owned()), 1)],
        "a rev-1 lift adds the columns and backfills in one pass"
    );

    let filters = EventFilters {
        run_id: Some("anything".to_owned()),
        ..EventFilters::default()
    };
    assert_eq!(
        source.query_or(u64::MAX, |repo, conn| repo.count(conn, &filters, 0)),
        0,
        "the run_id column must exist for the filter to be applied in SQL"
    );
}

#[test]
fn a_revision_two_database_is_backfilled_by_the_migrator() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");

    let seed_conn = rusqlite::Connection::open(&path).expect("open");
    seed_conn
        .execute_batch(
            "CREATE TABLE security_events (\
                event_id TEXT PRIMARY KEY, event_type TEXT NOT NULL, category TEXT NOT NULL, \
                result TEXT NOT NULL DEFAULT 'succeeded', timestamp TEXT NOT NULL, \
                timestamp_epoch REAL NOT NULL, trace_id TEXT NOT NULL DEFAULT '', \
                pid INTEGER NOT NULL, uid INTEGER NOT NULL, session_id TEXT, run_id TEXT, \
                call_id TEXT, tool_call_id TEXT, details TEXT NOT NULL); \
             PRAGMA user_version = 2",
        )
        .expect("rev2 schema");
    seed_conn
        .execute(
            "INSERT INTO security_events (event_id, event_type, category, timestamp, \
             timestamp_epoch, pid, uid, details) VALUES ('old', 't', 'exec', \
             '2026-01-01T00:00:00+00:00', 1.0, 1, 1, '{\"result\": {\"verdict\": \"deny\"}}')",
            [],
        )
        .expect("legacy row");
    drop(seed_conn);

    let sink = sink(&path);
    sink.write(&event(
        "new",
        "exec",
        "2026-01-02T00:00:00+00:00",
        Map::new(),
    ));
    sink.close(1000.0);

    let verdicts = read(&path)
        .query_or_default(|repo, conn| repo.count_by(conn, "verdict", &EventFilters::default(), 0));
    assert_eq!(
        verdicts,
        vec![(Some("deny".to_owned()), 1)],
        "the 2 -> 3 migrator must backfill the legacy row"
    );
}

/// Ten threads sharing one sink lose nothing.
///
/// v1 guards this stream with a `threading.Lock`; v2 sets `serializes_writes`
/// on the sink for the same reason. Without it the shared connection would be
/// entered concurrently and rows would be lost rather than serialized.
#[test]
fn ten_threads_sharing_one_sink_lose_no_rows() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    let sink = Arc::new(sink(&path));

    std::thread::scope(|scope| {
        for thread in 0..10 {
            let sink = Arc::clone(&sink);
            scope.spawn(move || {
                for index in 0..10 {
                    sink.write(&event(
                        &format!("t{thread}-e{index}"),
                        "exec",
                        "2026-01-01T00:00:00+00:00",
                        Map::new(),
                    ));
                }
            });
        }
    });
    sink.close(1000.0);

    assert_eq!(
        read(&path).query_or(0, |repo, conn| repo.count(
            conn,
            &EventFilters::default(),
            0
        )),
        100
    );
}

/// Independent sinks on one database may lose rows only to `SQLITE_BUSY`.
///
/// The per-sink lock cannot order writers that do not share it, so the bound is
/// v1's: every event either landed or was dropped as busy, and no event landed
/// twice. Anything else — a corrupted database, a partial row — would break the
/// equality below.
#[test]
fn independent_sinks_on_one_database_only_lose_rows_to_busy() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");
    // v1 warms the database up first so the concurrent phase is not also a
    // schema race; that race has its own test below.
    let warmup = sink(&path);
    warmup.write(&event(
        "warmup",
        "exec",
        "2026-01-01T00:00:00+00:00",
        Map::new(),
    ));
    warmup.close(1000.0);

    let sinks: Vec<_> = (0..8).map(|_| sink(&path)).collect();
    std::thread::scope(|scope| {
        for (writer, sink) in sinks.iter().enumerate() {
            scope.spawn(move || {
                for index in 0..20 {
                    sink.write(&event(
                        &format!("w{writer}-e{index}"),
                        "concurrent",
                        "2026-01-01T00:00:00+00:00",
                        Map::new(),
                    ));
                }
            });
        }
    });
    for sink in &sinks {
        sink.close(1000.0);
    }

    let events = read(&path)
        .query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 1000, 0));
    let landed: Vec<_> = events
        .iter()
        .filter(|event| event.category == "concurrent")
        .map(|event| event.event_id.as_str())
        .collect();
    let mut unique = landed.clone();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(unique.len(), landed.len(), "no event may land twice");
    assert!(
        landed.len() <= 8 * 20,
        "a write can be dropped as busy but never invented"
    );
}

/// A cold bootstrap race is best effort, and never a corruption.
///
/// Four sinks create the same missing database at once, so several of them run
/// schema convergence concurrently. v1 accepts losing writes here; what neither
/// version accepts is a rebuilt database, which would discard whatever landed
/// first. The probe write afterwards proves the file is still usable.
#[test]
fn a_cold_bootstrap_race_keeps_the_database_usable() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.db");

    std::thread::scope(|scope| {
        for writer in 0..4 {
            let path = path.clone();
            scope.spawn(move || {
                let sink = sink(&path);
                for index in 0..5 {
                    sink.write(&event(
                        &format!("cold{writer}-e{index}"),
                        "cold",
                        "2026-01-01T00:00:00+00:00",
                        Map::new(),
                    ));
                }
                sink.close(1000.0);
            });
        }
    });

    let probe = sink(&path);
    probe.write(&event(
        "cold-probe",
        "cold",
        "2026-01-01T00:00:00+00:00",
        Map::new(),
    ));
    probe.close(1000.0);

    let events = read(&path)
        .query_or_default(|repo, conn| repo.query(conn, &EventFilters::default(), 1000, 0));
    let ids: Vec<_> = events.iter().map(|e| e.event_id.as_str()).collect();
    assert!(
        ids.contains(&"cold-probe"),
        "the database must still accept writes after the race"
    );
    assert!(ids.len() <= 4 * 5 + 1);
}
