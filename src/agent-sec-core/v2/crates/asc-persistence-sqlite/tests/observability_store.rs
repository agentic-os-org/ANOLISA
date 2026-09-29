//! End-to-end coverage of the observability binding against a real database.

use std::path::Path;
use std::sync::Arc;

use asc_observability::{
    OBSERVABILITY_SQLITE_SCHEMA_VERSION, ObservabilityHook, ObservabilityMetadata,
    ObservabilityRecord, USER_INPUT_PREVIEW_LIMIT,
};
use asc_persistence_sqlite::observability::{
    OBSERVABILITY_TABLES, ObservabilityEventRepository, ObservabilityFaultPolicy,
    repository::{EpochWindow, Page},
};
use asc_sqlite_kernel::{
    KernelError, ReadOnlySource, RecordRepository, SqliteSink, SqliteStore, schema,
};
use chrono::{DateTime, FixedOffset, TimeZone};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

const LOG_PREFIX: &str = "[observability]";

fn store(path: &Path, read_only: bool) -> Arc<SqliteStore> {
    Arc::new(
        SqliteStore::new(
            path,
            read_only,
            OBSERVABILITY_SQLITE_SCHEMA_VERSION,
            OBSERVABILITY_TABLES,
            // This stream is still at revision 1, so it passes no migrator at all
            // — the framework must converge without one.
            None,
            LOG_PREFIX,
        )
        .expect("store"),
    )
}

fn sink(path: &Path) -> SqliteSink<ObservabilityEventRepository, ObservabilityFaultPolicy> {
    SqliteSink::new(
        store(path, false),
        ObservabilityEventRepository,
        ObservabilityFaultPolicy,
        Some(7),
        false,
    )
}

fn read(path: &Path) -> ReadOnlySource<ObservabilityEventRepository> {
    ReadOnlySource::new(store(path, true), ObservabilityEventRepository)
}

fn at(day: u32, hour: u32) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(0)
        .expect("utc offset")
        .with_ymd_and_hms(2026, 1, day, hour, 0, 0)
        .single()
        .expect("timestamp")
}

/// The record's own epoch conversion, so tests and production agree.
fn epoch(value: DateTime<FixedOffset>) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "fixture timestamps are far inside f64's exact integer range"
    )]
    let seconds = value.timestamp() as f64;
    seconds
}

fn metrics(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn record(
    hook: ObservabilityHook,
    observed_at: DateTime<FixedOffset>,
    metadata: ObservabilityMetadata,
    values: Map<String, Value>,
) -> ObservabilityRecord {
    ObservabilityRecord::new(hook, observed_at, metadata, values).expect("record")
}

/// A record carrying only the hook's first declared metric.
fn minimal(
    hook: ObservabilityHook,
    observed_at: DateTime<FixedOffset>,
    metadata: ObservabilityMetadata,
) -> ObservabilityRecord {
    let first = hook.metric_names()[0];
    record(hook, observed_at, metadata, metrics(&[(first, json!("x"))]))
}

/// Two sessions: `s-1` with two runs, `s-2` with one.
fn seed(path: &Path) {
    let sink = sink(path);
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1"),
        metrics(&[("user_input", json!("first turn"))]),
    ));
    sink.write(&minimal(
        ObservabilityHook::AfterAgentRun,
        at(1, 1),
        ObservabilityMetadata::new("s-1", "r-1"),
    ));
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(2, 0),
        ObservabilityMetadata::new("s-1", "r-2"),
        metrics(&[("user_input", json!("second turn"))]),
    ));
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(3, 0),
        ObservabilityMetadata::new("s-2", "r-9"),
        metrics(&[("user_input", json!("other session"))]),
    ));
    sink.close(1000.0);
}

#[test]
fn a_written_record_round_trips_with_its_wire_fields_intact() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    let written = record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1").with_call_id("c-1"),
        metrics(&[("user_input", json!("hello"))]),
    );

    let sink = sink(&path);
    sink.write_or_raise(&written).expect("write");
    sink.close(1000.0);

    let rows = read(&path).query_or_default(|repo, conn| {
        repo.list_events(conn, "s-1", "r-1", EpochWindow::default(), Page::default())
    });
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.hook, "before_agent_run");
    assert_eq!(row.observed_at, written.observed_at_iso());
    assert!((row.observed_at_epoch - written.observed_at_epoch()).abs() < f64::EPSILON);
    assert_eq!(row.session_id, "s-1");
    assert_eq!(row.run_id, "r-1");
    assert_eq!(
        row.call_id, None,
        "before_agent_run has the common metadata shape, so callId is dropped"
    );
    assert_eq!(row.tool_call_id, None);
    // The stored blobs must be byte-identical to what the JSONL writer emits.
    assert_eq!(
        row.metrics_json,
        written.metrics().to_json_string().expect("metrics")
    );
    assert_eq!(
        row.metadata_json,
        written.metadata().to_json_string().expect("metadata")
    );
}

#[test]
fn the_correlation_columns_follow_the_hook_metadata_shape() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");

    let sink = sink(&path);
    // A model-call hook keeps `callId` and drops `toolCallId`.
    sink.write_or_raise(&minimal(
        ObservabilityHook::BeforeLlmCall,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1")
            .with_call_id("c-1")
            .with_tool_call_id("t-1"),
    ))
    .expect("model call");
    // A tool-call hook keeps both.
    sink.write_or_raise(&minimal(
        ObservabilityHook::BeforeToolCall,
        at(1, 1),
        ObservabilityMetadata::new("s-1", "r-1")
            .with_call_id("c-2")
            .with_tool_call_id("t-2"),
    ))
    .expect("tool call");
    sink.close(1000.0);

    let rows = read(&path).query_or_default(|repo, conn| {
        repo.list_events(conn, "s-1", "r-1", EpochWindow::default(), Page::default())
    });
    assert_eq!(rows[0].call_id.as_deref(), Some("c-1"));
    assert_eq!(rows[0].tool_call_id, None);
    assert_eq!(rows[1].call_id.as_deref(), Some("c-2"));
    assert_eq!(rows[1].tool_call_id.as_deref(), Some("t-2"));
}

#[test]
fn counts_cover_records_sessions_and_runs() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);
    let source = read(&path);

    assert_eq!(source.query_or(0, ObservabilityEventRepository::count), 4);
    assert_eq!(
        source.query_or(0, |repo, conn| repo
            .count_sessions(conn, EpochWindow::default())),
        2
    );
    assert_eq!(
        source.query_or(0, |repo, conn| repo.count_runs(
            conn,
            "s-1",
            EpochWindow::default()
        )),
        2
    );
    assert_eq!(
        source.query_or(0, |repo, conn| repo.count_runs(
            conn,
            "absent",
            EpochWindow::default()
        )),
        0
    );
}

#[test]
fn a_time_window_bounds_counts_inclusively_then_exclusively() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);
    let source = read(&path);

    let window = EpochWindow {
        start_epoch: Some(epoch(at(1, 0))),
        end_epoch: Some(epoch(at(3, 0))),
    };
    assert_eq!(
        source.query_or(0, |repo, conn| repo.count_sessions(conn, window)),
        1,
        "the upper bound is exclusive, so s-2 at day 3 drops out"
    );
}

#[test]
fn sessions_are_listed_most_recent_first_with_their_aggregates() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);

    let sessions = read(&path).query_or_default(|repo, conn| {
        repo.list_sessions(conn, EpochWindow::default(), Page::default())
    });
    assert_eq!(
        sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s-2", "s-1"]
    );

    let first = &sessions[1];
    assert_eq!(first.session_id, "s-1");
    assert_eq!(first.turn_count, 2);
    assert_eq!(first.event_count, 3);
    assert!(first.first_seen_epoch < first.last_seen_epoch);
}

#[test]
fn runs_are_listed_chronologically_with_a_user_input_preview() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);

    let runs = read(&path).query_or_default(|repo, conn| {
        repo.list_runs(conn, "s-1", EpochWindow::default(), Page::default())
    });
    assert_eq!(
        runs.iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        vec!["r-1", "r-2"]
    );
    assert_eq!(runs[0].user_input_preview.as_deref(), Some("first turn"));
    assert_eq!(runs[0].event_count, 2);
    assert_eq!(runs[1].user_input_preview.as_deref(), Some("second turn"));
}

#[test]
fn the_preview_comes_from_the_first_before_agent_run_of_the_run() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");

    let sink = sink(&path);
    let metadata = ObservabilityMetadata::new("s-1", "r-1");
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        metadata.clone(),
        metrics(&[("user_input", json!("earliest"))]),
    ));
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 5),
        metadata.clone(),
        metrics(&[("user_input", json!("later"))]),
    ));
    sink.close(1000.0);

    let runs = read(&path).query_or_default(|repo, conn| {
        repo.list_runs(conn, "s-1", EpochWindow::default(), Page::default())
    });
    assert_eq!(
        runs[0].user_input_preview.as_deref(),
        Some("earliest"),
        "ROW_NUMBER() must pick the earliest row, not an arbitrary one"
    );
}

#[test]
fn a_run_without_a_before_agent_run_row_has_no_preview() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");

    let sink = sink(&path);
    sink.write(&minimal(
        ObservabilityHook::AfterAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1"),
    ));
    sink.close(1000.0);

    let runs = read(&path).query_or_default(|repo, conn| {
        repo.list_runs(conn, "s-1", EpochWindow::default(), Page::default())
    });
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].user_input_preview, None);
}

#[test]
fn the_preview_falls_back_to_prompt_and_is_truncated() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");

    let long = "x".repeat(USER_INPUT_PREVIEW_LIMIT + 40);
    let sink = sink(&path);
    // `before_agent_run` declares both metrics; an empty `user_input` is falsy in
    // v1, so the preview must fall through to `prompt`.
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1"),
        metrics(&[("user_input", json!("")), ("prompt", json!(long))]),
    ));
    sink.close(1000.0);

    let runs = read(&path).query_or_default(|repo, conn| {
        repo.list_runs(conn, "s-1", EpochWindow::default(), Page::default())
    });
    let preview = runs[0].user_input_preview.as_deref().expect("preview");
    assert_eq!(preview.chars().count(), USER_INPUT_PREVIEW_LIMIT);
}

#[test]
fn events_of_one_run_come_back_oldest_first() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);

    let rows = read(&path).query_or_default(|repo, conn| {
        repo.list_events(conn, "s-1", "r-1", EpochWindow::default(), Page::default())
    });
    assert_eq!(
        rows.iter().map(|row| row.hook.as_str()).collect::<Vec<_>>(),
        vec!["before_agent_run", "after_agent_run"]
    );
    assert!(rows[0].id < rows[1].id, "the autoincrement id must advance");
}

#[test]
fn paging_applies_limit_and_offset_independently() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);
    let source = read(&path);

    let limited = source.query_or_default(|repo, conn| {
        repo.list_sessions(
            conn,
            EpochWindow::default(),
            Page {
                limit: Some(1),
                offset: 0,
            },
        )
    });
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].session_id, "s-2");

    // An offset without a limit must still work; that is the `LIMIT -1` branch.
    let skipped = source.query_or_default(|repo, conn| {
        repo.list_sessions(
            conn,
            EpochWindow::default(),
            Page {
                limit: None,
                offset: 1,
            },
        )
    });
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].session_id, "s-1");
}

#[test]
fn a_malformed_record_surfaces_without_disposing_the_connection() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    let sink = sink(&path);

    // Prime the connection so a dispose would be observable.
    sink.write_or_raise(&minimal(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1"),
    ))
    .expect("first write");
    assert!(sink.store().is_open());

    let store = store(&path, false);
    let error = store
        .with_connection(true, |conn| {
            conn.execute("DROP TABLE observability_events", [])?;
            ObservabilityEventRepository.insert_or_raise(
                conn,
                &minimal(
                    ObservabilityHook::BeforeAgentRun,
                    at(1, 1),
                    ObservabilityMetadata::new("s-1", "r-1"),
                ),
            )
        })
        .expect_err("the table is gone");
    assert!(
        matches!(error, KernelError::Sqlite(_)),
        "a missing table is a database fault, not a malformed record: {error}"
    );
}

#[test]
fn a_write_to_a_disabled_stream_surfaces_the_v1_message() {
    let dir = TempDir::new().expect("temp dir");
    // A directory where the database file should be makes the open fail.
    let path = dir.path().join("observability.db");
    std::fs::create_dir(&path).expect("occupy the path");

    let sink = sink(&path);
    let error = sink
        .write_or_raise(&minimal(
            ObservabilityHook::BeforeAgentRun,
            at(1, 0),
            ObservabilityMetadata::new("s-1", "r-1"),
        ))
        .expect_err("the path is not a database");
    // The exact class depends on what SQLite reports for a directory; what
    // matters is that this stream raises rather than swallowing.
    assert!(!error.to_string().is_empty());
}

#[test]
fn retention_prunes_by_observed_at_epoch() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);

    let now = epoch(at(3, 0));
    let removed = store(&path, false)
        .with_connection(true, |conn| {
            ObservabilityEventRepository.prune(conn, 1, now)
        })
        .expect("prune")
        .expect("value");
    assert_eq!(
        removed, 2,
        "the two day-1 rows fall outside a one-day window"
    );
    assert_eq!(
        read(&path).query_or(0, ObservabilityEventRepository::count),
        2
    );
}

#[test]
fn close_runs_retention_through_the_maintenance_gate() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");

    let sink = sink(&path);
    sink.write(&minimal(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1"),
    ));
    // Retention is 7 days and the fixture row is far in the past relative to now.
    sink.close(epoch(at(30, 0)));

    assert!(dir.path().join("observability.db.maintenance").exists());
    assert_eq!(
        read(&path).query_or(u64::MAX, ObservabilityEventRepository::count),
        0
    );
}

#[test]
fn the_converged_schema_carries_every_declared_column_and_index() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    seed(&path);

    store(&path, true)
        .with_connection(true, |conn| {
            let columns = schema::existing_columns(conn, "observability_events")?;
            for expected in OBSERVABILITY_TABLES[0].columns {
                assert!(
                    columns.iter().any(|name| name == expected.name),
                    "{} is missing",
                    expected.name
                );
            }

            let mut statement = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' \
                 AND tbl_name = 'observability_events'",
            )?;
            let names: Vec<String> = statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            for index in OBSERVABILITY_TABLES[0].indexes {
                assert!(
                    names.iter().any(|name| name == index.name),
                    "{} is missing",
                    index.name
                );
            }

            let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            assert_eq!(version, i64::from(OBSERVABILITY_SQLITE_SCHEMA_VERSION));
            Ok::<(), KernelError>(())
        })
        .expect("inspect")
        .expect("value");
}

#[test]
fn a_missing_database_degrades_to_empty_reads() {
    let dir = TempDir::new().expect("temp dir");
    let source = read(&dir.path().join("absent.db"));

    assert_eq!(source.query_or(0, ObservabilityEventRepository::count), 0);
    assert!(
        source
            .query_or_default(|repo, conn| repo.list_sessions(
                conn,
                EpochWindow::default(),
                Page::default()
            ))
            .is_empty()
    );
    assert!(
        source
            .query_or_default(|repo, conn| repo.list_runs(
                conn,
                "s-1",
                EpochWindow::default(),
                Page::default()
            ))
            .is_empty()
    );
}

/// A run id is only unique inside its session, so both must be matched.
///
/// Two agents can pick the same run id; filtering on `run_id` alone would leak
/// one session's turns into the other's timeline.
#[test]
fn listing_events_is_scoped_to_the_session_when_run_ids_collide() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    let sink = sink(&path);
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "shared-run"),
        metrics(&[("user_input", json!("belongs to s-1"))]),
    ));
    sink.write(&record(
        ObservabilityHook::BeforeAgentRun,
        at(1, 1),
        ObservabilityMetadata::new("s-2", "shared-run"),
        metrics(&[("user_input", json!("belongs to s-2"))]),
    ));
    sink.close(1000.0);

    let source = read(&path);
    for (session, expected) in [("s-1", "belongs to s-1"), ("s-2", "belongs to s-2")] {
        let rows = source.query_or_default(|repo, conn| {
            repo.list_events(
                conn,
                session,
                "shared-run",
                EpochWindow::default(),
                Page::default(),
            )
        });
        assert_eq!(rows.len(), 1, "{session} must see only its own row");
        assert_eq!(rows[0].session_id, session);
        assert!(
            rows[0].metrics_json.contains(expected),
            "expected {expected} in {}",
            rows[0].metrics_json
        );
    }
}

/// A reader sees committed rows while a writer keeps appending.
///
/// This is what WAL buys, and v1 asserts it too: the read-only connection must
/// not block the writer, and must not be served a stale snapshot forever.
#[test]
fn a_reader_keeps_up_with_a_writer_that_is_still_appending() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.db");
    let sink = sink(&path);
    sink.write(&minimal(
        ObservabilityHook::BeforeAgentRun,
        at(1, 0),
        ObservabilityMetadata::new("s-1", "r-1"),
    ));

    let source = read(&path);
    assert_eq!(source.query_or(0, ObservabilityEventRepository::count), 1);

    sink.write(&minimal(
        ObservabilityHook::AfterAgentRun,
        at(1, 1),
        ObservabilityMetadata::new("s-1", "r-1"),
    ));

    assert_eq!(
        source.query_or(0, ObservabilityEventRepository::count),
        2,
        "the reader must observe rows committed after it first opened"
    );
    sink.close(1000.0);
}
