use super::*;

use crate::record::OperationType;

fn new_recorder() -> (StatsRecorder, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("stats.db");
    let rec = StatsRecorder::new(&db).unwrap();
    (rec, dir)
}

fn sample(op: OperationType, mode: CompressionMode, session: &str) -> StatsRecord {
    StatsRecord::new(op, "cli".to_string(), 1000, 400, 500, 200)
        .with_session_id(session)
        .with_mode(mode)
}

/// Simulate a row that was written under a different UTC offset (or with a
/// sub-millisecond timestamp) than this machine's clock would produce today:
/// rewrite the stored text and keep the derived instant key in sync, exactly
/// as a recorder running at that time would have persisted both.
fn rewrite_timestamp(conn: &rusqlite::Connection, id: i64, text: &str) {
    let ns = chrono::DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap();
    conn.execute(
        "UPDATE stats SET timestamp = ?1, timestamp_ns = ?2 WHERE id = ?3",
        rusqlite::params![text, ns, id],
    )
    .unwrap();
}

#[test]
fn records_and_reads_mode() {
    let (rec, _dir) = new_recorder();
    let id = rec
        .record(&sample(
            OperationType::CompressSchema,
            CompressionMode::DryRun,
            "s1",
        ))
        .unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.mode, CompressionMode::DryRun);
    assert_eq!(got.session_id.as_deref(), Some("s1"));
}

#[test]
fn records_and_reads_stash_fields() {
    let (rec, _dir) = new_recorder();
    let rec_in = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "stash-ses",
    )
    .with_stash(Some(3), Some(0), Some(42));
    let id = rec.record(&rec_in).unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.stash_writes, Some(3));
    assert_eq!(got.stash_errors, Some(0));
    assert_eq!(got.stash_size, Some(42));
}

#[test]
fn stash_fields_default_none_when_unstashed() {
    let (rec, _dir) = new_recorder();
    let id = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "no-stash",
        ))
        .unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.stash_writes, None);
    assert_eq!(got.stash_errors, None);
    assert_eq!(got.stash_size, None);
}

#[test]
fn default_mode_is_active() {
    let (rec, _dir) = new_recorder();
    let id = rec
        .record(&sample(
            OperationType::CompressSchema,
            CompressionMode::Active,
            "s1",
        ))
        .unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.mode, CompressionMode::Active);
}

#[test]
fn records_by_session_filters() {
    let (rec, _dir) = new_recorder();
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "baseline",
    ))
    .unwrap();
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::DryRun,
        "tokenless",
    ))
    .unwrap();
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "baseline",
    ))
    .unwrap();

    let baseline = rec.records_by_session("baseline", None).unwrap();
    let tokenless = rec.records_by_session("tokenless", None).unwrap();
    assert_eq!(baseline.len(), 2);
    assert_eq!(tokenless.len(), 1);
    assert_eq!(tokenless[0].mode, CompressionMode::DryRun);
}

#[test]
fn records_for_diff_filters_tool_and_orders_oldest_first() {
    let (rec, _dir) = new_recorder();
    let first = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "session-diff",
    )
    .with_tool_use_id("tool-a");
    let second = sample(
        OperationType::CompressToon,
        CompressionMode::Active,
        "session-diff",
    )
    .with_tool_use_id("tool-a");
    let other = sample(
        OperationType::CompressSchema,
        CompressionMode::Active,
        "session-diff",
    )
    .with_tool_use_id("tool-b");
    let first_id = rec.record(&first).unwrap();
    let second_id = rec.record(&second).unwrap();
    rec.record(&other).unwrap();

    let records = rec
        .records_for_diff("session-diff", Some("tool-a"))
        .unwrap();
    assert_eq!(records.as_slice().len(), 2);
    assert_eq!(records.as_slice()[0].id, first_id);
    assert_eq!(records.as_slice()[1].id, second_id);

    let session = rec.records_for_diff("session-diff", None).unwrap();
    assert_eq!(session.as_slice().len(), 3);
    assert!(
        session
            .as_slice()
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    );
}

#[test]
fn session_diff_avoids_loading_payloads_and_preserves_links() {
    let (rec, _dir) = new_recorder();
    let middle = "middle".repeat(350_000);
    let first = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "bounded-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("before".to_string(), middle.clone());
    let second = sample(
        OperationType::CompressToon,
        CompressionMode::Active,
        "bounded-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text(middle, "after".to_string());
    rec.record(&first).unwrap();
    rec.record(&second).unwrap();

    let records = rec.records_for_diff("bounded-session", None).unwrap();
    assert!(records.as_slice().iter().all(|record| {
        record.before_text.is_none()
            && record.after_text.is_none()
            && record.before_output.is_none()
            && record.after_output.is_none()
    }));

    let report = crate::diff::session_report(
        &records,
        "bounded-session",
        20,
        crate::diff::DiffSort::Saved,
    );
    let json = serde_json::to_value(report).unwrap();
    assert_eq!(json["chains"].as_array().unwrap().len(), 1);
    assert_eq!(json["chains"][0]["status"], "linked");
}

#[test]
fn session_diff_database_linking_matches_record_semantics() {
    let (rec, _dir) = new_recorder();
    let first = sample(
        OperationType::RewriteCommand,
        CompressionMode::Active,
        "linked-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("legacy before".to_string(), "legacy after".to_string())
    .with_output("raw output".to_string(), "middle".to_string());
    let second = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "linked-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("middle".to_string(), "short".to_string());
    let dry_run = sample(
        OperationType::CompressToon,
        CompressionMode::DryRun,
        "linked-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("short".to_string(), "predicted".to_string());
    rec.record(&first).unwrap();
    rec.record(&second).unwrap();
    rec.record(&dry_run).unwrap();

    let records = rec.records_for_diff("linked-session", None).unwrap();
    let report =
        crate::diff::session_report(&records, "linked-session", 20, crate::diff::DiffSort::Time);
    let json = serde_json::to_value(report).unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 2);
    assert_eq!(json["chains"][0]["status"], "standalone");
    assert_eq!(json["chains"][0]["mode"], "dry-run");
    assert_eq!(json["chains"][1]["status"], "linked");
    assert_eq!(json["chains"][1]["stages"].as_array().unwrap().len(), 2);
}

#[test]
fn session_diff_links_records_written_across_a_utc_offset_change() {
    // A session that spans a UTC-offset change (DST fall-back, travel, or a
    // machine move) persists rfc3339 timestamps whose wall-clock text no
    // longer sorts in instant order: the -05:00 half of the repeated hour
    // text-sorts before the earlier -04:00 half. The database-side link
    // must order by the instant the timestamps denote, not their text, or
    // the prelinked flags describe a different adjacency than the report
    // builder iterates and real compression chains split apart.
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    let first = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "dst-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("before".to_string(), "middle".to_string());
    let second = sample(
        OperationType::CompressToon,
        CompressionMode::Active,
        "dst-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("middle".to_string(), "after".to_string());
    let unrelated = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "dst-session",
    )
    .with_tool_use_id("tool-chain")
    .with_text("unrelated".to_string(), "other".to_string());
    let first_id = rec.record(&first).unwrap();
    let second_id = rec.record(&second).unwrap();
    let third_id = rec.record(&unrelated).unwrap();

    // 01:30-04:00 (05:30Z) precedes 01:15-05:00 (06:15Z) in instant order,
    // but sorts after it lexicographically.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        rewrite_timestamp(&conn, first_id, "2026-11-01T01:30:00.000000-04:00");
        rewrite_timestamp(&conn, second_id, "2026-11-01T01:15:00.000000-05:00");
        rewrite_timestamp(&conn, third_id, "2026-11-01T01:45:00.000000-05:00");
    }

    let records = rec.records_for_diff("dst-session", None).unwrap();
    let report =
        crate::diff::session_report(&records, "dst-session", 20, crate::diff::DiffSort::Time);
    let json = serde_json::to_value(report).unwrap();
    let chains = json["chains"].as_array().unwrap();

    assert_eq!(chains.len(), 2, "chains: {chains:?}");
    let linked = chains
        .iter()
        .find(|chain| chain["status"] == "linked")
        .unwrap_or_else(|| panic!("no linked chain in {chains:?}"));
    assert_eq!(linked["stages"].as_array().unwrap().len(), 2);
    assert_eq!(
        chains
            .iter()
            .filter(|chain| chain["status"] == "standalone")
            .count(),
        1
    );
}

#[test]
fn newest_record_windows_span_a_utc_offset_change() {
    // A limited newest-first window must keep the records with the latest
    // instants. Text ordering of rfc3339 timestamps spans offsets by
    // wall clock only, so during a fall-back the repeated hour's -05:00
    // text sorts before the earlier -04:00 text and would let an older
    // record displace a newer one from the window.
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    let oldest = sample(
        OperationType::CompressSchema,
        CompressionMode::Active,
        "dst-list",
    );
    let middle = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "dst-list",
    );
    let newest = sample(
        OperationType::CompressToon,
        CompressionMode::Active,
        "dst-list",
    );
    let oldest_id = rec.record(&oldest).unwrap();
    let middle_id = rec.record(&middle).unwrap();
    let newest_id = rec.record(&newest).unwrap();

    // Instants: oldest 05:30Z, middle 06:15Z, newest 06:45Z; the middle
    // record's -05:00 text sorts before the oldest record's -04:00 text.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        rewrite_timestamp(&conn, oldest_id, "2026-11-01T01:30:00.000000-04:00");
        rewrite_timestamp(&conn, middle_id, "2026-11-01T01:15:00.000000-05:00");
        rewrite_timestamp(&conn, newest_id, "2026-11-01T01:45:00.000000-05:00");
    }

    let window: Vec<i64> = rec
        .all_records(Some(2))
        .unwrap()
        .iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(window, vec![newest_id, middle_id]);

    let session_window: Vec<i64> = rec
        .records_by_session("dst-list", Some(2))
        .unwrap()
        .iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(session_window, vec![newest_id, middle_id]);
}

#[test]
fn newest_record_windows_separate_submillisecond_timestamps() {
    // SQLite's date functions only parse timestamps to whole milliseconds,
    // so a millisecond-collapsed key orders sub-millisecond records by their
    // insert id instead of their instant. Records get their timestamp when
    // they are constructed, before the recorder lock serializes the
    // inserts, so insertion order can differ from instant order: a limited
    // window must still keep the genuinely newer record, and the diff
    // window must link the chain in instant order.
    let (rec, _dir) = new_recorder();
    let later_instant = chrono::DateTime::parse_from_rfc3339("2026-11-15T01:15:00.000200+00:00")
        .unwrap()
        .with_timezone(&chrono::Local);
    let earlier_instant = chrono::DateTime::parse_from_rfc3339("2026-11-15T01:15:00.000100+00:00")
        .unwrap()
        .with_timezone(&chrono::Local);
    // Inserted first (lower id) but 100ns later than the second record.
    let newer = sample(
        OperationType::CompressToon,
        CompressionMode::Active,
        "subms-session",
    )
    .with_tool_use_id("tool-subms")
    .with_text("middle".to_string(), "after".to_string())
    .with_timestamp(later_instant);
    let older = sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "subms-session",
    )
    .with_tool_use_id("tool-subms")
    .with_text("before".to_string(), "middle".to_string())
    .with_timestamp(earlier_instant);
    let newer_id = rec.record(&newer).unwrap();
    let older_id = rec.record(&older).unwrap();
    assert!(newer_id < older_id);

    let window: Vec<i64> = rec
        .all_records(Some(1))
        .unwrap()
        .iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(window, vec![newer_id]);

    let session_window: Vec<i64> = rec
        .records_by_session("subms-session", Some(1))
        .unwrap()
        .iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(session_window, vec![newer_id]);

    let records = rec.records_for_diff("subms-session", None).unwrap();
    let report =
        crate::diff::session_report(&records, "subms-session", 20, crate::diff::DiffSort::Time);
    let json = serde_json::to_value(report).unwrap();
    let chains = json["chains"].as_array().unwrap();
    assert_eq!(chains.len(), 1, "chains: {chains:?}");
    assert_eq!(chains[0]["status"], "linked");
    let stages = chains[0]["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    assert_eq!(stages[0]["record_id"], older_id);
    assert_eq!(stages[1]["record_id"], newer_id);
}

#[test]
fn newest_record_windows_read_through_the_instant_index() {
    // A limited newest-first window must read the newest rows through
    // idx_stats_instant instead of scanning the payload-bearing table and
    // sorting it: the plan for the window's ordering must not fall back to
    // a temporary b-tree.
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "idx-session",
    ))
    .unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut stmt = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT id FROM stats \
             ORDER BY timestamp_ns DESC, id DESC LIMIT 20",
        )
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let plan = plan.join(" | ");
    assert!(plan.contains("idx_stats_instant"), "plan: {plan}");
    assert!(!plan.contains("TEMP B-TREE"), "plan: {plan}");
}

#[test]
fn opening_a_legacy_database_backfills_the_instant_key() {
    // Databases written before the instant key existed carry offset-bearing
    // text with no timestamp_ns column. Opening one derives each key from
    // the stored text at full chrono precision, so legacy rows order
    // exactly like rows the recorder writes today, while text that never
    // parses keeps the unparseable sentinel and still sorts last in
    // ascending order.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let rec = StatsRecorder::new(&db_path).unwrap();
    let first_id = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "legacy",
        ))
        .unwrap();
    let second_id = rec
        .record(&sample(
            OperationType::CompressSchema,
            CompressionMode::Active,
            "legacy",
        ))
        .unwrap();
    let corrupt_id = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "legacy",
        ))
        .unwrap();
    {
        // A legacy writer had no instant-key column: rewrite only the
        // stored text and let the reopen backfill derive the keys.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("DROP INDEX idx_stats_instant", []).unwrap();
        conn.execute("DROP INDEX idx_stats_text_instant", [])
            .unwrap();
        conn.execute("ALTER TABLE stats DROP COLUMN timestamp_ns", [])
            .unwrap();
        conn.execute(
            "UPDATE stats SET timestamp = '2026-11-15T01:30:00.000000-04:00' WHERE id = ?1",
            [first_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE stats SET timestamp = '2026-11-15T01:15:00.000000-05:00' WHERE id = ?1",
            [second_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE stats SET timestamp = 'not-a-date' WHERE id = ?1",
            [corrupt_id],
        )
        .unwrap();
    }
    drop(rec);

    let rec = StatsRecorder::new(&db_path).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for (id, text) in [
            (first_id, "2026-11-15T01:30:00.000000-04:00"),
            (second_id, "2026-11-15T01:15:00.000000-05:00"),
        ] {
            let expected = chrono::DateTime::parse_from_rfc3339(text)
                .unwrap()
                .timestamp_nanos_opt()
                .unwrap();
            let key: i64 = conn
                .query_row(
                    "SELECT timestamp_ns FROM stats WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(key, expected, "legacy key for row {id}");
        }
        let corrupt_key: i64 = conn
            .query_row(
                "SELECT timestamp_ns FROM stats WHERE id = ?1",
                [corrupt_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(corrupt_key, i64::MAX);
    }
    // 01:15-05:00 (06:15Z) is newer than 01:30-04:00 (05:30Z); the corrupt
    // sentinel sorts first in newest-first windows, where the previous text
    // comparison already placed it.
    let window: Vec<i64> = rec
        .all_records(Some(2))
        .unwrap()
        .iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(window, vec![corrupt_id, second_id]);
}

#[test]
fn records_for_diff_caps_to_newest_records() {
    let (rec, _dir) = new_recorder();
    for _ in 0..(StatsRecorder::DEFAULT_LIMIT + 1) {
        rec.record(&sample(
            OperationType::CompressSchema,
            CompressionMode::Active,
            "large-session",
        ))
        .unwrap();
    }

    let records = rec.records_for_diff("large-session", None).unwrap();
    assert_eq!(records.as_slice().len(), StatsRecorder::DEFAULT_LIMIT);
    assert_eq!(records.as_slice()[0].id, 2);
}

#[test]
fn count_returns_total_records() {
    let (rec, _dir) = new_recorder();
    assert_eq!(rec.count().unwrap(), 0);
    rec.record(&sample(
        OperationType::CompressSchema,
        CompressionMode::Active,
        "s1",
    ))
    .unwrap();
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "s1",
    ))
    .unwrap();
    assert_eq!(rec.count().unwrap(), 2);
}

#[test]
fn clear_removes_all_records() {
    let (rec, _dir) = new_recorder();
    rec.record(&sample(
        OperationType::CompressSchema,
        CompressionMode::Active,
        "s1",
    ))
    .unwrap();
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "s1",
    ))
    .unwrap();
    assert_eq!(rec.count().unwrap(), 2);
    rec.clear().unwrap();
    assert_eq!(rec.count().unwrap(), 0);
}

#[test]
fn all_records_with_limit() {
    let (rec, _dir) = new_recorder();
    for _ in 0..5 {
        rec.record(&sample(
            OperationType::CompressSchema,
            CompressionMode::Active,
            "s1",
        ))
        .unwrap();
    }
    let all = rec.all_records(None).unwrap();
    assert_eq!(all.len(), 5);
    let limited = rec.all_records(Some(3)).unwrap();
    assert_eq!(limited.len(), 3);
}

#[test]
fn record_by_id_missing_returns_none() {
    let (rec, _dir) = new_recorder();
    assert!(rec.record_by_id(9999).unwrap().is_none());
}

#[test]
fn summary_from_empty_records() {
    let summary = StatsSummary::from_records(&[]);
    assert_eq!(summary.total_records, 0);
    assert_eq!(summary.chars_saved(), 0);
    assert_eq!(summary.tokens_saved(), 0);
    assert_eq!(summary.chars_percent(), 0.0);
    assert_eq!(summary.tokens_percent(), 0.0);
}

#[test]
fn summary_from_records_aggregates() {
    let records = vec![
        StatsRecord::new(
            OperationType::CompressSchema,
            "a".into(),
            1000,
            400,
            500,
            200,
        ),
        StatsRecord::new(
            OperationType::CompressResponse,
            "b".into(),
            2000,
            800,
            1000,
            400,
        ),
    ];
    let summary = StatsSummary::from_records(&records);
    assert_eq!(summary.total_records, 2);
    assert_eq!(summary.total_before_chars, 3000);
    assert_eq!(summary.total_after_chars, 1500);
    assert_eq!(summary.total_before_tokens, 1200);
    assert_eq!(summary.total_after_tokens, 600);
    assert_eq!(summary.chars_saved(), 1500);
    assert_eq!(summary.tokens_saved(), 600);
    assert!((summary.chars_percent() - 50.0).abs() < 0.1);
    assert!((summary.tokens_percent() - 50.0).abs() < 0.1);
}

#[test]
fn summary_zero_before_returns_zero_percent() {
    let summary = StatsSummary {
        total_records: 1,
        total_before_chars: 0,
        total_after_chars: 0,
        total_before_tokens: 0,
        total_after_tokens: 0,
    };
    assert_eq!(summary.chars_percent(), 0.0);
    assert_eq!(summary.tokens_percent(), 0.0);
}

#[test]
fn actual_savings_percent_zero_session_total() {
    let summary = StatsSummary {
        total_records: 1,
        total_before_chars: 1000,
        total_after_chars: 500,
        total_before_tokens: 400,
        total_after_tokens: 200,
    };
    assert_eq!(summary.actual_savings_percent(0), 0.0);
    let pct = summary.actual_savings_percent(2000);
    assert!((pct - 10.0).abs() < 0.1);
}

#[test]
fn schema_migration_adds_missing_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("migrate.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                operation TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                source_pid INTEGER,
                session_id TEXT,
                tool_use_id TEXT,
                before_chars INTEGER NOT NULL,
                before_tokens INTEGER NOT NULL,
                after_chars INTEGER NOT NULL,
                after_tokens INTEGER NOT NULL,
                before_text TEXT,
                after_text TEXT
            )",
        )
        .unwrap();
    }
    // A row written by the legacy schema must survive the migration
    // untouched and read back with every migrated column as None.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO stats (
                timestamp, operation, agent_id, before_chars, before_tokens,
                after_chars, after_tokens
            ) VALUES ('2024-01-01T00:00:00+00:00', 'compress-response', 'old', 10, 4, 5, 2)",
            [],
        )
        .unwrap();
    }
    let rec = StatsRecorder::new(&db_path).unwrap();
    let legacy = rec.record_by_id(1).unwrap().unwrap();
    assert_eq!(legacy.agent_id, "old");
    assert_eq!(legacy.before_tokens, 4);
    assert_eq!(legacy.stash_writes, None);
    assert_eq!(legacy.content_type, None);
    assert_eq!(legacy.applied_operations, None);
    assert_eq!(legacy.recoverability, None);
    assert_eq!(legacy.tokenizer_id, None);
    assert_eq!(legacy.unrecoverable_truncations, None);

    let record = StatsRecord::new(OperationType::CompressSchema, "cli".into(), 100, 25, 50, 12)
        .with_mode(CompressionMode::Active)
        .with_stash(Some(1), Some(0), Some(5));
    let id = rec.record(&record).unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.mode, CompressionMode::Active);
    assert_eq!(got.stash_writes, Some(1));

    // The §4.6 tables arrive with the migration too.
    {
        let conn = rec.lock_conn();
        for table in ["compression_artifacts", "retrieve_events"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
    }

    let conn = rec.lock_conn();
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_index_info('idx_session_tool') ORDER BY seqno")
        .unwrap();
    let indexed_columns = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(indexed_columns, ["session_id", "tool_use_id"]);
}

fn create_legacy_stats_database(path: &Path, extra_retrieve_columns: usize) -> Connection {
    let conn = Connection::open(path).unwrap();
    let extra_columns = (0..extra_retrieve_columns)
        .map(|index| format!(", extra_{index} TEXT"))
        .collect::<String>();
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE stats (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             timestamp TEXT NOT NULL,
             operation TEXT NOT NULL,
             agent_id TEXT NOT NULL,
             source_pid INTEGER,
             session_id TEXT,
             tool_use_id TEXT,
             before_chars INTEGER NOT NULL,
             before_tokens INTEGER NOT NULL,
             after_chars INTEGER NOT NULL,
             after_tokens INTEGER NOT NULL,
             before_text TEXT,
             after_text TEXT
         );
         INSERT INTO stats (
             timestamp, operation, agent_id, before_chars, before_tokens,
             after_chars, after_tokens
         ) VALUES ('2024-01-01T00:00:00+00:00', 'compress-response', 'old', 10, 4, 5, 2);
         CREATE TABLE retrieve_events (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             timestamp TEXT NOT NULL,
             hash TEXT NOT NULL,
             outcome TEXT NOT NULL,
             source TEXT NOT NULL,
             payload_tokens INTEGER,
             tokenizer_id TEXT{extra_columns}
         );
         INSERT INTO retrieve_events (
             timestamp, hash, outcome, source, payload_tokens
         ) VALUES ('2024-01-01T00:00:00+00:00', 'old-hash', 'hit', 'cli', 120);",
    ))
    .unwrap();
    conn
}

fn record_concurrently(path: &Path, workers: usize) {
    let barrier = std::sync::Barrier::new(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let barrier = &barrier;
                scope.spawn(move || -> StatsResult<()> {
                    barrier.wait();
                    let recorder = StatsRecorder::new(path)?;
                    recorder.record(&sample(
                        OperationType::CompressResponse,
                        CompressionMode::Active,
                        "concurrent",
                    ))?;
                    recorder.record_retrieve_event(
                        "new-hash",
                        "hit",
                        "cli",
                        Some(10),
                        None,
                        Some("new-agent"),
                        Some("concurrent"),
                        Some("tool"),
                    )?;
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
    });
}

#[test]
fn concurrent_legacy_migrations_preserve_historical_rows() {
    let dir = tempfile::tempdir().unwrap();
    for round in 0..4 {
        let path = dir.path().join(format!("legacy-{round}.db"));
        drop(create_legacy_stats_database(&path, 0));
        record_concurrently(&path, 8);

        let recorder = StatsRecorder::new(&path).unwrap();
        let legacy = recorder.record_by_id(1).unwrap().unwrap();
        assert_eq!(legacy.agent_id, "old");
        assert_eq!(legacy.before_tokens, 4);
        assert_eq!(legacy.after_tokens, 2);
        assert_eq!(legacy.applied_operations, None);
        assert_eq!(recorder.count().unwrap(), 9);
        assert_eq!(
            recorder.retrieve_totals().unwrap(),
            RetrieveTotals {
                hits: 9,
                retrieved_tokens: 200,
                ..RetrieveTotals::default()
            }
        );
    }
}

#[test]
fn concurrent_fresh_openers_keep_all_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh.db");
    record_concurrently(&path, 8);

    let recorder = StatsRecorder::new(&path).unwrap();
    assert_eq!(recorder.count().unwrap(), 8);
    assert_eq!(recorder.retrieve_totals().unwrap().hits, 8);
    assert_eq!(recorder.retrieve_totals().unwrap().retrieved_tokens, 80);
}

#[test]
fn wal_contention_obeys_one_wait_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("busy.db");
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("CREATE TABLE existing (value TEXT); BEGIN; SELECT * FROM existing;")
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let connection = Connection::open(path).unwrap();
        let started = Instant::now();
        let result = StatsRecorder::enable_wal(&connection, Duration::from_millis(100));
        sender.send((result, started.elapsed())).unwrap();
    });
    let outcome = receiver.recv_timeout(Duration::from_secs(2));
    // Release the lock before asserting, so an unbounded retry still has a
    // way to finish and cannot strand a test thread on the failure path.
    drop(reader);
    worker.join().unwrap();
    let (result, elapsed) = outcome.expect("WAL initialization exceeded its wait budget");
    assert!(matches!(
        result,
        Err(StatsError::Database(rusqlite::Error::SqliteFailure(code, _)))
            if code.code == rusqlite::ErrorCode::DatabaseBusy
    ));
    assert!(elapsed >= Duration::from_millis(70));
}

#[test]
fn wal_initialization_preserves_non_busy_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE existing (value TEXT);")
        .unwrap();
    drop(connection);
    let readonly =
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(matches!(
        StatsRecorder::enable_wal(&readonly, Duration::from_millis(100)),
        Err(StatsError::Database(rusqlite::Error::SqliteFailure(code, _)))
            if code.code == rusqlite::ErrorCode::ReadOnly
    ));
}

#[test]
fn failed_retrieve_migration_rolls_back_stats_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollback.db");
    let conn = Connection::open(&path).unwrap();
    let max_column: String = conn
        .query_row(
            "SELECT compile_options FROM pragma_compile_options
             WHERE compile_options LIKE 'MAX_COLUMN=%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let max_column: usize = max_column
        .strip_prefix("MAX_COLUMN=")
        .unwrap()
        .parse()
        .unwrap();
    drop(conn);
    // Exhaust the second table's column limit so its migration fails after
    // stats has already added columns. Both tables must roll back together.
    let conn = create_legacy_stats_database(&path, max_column - 7);
    assert!(matches!(
        StatsRecorder::new(&path),
        Err(StatsError::Database(_))
    ));
    let added: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = 'before_output'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(added, 0, "failed migration left stats partially upgraded");

    for index in 0..4 {
        conn.execute(
            &format!("ALTER TABLE retrieve_events DROP COLUMN extra_{index}"),
            [],
        )
        .unwrap();
    }
    let recorder = StatsRecorder::new(&path).unwrap();
    assert_eq!(recorder.record_by_id(1).unwrap().unwrap().agent_id, "old");
    assert_eq!(recorder.count().unwrap(), 1);
    assert_eq!(recorder.retrieve_totals().unwrap().retrieved_tokens, 120);
}

#[test]
fn all_records_handles_corrupt_row() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("corrupt.db");
    let rec = StatsRecorder::new(&db_path).unwrap();
    rec.record(&sample(
        OperationType::CompressSchema,
        CompressionMode::Active,
        "s1",
    ))
    .unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO stats (timestamp, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens)
             VALUES ('not-a-date', 'compress_schema', 'cli', 100, 25, 50, 12)",
            [],
        )
        .unwrap();
    }
    let records = rec.all_records(None).unwrap();
    assert!(!records.is_empty());
}

#[test]
fn record_with_before_after_output() {
    let (rec, _dir) = new_recorder();
    let record = StatsRecord::new(OperationType::CompressSchema, "cli".into(), 100, 25, 50, 12)
        .with_before_text("before-text".to_string())
        .with_after_text("after-text".to_string())
        .with_output("before-output".to_string(), "after-output".to_string());
    let id = rec.record(&record).unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.before_text.as_deref(), Some("before-text"));
    assert_eq!(got.after_text.as_deref(), Some("after-text"));
    assert_eq!(got.before_output.as_deref(), Some("before-output"));
    assert_eq!(got.after_output.as_deref(), Some("after-output"));
}

#[test]
fn entry_metadata_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let rec = StatsRecorder::new(dir.path().join("stats.db")).unwrap();
    let record = StatsRecord::new(
        OperationType::CompressResponse,
        "cli".into(),
        100,
        40,
        50,
        20,
    )
    .with_entry_metadata(
        Some("api-records".into()),
        Some("command_output".into()),
        Some(vec!["json_cleanup".into(), "toon".into()]),
        Some("lossless".into()),
        "heuristic-v1",
        Some(2),
    );
    let id = rec.record(&record).unwrap();
    let got = rec.record_by_id(id).unwrap().unwrap();
    assert_eq!(got.content_type.as_deref(), Some("api-records"));
    assert_eq!(got.content_origin.as_deref(), Some("command_output"));
    assert_eq!(
        got.applied_operations.as_deref(),
        Some(["json_cleanup".to_string(), "toon".to_string()].as_slice())
    );
    assert_eq!(got.recoverability.as_deref(), Some("lossless"));
    assert_eq!(got.tokenizer_id.as_deref(), Some("heuristic-v1"));
    assert_eq!(got.unrecoverable_truncations, Some(2));
}

#[test]
fn artifacts_attach_to_their_stats_row() {
    let dir = tempfile::tempdir().unwrap();
    let rec = StatsRecorder::new(dir.path().join("stats.db")).unwrap();
    let id = rec
        .record(&StatsRecord::new(
            OperationType::CompressResponse,
            "cli".into(),
            100,
            40,
            50,
            20,
        ))
        .unwrap();
    let hashes = vec!["a".repeat(24), "b".repeat(24)];
    rec.record_artifacts(id, "response-cleanup", &hashes)
        .unwrap();

    let conn = rec.lock_conn();
    let rows: Vec<(i64, String, String, i64)> = conn
        .prepare("SELECT stats_id, hash, compressor_id, emitted FROM compression_artifacts ORDER BY hash")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (id, "a".repeat(24), "response-cleanup".into(), 1),
            (id, "b".repeat(24), "response-cleanup".into(), 1),
        ]
    );
}

#[test]
fn retrieve_events_aggregate_into_totals() {
    let dir = tempfile::tempdir().unwrap();
    let rec = StatsRecorder::new(dir.path().join("stats.db")).unwrap();
    let hash = "c".repeat(24);
    rec.record_retrieve_event(
        &hash,
        "hit",
        "cli",
        Some(120),
        Some("heuristic-v1"),
        Some("agent"),
        Some("session"),
        Some("tool"),
    )
    .unwrap();
    rec.record_retrieve_event(
        &hash,
        "hit",
        "cli",
        Some(80),
        Some("heuristic-v1"),
        None,
        None,
        None,
    )
    .unwrap();
    rec.record_retrieve_event(&hash, "miss", "embedded", None, None, None, None, None)
        .unwrap();
    rec.record_retrieve_event(&hash, "error", "cli", None, None, None, None, None)
        .unwrap();

    let totals = rec.retrieve_totals().unwrap();
    assert_eq!(
        totals,
        RetrieveTotals {
            hits: 2,
            misses: 1,
            errors: 1,
            retrieved_tokens: 200,
        }
    );
}

#[test]
fn clear_empties_the_attribution_tables() {
    let dir = tempfile::tempdir().unwrap();
    let rec = StatsRecorder::new(dir.path().join("stats.db")).unwrap();
    let id = rec
        .record(&StatsRecord::new(
            OperationType::CompressResponse,
            "cli".into(),
            100,
            40,
            50,
            20,
        ))
        .unwrap();
    rec.record_artifacts(id, "response-cleanup", &["d".repeat(24)])
        .unwrap();
    rec.record_retrieve_event(
        &"d".repeat(24),
        "hit",
        "cli",
        Some(10),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    rec.clear().unwrap();
    assert_eq!(rec.count().unwrap(), 0);
    assert_eq!(rec.retrieve_totals().unwrap(), RetrieveTotals::default());
    let conn = rec.lock_conn();
    let artifacts: i64 = conn
        .query_row("SELECT COUNT(*) FROM compression_artifacts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(artifacts, 0);
}

/// A stats or SLS warning must not be able to fail the command when
/// stderr itself cannot be written (a full filesystem behind redirected
/// logs, a closed descriptor). `eprintln!` panics on that shape, turning
/// the fail-soft warning into a process failure while the compression
/// output is fine; `warn_stats` must survive it. Redirect fd 2 to
/// /dev/full, call the helper, restore, and assert the call returned —
/// a panicking write makes this test fail.
#[test]
#[cfg(target_os = "linux")]
fn warn_stats_survives_an_unwritable_stderr() {
    use std::os::unix::io::AsRawFd;

    let dev_full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let saved = unsafe { libc::dup(2) };
    assert!(saved >= 0, "dup stderr");
    unsafe {
        libc::dup2(dev_full.as_raw_fd(), 2);
    }
    // The helper's whole contract: no panic, the write error is
    // discarded.
    crate::warn_stats("[tokenless-stats] WARNING: simulated stderr failure");
    // Restore before any assert can print.
    let restored = unsafe { libc::dup2(saved, 2) };
    let _ = std::io::Write::flush(&mut std::io::stderr());
    assert!(restored >= 0, "restore stderr");
    unsafe {
        libc::close(saved);
    }
    drop(dev_full);
}

#[test]
fn parse_busy_timeout_bounds_test_overrides() {
    assert_eq!(parse_busy_timeout(None), Duration::from_secs(5));
    assert_eq!(parse_busy_timeout(Some("")), Duration::from_secs(5));
    assert_eq!(parse_busy_timeout(Some("abc")), Duration::from_secs(5));
    assert_eq!(parse_busy_timeout(Some("0")), Duration::from_secs(5));
    assert_eq!(parse_busy_timeout(Some("9")), Duration::from_secs(5));
    assert_eq!(parse_busy_timeout(Some("10")), Duration::from_millis(10));
    assert_eq!(parse_busy_timeout(Some("200")), Duration::from_millis(200));
    assert_eq!(
        parse_busy_timeout(Some("5000")),
        Duration::from_millis(5000)
    );
    assert_eq!(parse_busy_timeout(Some("5001")), Duration::from_secs(5));
}

fn fixed_local_time(rfc3339: &str) -> DateTime<Local> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&Local)
}

#[test]
fn prune_older_than_removes_old_stats_artifacts_and_retrieve_events() {
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    let now = fixed_local_time("2026-11-26T12:00:00+00:00");
    let old_ts = fixed_local_time("2026-10-15T12:00:00+00:00"); // 42 days old
    let recent_ts = fixed_local_time("2026-11-20T12:00:00+00:00"); // 6 days old

    let old_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "s1",
            )
            .with_timestamp(old_ts)
            .with_text("old-before".into(), "old-after".into()),
        )
        .unwrap();
    let recent_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "s1",
            )
            .with_timestamp(recent_ts)
            .with_text("new-before".into(), "new-after".into()),
        )
        .unwrap();

    rec.record_artifacts(old_id, "json_truncation", &["a".repeat(24)])
        .unwrap();
    rec.record_artifacts(recent_id, "json_truncation", &["b".repeat(24)])
        .unwrap();

    let old_retrieve_id = rec
        .record_retrieve_event(
            &"a".repeat(24),
            "hit",
            "cli",
            Some(50),
            None,
            None,
            Some("s1"),
            None,
        )
        .unwrap();
    let recent_retrieve_id = rec
        .record_retrieve_event(
            &"b".repeat(24),
            "hit",
            "cli",
            Some(25),
            None,
            None,
            Some("s1"),
            None,
        )
        .unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let old_ns = old_ts.timestamp_nanos_opt().unwrap();
        let recent_ns = recent_ts.timestamp_nanos_opt().unwrap();
        conn.execute(
            "UPDATE retrieve_events SET timestamp = ?1, timestamp_ns = ?2 WHERE id = ?3",
            rusqlite::params![old_ts.to_rfc3339(), old_ns, old_retrieve_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE retrieve_events SET timestamp = ?1, timestamp_ns = ?2 WHERE id = ?3",
            rusqlite::params![recent_ts.to_rfc3339(), recent_ns, recent_retrieve_id],
        )
        .unwrap();
    }

    let report = rec
        .apply_retention_at(
            now,
            RetentionPolicy {
                row_retention_days: Some(30),
                text_retention_days: None,
            },
            false,
        )
        .unwrap();
    assert_eq!(
        report,
        PruneReport {
            stats_rows_deleted: 1,
            text_rows_trimmed: 0,
            artifacts_deleted: 1,
            retrieve_events_deleted: 1,
        }
    );

    assert!(rec.record_by_id(old_id).unwrap().is_none());
    assert!(rec.record_by_id(recent_id).unwrap().is_some());
    assert_eq!(rec.count().unwrap(), 1);
    assert_eq!(
        rec.retrieve_totals().unwrap(),
        RetrieveTotals {
            hits: 1,
            retrieved_tokens: 25,
            ..RetrieveTotals::default()
        }
    );
    let conn = rec.lock_conn();
    let remaining_artifacts: Vec<i64> = conn
        .prepare("SELECT stats_id FROM compression_artifacts")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(remaining_artifacts, vec![recent_id]);
}

#[test]
fn trim_text_older_than_clears_payloads_keeps_metrics_and_prunes_orphans() {
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    let now = fixed_local_time("2026-11-26T12:00:00+00:00");
    let old_ts = fixed_local_time("2026-11-10T12:00:00+00:00"); // 16 days old
    let recent_ts = fixed_local_time("2026-11-25T12:00:00+00:00"); // 1 day old

    let old_id = rec
        .record(
            &sample(
                OperationType::RewriteCommand,
                CompressionMode::Active,
                "trim-s",
            )
            .with_timestamp(old_ts)
            .with_tool_use_id("tool-old")
            .with_text("old-before".into(), "old-after".into())
            .with_output("old-out-before".into(), "old-out-after".into()),
        )
        .unwrap();
    let recent_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "trim-s",
            )
            .with_timestamp(recent_ts)
            .with_tool_use_id("tool-new")
            .with_text("new-before".into(), "new-after".into()),
        )
        .unwrap();

    rec.record_artifacts(old_id, "json_truncation", &["a".repeat(24)])
        .unwrap();
    rec.record_retrieve_event(
        &"a".repeat(24),
        "hit",
        "cli",
        Some(40),
        None,
        None,
        Some("trim-s"),
        None,
    )
    .unwrap();
    {
        // Simulate a pre-existing orphan artifact row whose stats row was previously deleted.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO compression_artifacts (stats_id, hash, compressor_id, emitted)
             VALUES (999999, ?1, 'json_truncation', 1)",
            ["f".repeat(24)],
        )
        .unwrap();
    }

    let report = rec
        .apply_retention_at(
            now,
            RetentionPolicy {
                row_retention_days: None,
                text_retention_days: Some(7),
            },
            true,
        )
        .unwrap();
    assert_eq!(
        report,
        PruneReport {
            stats_rows_deleted: 0,
            text_rows_trimmed: 1,
            artifacts_deleted: 1,
            retrieve_events_deleted: 0,
        }
    );

    // Repeating the trim immediately finds zero remaining untrimmed rows.
    let second = rec
        .apply_retention_at(
            now,
            RetentionPolicy {
                row_retention_days: None,
                text_retention_days: Some(7),
            },
            true,
        )
        .unwrap();
    assert!(second.is_empty());

    let trimmed = rec.record_by_id(old_id).unwrap().unwrap();
    assert_eq!(trimmed.before_chars, 1000);
    assert_eq!(trimmed.before_tokens, 400);
    assert_eq!(trimmed.after_chars, 500);
    assert_eq!(trimmed.after_tokens, 200);
    assert!(trimmed.before_text.is_none());
    assert!(trimmed.after_text.is_none());
    assert!(trimmed.before_output.is_none());
    assert!(trimmed.after_output.is_none());

    let recent = rec.record_by_id(recent_id).unwrap().unwrap();
    assert_eq!(recent.before_text.as_deref(), Some("new-before"));
    assert_eq!(recent.after_text.as_deref(), Some("new-after"));

    // Surviving row's artifact and retrieve_events stay intact.
    assert_eq!(rec.retrieve_totals().unwrap().hits, 1);
    {
        let conn = rec.lock_conn();
        let artifact_ids: Vec<i64> = conn
            .prepare("SELECT stats_id FROM compression_artifacts")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(artifact_ids, vec![old_id]);
    }

    // Query/formatting/diff/reader paths handle trimmed NULL text gracefully.
    let show_out = crate::query::format_show(&trimmed);
    assert!(show_out.contains("(no text content stored)"), "{show_out}");

    let all = rec.all_records(None).unwrap();
    let summary = StatsSummary::from_records(&all);
    assert_eq!(summary.total_records, 2);
    assert_eq!(summary.total_before_tokens, 800);
    assert_eq!(summary.total_after_tokens, 400);

    let single_diff = crate::diff::record_report(&trimmed, 3);
    let single_diff_json = serde_json::to_value(single_diff).unwrap();
    assert_eq!(single_diff_json["chains"][0]["diff"]["available"], false);
    assert_eq!(
        single_diff_json["chains"][0]["diff"]["omitted_reason"],
        "missing-content"
    );

    let session_diff_records = rec.records_for_diff("trim-s", None).unwrap();
    let session_diff = crate::diff::session_report(
        &session_diff_records,
        "trim-s",
        20,
        crate::diff::DiffSort::Time,
    );
    let session_diff_json = serde_json::to_value(session_diff).unwrap();
    assert_eq!(session_diff_json["chains"].as_array().unwrap().len(), 2);

    let reader = crate::reader::StatsReader::open(&db_path).unwrap();
    let page = reader.list(&crate::reader::RecordQuery::default()).unwrap();
    assert_eq!(page.records.len(), 2);
    let fetched = reader.get(old_id, true).unwrap().unwrap();
    assert!(fetched["before_text"].is_null());
    assert_eq!(fetched["before_tokens"], 400);
}

#[test]
fn apply_retention_combines_row_expiry_and_text_trimming() {
    let (rec, _dir) = new_recorder();
    let now = fixed_local_time("2026-11-26T12:00:00+00:00");
    let expired_ts = fixed_local_time("2026-10-20T12:00:00+00:00"); // 37d old -> deleted
    let mid_ts = fixed_local_time("2026-11-15T12:00:00+00:00"); // 11d old -> trimmed
    let fresh_ts = fixed_local_time("2026-11-24T12:00:00+00:00"); // 2d old -> untouched

    let expired_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "s",
            )
            .with_timestamp(expired_ts)
            .with_text("exp".into(), "exp2".into()),
        )
        .unwrap();
    let mid_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "s",
            )
            .with_timestamp(mid_ts)
            .with_text("mid".into(), "mid2".into()),
        )
        .unwrap();
    let fresh_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "s",
            )
            .with_timestamp(fresh_ts)
            .with_text("fresh".into(), "fresh2".into()),
        )
        .unwrap();

    let report = rec
        .apply_retention_at(
            now,
            RetentionPolicy {
                row_retention_days: Some(30),
                text_retention_days: Some(7),
            },
            true,
        )
        .unwrap();
    assert_eq!(report.stats_rows_deleted, 1);
    assert_eq!(report.text_rows_trimmed, 1);
    assert!(rec.record_by_id(expired_id).unwrap().is_none());

    let mid = rec.record_by_id(mid_id).unwrap().unwrap();
    assert!(mid.before_text.is_none());
    assert_eq!(mid.before_tokens, 400);

    let fresh = rec.record_by_id(fresh_id).unwrap().unwrap();
    assert_eq!(fresh.before_text.as_deref(), Some("fresh"));
}

#[test]
fn retention_queries_use_instant_and_partial_text_indexes() {
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    rec.record(
        &sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "s",
        )
        .with_text("before".into(), "after".into()),
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let explain = |sql: &str| -> String {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" | ")
    };

    let prune_plan = explain("EXPLAIN QUERY PLAN DELETE FROM stats WHERE timestamp_ns < 100");
    assert!(
        prune_plan.contains("idx_stats_instant"),
        "expected idx_stats_instant in plan: {prune_plan}"
    );
    assert!(!prune_plan.contains("TEMP B-TREE"), "plan: {prune_plan}");

    let trim_plan = explain(
        "EXPLAIN QUERY PLAN
         UPDATE stats
         SET before_text = NULL,
             after_text = NULL,
             before_output = NULL,
             after_output = NULL
         WHERE timestamp_ns < 100
           AND (before_text IS NOT NULL
             OR after_text IS NOT NULL
             OR before_output IS NOT NULL
             OR after_output IS NOT NULL)",
    );
    assert!(
        trim_plan.contains("idx_stats_text_instant"),
        "expected idx_stats_text_instant in plan: {trim_plan}"
    );
    assert!(!trim_plan.contains("TEMP B-TREE"), "plan: {trim_plan}");

    let retrieve_plan =
        explain("EXPLAIN QUERY PLAN DELETE FROM retrieve_events WHERE timestamp_ns < 100");
    assert!(
        retrieve_plan.contains("idx_retrieve_events_instant"),
        "expected idx_retrieve_events_instant in plan: {retrieve_plan}"
    );

    let backfill_stats_plan = explain(
        "EXPLAIN QUERY PLAN
         SELECT id, timestamp FROM stats
         WHERE timestamp_ns IS NULL
         ORDER BY id ASC
         LIMIT 1000",
    );
    assert!(
        backfill_stats_plan.contains("idx_stats_instant"),
        "expected idx_stats_instant in plan: {backfill_stats_plan}"
    );
    assert!(
        !backfill_stats_plan.contains("TEMP B-TREE"),
        "plan: {backfill_stats_plan}"
    );

    let backfill_retrieve_plan = explain(
        "EXPLAIN QUERY PLAN
         SELECT id, timestamp FROM retrieve_events
         WHERE timestamp_ns IS NULL
         ORDER BY id ASC
         LIMIT 1000",
    );
    assert!(
        backfill_retrieve_plan.contains("idx_retrieve_events_instant"),
        "expected idx_retrieve_events_instant in plan: {backfill_retrieve_plan}"
    );
    assert!(
        !backfill_retrieve_plan.contains("TEMP B-TREE"),
        "plan: {backfill_retrieve_plan}"
    );
}

#[test]
fn prune_handles_rtk_null_timestamp_ns_utc_offsets_corrupt_rows_and_max_days() {
    let (rec, dir) = new_recorder();
    let db_path = dir.path().join("stats.db");
    let now = fixed_local_time("2026-11-26T06:00:00+00:00");

    // Simulate RTK-written rows without timestamp_ns across UTC offsets:
    // 2026-11-01T01:30:00-04:00 == 05:30Z (> 25 days old from 2026-11-26T06:00Z -> expired for 25d)
    // 2026-11-01T01:15:00-05:00 == 06:15Z (< 25 days old from 2026-11-26T06:00Z -> kept for 25d)
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO stats (timestamp, timestamp_ns, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens, before_text, after_text)
         VALUES ('2026-11-01T01:30:00.000000-04:00', NULL, 'rewrite-command', 'rtk', 100, 25, 50, 12, 'b1', 'a1')",
        [],
    )
    .unwrap();
    let older_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO stats (timestamp, timestamp_ns, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens, before_text, after_text)
         VALUES ('2026-11-01T01:15:00.000000-05:00', NULL, 'rewrite-command', 'rtk', 100, 25, 50, 12, 'b2', 'a2')",
        [],
    )
    .unwrap();
    let newer_id = conn.last_insert_rowid();
    // Corrupt timestamp must be preserved (UNPARSEABLE_INSTANT_NS == i64::MAX).
    conn.execute(
        "INSERT INTO stats (timestamp, timestamp_ns, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens)
         VALUES ('not-a-date', NULL, 'compress-response', 'cli', 100, 25, 50, 12)",
        [],
    )
    .unwrap();
    let corrupt_id = conn.last_insert_rowid();
    drop(conn);

    // u32::MAX days must not overflow or delete anything.
    let no_overflow = rec
        .apply_retention_at(
            now,
            RetentionPolicy {
                row_retention_days: Some(u32::MAX),
                text_retention_days: Some(u32::MAX),
            },
            true,
        )
        .unwrap();
    assert!(no_overflow.is_empty());

    let report = rec
        .apply_retention_at(
            now,
            RetentionPolicy {
                row_retention_days: Some(25),
                text_retention_days: None,
            },
            false,
        )
        .unwrap();
    assert_eq!(report.stats_rows_deleted, 1);
    assert!(rec.record_by_id(older_id).unwrap().is_none());
    assert!(rec.record_by_id(newer_id).unwrap().is_some());
    assert!(rec.record_by_id(corrupt_id).unwrap().is_some());

    // Zero days disables retention as a no-op.
    assert!(rec.prune_older_than(0).unwrap().is_empty());
    assert!(rec.trim_text_older_than(0).unwrap().is_empty());
}

#[test]
fn periodic_sampled_retention_runs_on_interval_and_is_fail_soft() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let rec = StatsRecorder::with_retention(
        &db_path,
        RetentionPolicy {
            row_retention_days: Some(30),
            text_retention_days: Some(7),
        },
    )
    .unwrap();

    let expired_ts = Local::now() - chrono::Duration::days(45);
    let mid_ts = Local::now() - chrono::Duration::days(10);
    let expired_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "auto",
            )
            .with_timestamp(expired_ts)
            .with_text("old".into(), "old2".into()),
        )
        .unwrap();
    let mid_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "auto",
            )
            .with_timestamp(mid_ts)
            .with_text("mid".into(), "mid2".into()),
        )
        .unwrap();

    // Rows 3..63 must not trigger sampled cleanup yet.
    for _ in 3..StatsRecorder::AUTO_RETENTION_INTERVAL {
        rec.record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "auto",
        ))
        .unwrap();
    }
    assert!(rec.record_by_id(expired_id).unwrap().is_some());
    assert!(
        rec.record_by_id(mid_id)
            .unwrap()
            .unwrap()
            .before_text
            .is_some()
    );

    // Row 64 triggers sampled cleanup.
    let trigger_id = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "auto",
        ))
        .unwrap();
    assert_eq!(trigger_id, StatsRecorder::AUTO_RETENTION_INTERVAL);
    assert!(rec.record_by_id(expired_id).unwrap().is_none());
    let trimmed_mid = rec.record_by_id(mid_id).unwrap().unwrap();
    assert!(trimmed_mid.before_text.is_none());

    // Fail-soft: even if retention cleanup fails (e.g. blocked DELETE trigger),
    // record() on the next interval still commits and returns Ok(row_id).
    let another_expired_id = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "auto",
            )
            .with_timestamp(expired_ts),
        )
        .unwrap();
    {
        let conn = rec.lock_conn();
        conn.execute_batch(
            "CREATE TRIGGER block_retention_delete BEFORE DELETE ON stats
             BEGIN SELECT RAISE(ABORT, 'blocked delete'); END;",
        )
        .unwrap();
    }
    for _ in (another_expired_id + 1)..(StatsRecorder::AUTO_RETENTION_INTERVAL * 2) {
        rec.record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "auto",
        ))
        .unwrap();
    }
    let second_trigger_id = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "auto",
        ))
        .unwrap();
    assert_eq!(
        second_trigger_id,
        StatsRecorder::AUTO_RETENTION_INTERVAL * 2
    );
    assert!(rec.record_by_id(another_expired_id).unwrap().is_some());
}

#[test]
fn compact_and_clear_reclaim_disk_space_preserve_permissions_and_handle_busy_db() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let rec = StatsRecorder::new(&db_path).unwrap();

    let big_before = "A".repeat(256 * 1024);
    let big_after = "B".repeat(128 * 1024);
    let old_ts = Local::now() - chrono::Duration::days(30);
    for _ in 0..8 {
        rec.record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "big",
            )
            .with_timestamp(old_ts)
            .with_text(big_before.clone(), big_after.clone()),
        )
        .unwrap();
    }
    // Checkpoint WAL first so db_path reflects the full allocated footprint.
    let pre_ckpt = rec.checkpoint_wal().unwrap();
    assert!(pre_ckpt.wal_checkpoint_completed);
    let size_with_payloads = db_bundle_size(&db_path);
    assert!(size_with_payloads > 1_000_000, "size: {size_with_payloads}");

    let trimmed = rec.trim_text_older_than(7).unwrap();
    assert_eq!(trimmed.text_rows_trimmed, 8);
    let compact = rec.compact().unwrap();
    assert!(compact.vacuum_attempted);
    assert!(compact.vacuum_completed);
    assert!(compact.wal_checkpoint_completed);
    assert!(!compact.wal_busy);
    assert!(
        compact.bytes_after < size_with_payloads / 4,
        "expected compaction to reclaim freed pages: before={size_with_payloads}, after={}",
        compact.bytes_after
    );
    assert_eq!(
        sidecar_path(&db_path, "-wal")
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0),
        0
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&db_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // Active read snapshot makes TRUNCATE report wal_busy=true without blocking.
    let reader = rusqlite::Connection::open(&db_path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT COUNT(*) FROM stats;")
        .unwrap();
    rec.record(
        &sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "big",
        )
        .with_text(big_before, big_after),
    )
    .unwrap();
    let wal_report = rec.checkpoint_wal().unwrap();
    assert!(!wal_report.wal_checkpoint_completed);
    assert!(wal_report.wal_busy);
    drop(reader);

    // Active write lock makes VACUUM defer fail-soft (vacuum_completed == false, Ok).
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let busy_compact = rec.compact().unwrap();
    assert!(busy_compact.vacuum_attempted);
    assert!(!busy_compact.vacuum_completed);
    assert!(!busy_compact.wal_checkpoint_completed);
    assert!(busy_compact.wal_busy);
    drop(writer);

    // clear() deletes everything and reclaims space.
    rec.clear().unwrap();
    assert_eq!(rec.count().unwrap(), 0);
    assert!(db_bundle_size(&db_path) < size_with_payloads / 4);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&db_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn mixed_external_and_recorder_writes_do_not_skip_retention_window() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let rec = StatsRecorder::with_retention(&db_path, RetentionPolicy::default_enabled()).unwrap();
    let expired_ts = Local::now() - chrono::Duration::days(45);
    let recent_ts = Local::now().to_rfc3339();

    let first_expired = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "mix",
            )
            .with_timestamp(expired_ts)
            .with_text("old-1".into(), "old-1-a".into()),
        )
        .unwrap();

    // External writer (e.g. RTK) occupies rows 2..=64 including row 64.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for index in 2..=StatsRecorder::AUTO_RETENTION_INTERVAL {
            conn.execute(
                "INSERT INTO stats (timestamp, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens)
                 VALUES (?1, 'rewrite-command', 'rtk', 100, 25, 40, 10)",
                [&recent_ts],
            )
            .unwrap();
            let _ = index;
        }
    }
    assert!(rec.record_by_id(first_expired).unwrap().is_some());

    // Next recorder insert lands at row 65 (not % 64) and must still trigger cleanup.
    let trigger_one = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "mix",
        ))
        .unwrap();
    assert_eq!(trigger_one, StatsRecorder::AUTO_RETENTION_INTERVAL + 1);
    assert!(rec.record_by_id(first_expired).unwrap().is_none());

    // Repeat the second window: expired row at 66, external writer occupies row 128,
    // recorder write at 129 triggers cleanup immediately (129 - 65 == 64).
    let second_expired = rec
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "mix",
            )
            .with_timestamp(expired_ts),
        )
        .unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for _ in (second_expired + 1)..=(StatsRecorder::AUTO_RETENTION_INTERVAL * 2) {
            conn.execute(
                "INSERT INTO stats (timestamp, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens)
                 VALUES (?1, 'rewrite-command', 'rtk', 100, 25, 40, 10)",
                [&recent_ts],
            )
            .unwrap();
        }
    }
    assert!(rec.record_by_id(second_expired).unwrap().is_some());
    let trigger_two = rec
        .record(&sample(
            OperationType::CompressResponse,
            CompressionMode::Active,
            "mix",
        ))
        .unwrap();
    assert_eq!(trigger_two, StatsRecorder::AUTO_RETENTION_INTERVAL * 2 + 1);
    assert!(rec.record_by_id(second_expired).unwrap().is_none());
}

#[test]
fn maybe_run_periodic_retention_cleans_rtk_only_database_without_recorder_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let old_ts = (Local::now() - chrono::Duration::days(45)).to_rfc3339();
    let mid_ts = (Local::now() - chrono::Duration::days(15)).to_rfc3339();
    let recent_ts = Local::now().to_rfc3339();
    let my_pid = 42_001_i64;
    let other_pid = 42_002_i64;

    // Nonexistent file is a fast default snapshot / Ok(None).
    assert_eq!(
        StatsRecorder::read_retention_snapshot(&db_path).unwrap(),
        RetentionSnapshot::default()
    );
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(
            &db_path,
            RetentionPolicy::default_enabled(),
            RetentionSnapshot::default(),
            my_pid,
        )
        .unwrap(),
        None
    );

    // Seed RTK-only schema (no timestamp_ns column, no stats_maintenance table).
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        StatsRecorder::read_retention_snapshot(&db_path).unwrap(),
        RetentionSnapshot::default()
    );
    conn.execute_batch(
        "CREATE TABLE stats (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            operation TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            source_pid INTEGER,
            session_id TEXT,
            tool_use_id TEXT,
            before_chars INTEGER NOT NULL,
            before_tokens INTEGER NOT NULL,
            after_chars INTEGER NOT NULL,
            after_tokens INTEGER NOT NULL,
            before_text TEXT,
            after_text TEXT
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens, before_text, after_text)
         VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20, 'rtk-old-b', 'rtk-old-a')",
        rusqlite::params![old_ts, other_pid],
    )
    .unwrap();
    let expired_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens, before_text, after_text)
         VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20, 'rtk-mid-b', 'rtk-mid-a')",
        rusqlite::params![mid_ts, other_pid],
    )
    .unwrap();
    let mid_id = conn.last_insert_rowid();
    for _ in 3..StatsRecorder::AUTO_RETENTION_INTERVAL {
        conn.execute(
            "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens)
             VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20)",
            rusqlite::params![recent_ts, other_pid],
        )
        .unwrap();
    }
    assert_eq!(
        StatsRecorder::read_retention_snapshot(&db_path).unwrap(),
        RetentionSnapshot {
            max_row_id: StatsRecorder::AUTO_RETENTION_INTERVAL - 1,
            clear_epoch: 0,
        }
    );

    // Before 64 rows, read-only probe returns Ok(None) without migrating schema.
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(
            &db_path,
            RetentionPolicy::default_enabled(),
            RetentionSnapshot {
                max_row_id: StatsRecorder::AUTO_RETENTION_INTERVAL - 1,
                clear_epoch: 0,
            },
            my_pid,
        )
        .unwrap(),
        None
    );

    // Row 64 written by another PID must not trigger cleanup when `my_pid` did not write
    // (simulating a read-only `tokenless rtk gain` after a pre-existing 64-row DB).
    conn.execute(
        "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens)
         VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20)",
        rusqlite::params![recent_ts, other_pid],
    )
    .unwrap();
    assert_eq!(
        StatsRecorder::read_retention_snapshot(&db_path).unwrap(),
        RetentionSnapshot {
            max_row_id: StatsRecorder::AUTO_RETENTION_INTERVAL,
            clear_epoch: 0,
        }
    );
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(
            &db_path,
            RetentionPolicy::default_enabled(),
            RetentionSnapshot {
                max_row_id: StatsRecorder::AUTO_RETENTION_INTERVAL - 1,
                clear_epoch: 0,
            },
            my_pid,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(
            &db_path,
            RetentionPolicy::default_enabled(),
            RetentionSnapshot {
                max_row_id: StatsRecorder::AUTO_RETENTION_INTERVAL,
                clear_epoch: 0,
            },
            my_pid,
        )
        .unwrap(),
        None
    );
    let has_ts_ns: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = 'timestamp_ns'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0;
    assert!(!has_ts_ns);
    let intact_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stats WHERE id IN (?1, ?2) AND before_text IS NOT NULL",
            rusqlite::params![expired_id, mid_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(intact_rows, 2);

    // Row 65 written by `my_pid` crosses the threshold, deletes row 1, and trims row 2.
    conn.execute(
        "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens)
         VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20)",
        rusqlite::params![recent_ts, my_pid],
    )
    .unwrap();
    let trigger_id = StatsRecorder::AUTO_RETENTION_INTERVAL + 1;
    let report = StatsRecorder::maybe_run_periodic_retention(
        &db_path,
        RetentionPolicy::default_enabled(),
        RetentionSnapshot {
            max_row_id: StatsRecorder::AUTO_RETENTION_INTERVAL,
            clear_epoch: 0,
        },
        my_pid,
    )
    .unwrap()
    .expect("own write past 64 rows must trigger cleanup");
    assert_eq!(report.stats_rows_deleted, 1);
    assert_eq!(report.text_rows_trimmed, 1);

    let remaining_expired: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stats WHERE id = ?1",
            [expired_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_expired, 0);
    let (mid_before, mid_after): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT before_text, after_text FROM stats WHERE id = ?1",
            [mid_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(mid_before.is_none());
    assert!(mid_after.is_none());
    let watermark: i64 = conn
        .query_row(
            "SELECT last_retention_row_id FROM stats_maintenance WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(watermark, trigger_id);

    // Next 63 own rows (66..=128) stay below 64-row delta and skip cleanly.
    for _ in 0..(StatsRecorder::AUTO_RETENTION_INTERVAL - 1) {
        conn.execute(
            "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens)
             VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20)",
            rusqlite::params![recent_ts, my_pid],
        )
        .unwrap();
    }
    let below_next_window = trigger_id + StatsRecorder::AUTO_RETENTION_INTERVAL - 1;
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(
            &db_path,
            RetentionPolicy::default_enabled(),
            RetentionSnapshot {
                max_row_id: below_next_window - 1,
                clear_epoch: 0,
            },
            my_pid,
        )
        .unwrap(),
        None
    );

    // Row 129 (delta == 64) triggers again and advances the watermark.
    conn.execute(
        "INSERT INTO stats (timestamp, operation, agent_id, source_pid, before_chars, before_tokens, after_chars, after_tokens)
         VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20)",
        rusqlite::params![recent_ts, my_pid],
    )
    .unwrap();
    let second_trigger_id = trigger_id + StatsRecorder::AUTO_RETENTION_INTERVAL;
    let second = StatsRecorder::maybe_run_periodic_retention(
        &db_path,
        RetentionPolicy::default_enabled(),
        RetentionSnapshot {
            max_row_id: below_next_window,
            clear_epoch: 0,
        },
        my_pid,
    )
    .unwrap()
    .expect("own write at next 64-row delta must advance watermark");
    assert_eq!(second.total_changes(), 0);
    let watermark: i64 = conn
        .query_row(
            "SELECT last_retention_row_id FROM stats_maintenance WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(watermark, second_trigger_id);
}

#[test]
fn sampled_retention_bounds_timestamp_backfill_to_batch_limit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let policy = RetentionPolicy::default_enabled();
    let old_ts = (Local::now() - chrono::Duration::days(45)).to_rfc3339();

    drop(StatsRecorder::with_retention(&db_path, policy).unwrap());

    // Insert 2,500 expired RTK-format rows with NULL timestamp_ns.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut stmt = conn
        .prepare(
            "INSERT INTO stats (timestamp, timestamp_ns, operation, agent_id, before_chars, before_tokens, after_chars, after_tokens, before_text, after_text)
             VALUES (?1, NULL, 'rewrite-command', 'rtk', 100, 25, 40, 10, 'old-b', 'old-a')",
        )
        .unwrap();
    for _ in 0..2_500 {
        stmt.execute([&old_ts]).unwrap();
    }
    drop(stmt);

    // Opening with_retention must not run an unbounded open-time backfill.
    let rec = StatsRecorder::with_retention(&db_path, policy).unwrap();
    let unkeyed_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stats WHERE timestamp_ns IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unkeyed_before, 2_500);

    // One sampled retention pass (row 2,501) backfills and deletes only AUTO_RETENTION_BATCH_LIMIT (1,000) rows.
    rec.record(&sample(
        OperationType::CompressResponse,
        CompressionMode::Active,
        "batch",
    ))
    .unwrap();

    let total_after: usize = rec.count().unwrap();
    assert_eq!(
        total_after,
        2_500 - StatsRecorder::AUTO_RETENTION_BATCH_LIMIT + 1
    );
    let unkeyed_after: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM stats WHERE timestamp_ns IS NULL",
            [],
            |row| Ok(row.get::<_, i64>(0)? as usize),
        )
        .unwrap();
    assert_eq!(
        unkeyed_after,
        2_500 - StatsRecorder::AUTO_RETENTION_BATCH_LIMIT
    );
}

#[test]
fn out_of_order_rtk_maintenance_does_not_regress_watermark_or_retrigger_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let policy = RetentionPolicy::default_enabled();
    let old_ts = (Local::now() - chrono::Duration::days(45)).to_rfc3339();
    let recent_ts = Local::now().to_rfc3339();
    let seed_pid = 40_000_i64;
    let pid_a = 40_001_i64;
    let pid_b = 40_002_i64;

    // 1. Seed 2,500 expired RTK-format rows inside one transaction.
    {
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                operation TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                source_pid INTEGER,
                session_id TEXT,
                tool_use_id TEXT,
                before_chars INTEGER NOT NULL,
                before_tokens INTEGER NOT NULL,
                after_chars INTEGER NOT NULL,
                after_tokens INTEGER NOT NULL,
                before_text TEXT,
                after_text TEXT
            );",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO stats (
                        timestamp, operation, agent_id, source_pid,
                        before_chars, before_tokens, after_chars, after_tokens,
                        before_text, after_text
                    ) VALUES (?1, 'rewrite-command', 'expired-seed', ?2, 200, 50, 80, 20, 'old-b', 'old-a')",
                )
                .unwrap();
            for _ in 0..2_500 {
                stmt.execute(rusqlite::params![old_ts, seed_pid]).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    // 2. Process A appends row 2,501, 63 filler rows follow (2,502..=2,564),
    //    then Process B appends row 2,565.
    let before_a = StatsRecorder::read_retention_snapshot(&db_path).unwrap();
    assert_eq!(
        before_a,
        RetentionSnapshot {
            max_row_id: 2_500,
            clear_epoch: 0,
        }
    );
    {
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO stats (
                timestamp, operation, agent_id, source_pid,
                before_chars, before_tokens, after_chars, after_tokens
            ) VALUES (?1, 'rewrite-command', 'rtk-recent', ?2, 200, 50, 80, 20)",
            rusqlite::params![recent_ts, pid_a],
        )
        .unwrap();
        let row_a = conn.last_insert_rowid();
        assert_eq!(row_a, 2_501);

        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO stats (
                        timestamp, operation, agent_id, source_pid,
                        before_chars, before_tokens, after_chars, after_tokens
                    ) VALUES (?1, 'rewrite-command', 'rtk-recent', ?2, 200, 50, 80, 20)",
                )
                .unwrap();
            for _ in 0..63 {
                stmt.execute(rusqlite::params![recent_ts, seed_pid])
                    .unwrap();
            }
        }
        tx.commit().unwrap();

        let before_b = StatsRecorder::read_retention_snapshot(&db_path).unwrap();
        assert_eq!(
            before_b,
            RetentionSnapshot {
                max_row_id: 2_564,
                clear_epoch: 0,
            }
        );

        conn.execute(
            "INSERT INTO stats (
                timestamp, operation, agent_id, source_pid,
                before_chars, before_tokens, after_chars, after_tokens
            ) VALUES (?1, 'rewrite-command', 'rtk-recent', ?2, 200, 50, 80, 20)",
            rusqlite::params![recent_ts, pid_b],
        )
        .unwrap();
        let row_b = conn.last_insert_rowid();
        assert_eq!(row_b, 2_565);

        // 3. Process B completes maintenance first: deletes 1,000 expired rows and sets watermark=2,565.
        let report_b =
            StatsRecorder::maybe_run_periodic_retention(&db_path, policy, before_b, pid_b)
                .unwrap()
                .expect("process B should run initial cleanup batch");
        assert_eq!(
            report_b.stats_rows_deleted,
            StatsRecorder::AUTO_RETENTION_BATCH_LIMIT
        );

        let remaining_expired = |conn: &rusqlite::Connection| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM stats WHERE agent_id = 'expired-seed'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let current_watermark = |conn: &rusqlite::Connection| -> i64 {
            conn.query_row(
                "SELECT last_retention_row_id FROM stats_maintenance WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        assert_eq!(remaining_expired(&conn), 1_500);
        assert_eq!(current_watermark(&conn), 2_565);

        // 4. Process A finishes maintenance late: must skip without regressing watermark or deleting more rows.
        assert_eq!(
            StatsRecorder::maybe_run_periodic_retention(&db_path, policy, before_a, pid_a).unwrap(),
            None
        );
        assert_eq!(remaining_expired(&conn), 1_500);
        assert_eq!(current_watermark(&conn), 2_565);

        // 5. Direct in-transaction re-check with stale row_id 2,501 must also return Ok(None) and keep state intact.
        let direct = StatsRecorder::with_retention(&db_path, policy).unwrap();
        {
            let mut direct_conn = direct.lock_conn();
            assert_eq!(
                direct
                    .run_sampled_retention_inner(&mut direct_conn, row_a, before_a.clear_epoch)
                    .unwrap(),
                None
            );
        }
        assert_eq!(remaining_expired(&conn), 1_500);
        assert_eq!(current_watermark(&conn), 2_565);

        // 6. Very next foreground write (row 2,566) must NOT trigger an immediate cleanup pass.
        let row_next = direct
            .record(&sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "after-race",
            ))
            .unwrap();
        assert_eq!(row_next, 2_566);
        assert_eq!(remaining_expired(&conn), 1_500);
        assert_eq!(current_watermark(&conn), 2_565);

        // 7. clear() resets the watermark and advances clear_epoch atomically so subsequent writes trigger cleanly again at row 64.
        direct.clear().unwrap();
        assert_eq!(direct.count().unwrap(), 0);
        let maintenance: (i64, i64) = conn
            .query_row(
                "SELECT last_retention_row_id, clear_epoch FROM stats_maintenance WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(maintenance, (0, 1));

        let cleared_expired = direct
            .record(
                &sample(
                    OperationType::CompressResponse,
                    CompressionMode::Active,
                    "post-clear",
                )
                .with_timestamp(Local::now() - chrono::Duration::days(45)),
            )
            .unwrap();
        assert_eq!(cleared_expired, 1);
        for _ in 2..=StatsRecorder::AUTO_RETENTION_INTERVAL {
            direct
                .record(&sample(
                    OperationType::CompressResponse,
                    CompressionMode::Active,
                    "post-clear",
                ))
                .unwrap();
        }
        assert!(direct.record_by_id(cleared_expired).unwrap().is_none());
        assert_eq!(direct.count().unwrap(), 63);
        assert_eq!(
            current_watermark(&conn),
            StatsRecorder::AUTO_RETENTION_INTERVAL
        );
    }
}

#[test]
fn late_writer_after_clear_does_not_restore_pre_clear_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stats.db");
    let policy = RetentionPolicy::default_enabled();
    let old_ts = (Local::now() - chrono::Duration::days(45)).to_rfc3339();
    let recent_ts = Local::now().to_rfc3339();
    let pid_late = 41_001_i64;

    // 1. Legacy stats_maintenance without `clear_epoch` column must remain
    //    readable read-only and migrate cleanly while preserving the watermark.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE stats_maintenance (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                last_retention_row_id INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO stats_maintenance (singleton, last_retention_row_id) VALUES (1, 128);",
        )
        .unwrap();
        assert_eq!(
            StatsRecorder::read_retention_snapshot(&db_path).unwrap(),
            RetentionSnapshot {
                max_row_id: 0,
                clear_epoch: 0,
            }
        );
    }

    let recorder = StatsRecorder::with_retention(&db_path, policy).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let read_state = |conn: &rusqlite::Connection| -> (i64, i64) {
        conn.query_row(
            "SELECT last_retention_row_id, clear_epoch FROM stats_maintenance WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(read_state(&conn), (128, 0));

    // 2. Seed pre-clear rows up to id=3,000, snapshot before RTK runs, then
    //    append pre-clear row id=3,001 by `pid_late`.
    {
        let mut seed_conn = rusqlite::Connection::open(&db_path).unwrap();
        let tx = seed_conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO stats (
                        timestamp, operation, agent_id, source_pid,
                        before_chars, before_tokens, after_chars, after_tokens
                    ) VALUES (?1, 'rewrite-command', 'seed', 41000, 200, 50, 80, 20)",
                )
                .unwrap();
            for _ in 0..3_000 {
                stmt.execute([&recent_ts]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let before_late = StatsRecorder::read_retention_snapshot(&db_path).unwrap();
    assert_eq!(
        before_late,
        RetentionSnapshot {
            max_row_id: 3_000,
            clear_epoch: 0,
        }
    );
    conn.execute(
        "INSERT INTO stats (
            timestamp, operation, agent_id, source_pid,
            before_chars, before_tokens, after_chars, after_tokens
        ) VALUES (?1, 'rewrite-command', 'rtk', ?2, 200, 50, 80, 20)",
        rusqlite::params![recent_ts, pid_late],
    )
    .unwrap();
    let pre_clear_row_id = conn.last_insert_rowid();
    assert_eq!(pre_clear_row_id, 3_001);

    // 3. `clear()` empties stats, resets sqlite_sequence, zeroes the watermark,
    //    and advances clear_epoch to 1.
    recorder.clear().unwrap();
    assert_eq!(read_state(&conn), (0, 1));
    assert_eq!(
        StatsRecorder::read_retention_snapshot(&db_path).unwrap(),
        RetentionSnapshot {
            max_row_id: 0,
            clear_epoch: 1,
        }
    );

    // 4. Window A: late RTK probe with pre-clear snapshot (3000, epoch=0) skips cleanly.
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(&db_path, policy, before_late, pid_late)
            .unwrap(),
        None
    );
    assert_eq!(read_state(&conn), (0, 1));

    // 5. Window B: late writer that already passed the probe with (row_id=3001, epoch=0)
    //    enters the write transaction after `clear()` (even with an expired post-clear row
    //    present) -> must return Ok(None), preserve the post-clear row, and NOT restore 3001!
    let post_clear_expired = recorder
        .record(
            &sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "post-clear-expired",
            )
            .with_timestamp(Local::now() - chrono::Duration::days(45)),
        )
        .unwrap();
    assert_eq!(post_clear_expired, 1);
    {
        let mut rec_conn = recorder.lock_conn();
        assert_eq!(
            recorder
                .run_sampled_retention_inner(
                    &mut rec_conn,
                    pre_clear_row_id,
                    before_late.clear_epoch
                )
                .unwrap(),
            None
        );
    }
    assert_eq!(read_state(&conn), (0, 1));
    assert!(recorder.record_by_id(post_clear_expired).unwrap().is_some());

    // 6. Normal post-clear writes in epoch 1 still trigger automatic retention on row 64.
    for _ in 2..=StatsRecorder::AUTO_RETENTION_INTERVAL {
        recorder
            .record(&sample(
                OperationType::CompressResponse,
                CompressionMode::Active,
                "post-clear-recent",
            ))
            .unwrap();
    }
    assert!(recorder.record_by_id(post_clear_expired).unwrap().is_none());
    assert_eq!(
        read_state(&conn),
        (StatsRecorder::AUTO_RETENTION_INTERVAL, 1)
    );

    // 7. Even if post-clear row IDs surpass 3,001 AND the old PID appears again,
    //    the stale epoch=0 snapshot and stale epoch=0 in-transaction call still skip!
    {
        let mut bulk_conn = rusqlite::Connection::open(&db_path).unwrap();
        let tx = bulk_conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO stats (
                        timestamp, operation, agent_id, source_pid,
                        before_chars, before_tokens, after_chars, after_tokens
                    ) VALUES (?1, 'rewrite-command', 'epoch1', ?2, 200, 50, 80, 20)",
                )
                .unwrap();
            for _ in 0..3_000 {
                stmt.execute(rusqlite::params![old_ts, pid_late]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    assert_eq!(
        StatsRecorder::maybe_run_periodic_retention(&db_path, policy, before_late, pid_late)
            .unwrap(),
        None
    );
    {
        let mut rec_conn = recorder.lock_conn();
        assert_eq!(
            recorder
                .run_sampled_retention_inner(
                    &mut rec_conn,
                    pre_clear_row_id,
                    before_late.clear_epoch
                )
                .unwrap(),
            None
        );
    }
    assert_eq!(
        read_state(&conn),
        (StatsRecorder::AUTO_RETENTION_INTERVAL, 1)
    );

    // 8. Defense-in-depth: even if an external tool resets `sqlite_sequence`
    //    without bumping `clear_epoch`, a stale row_id above `sqlite_sequence.seq`
    //    cannot write a phantom watermark into `stats_maintenance`.
    conn.execute_batch(
        "DELETE FROM stats;
         DELETE FROM sqlite_sequence WHERE name = 'stats';
         UPDATE stats_maintenance SET last_retention_row_id = 0 WHERE singleton = 1;",
    )
    .unwrap();
    {
        let mut rec_conn = recorder.lock_conn();
        assert_eq!(
            recorder
                .run_sampled_retention_inner(&mut rec_conn, pre_clear_row_id, 1)
                .unwrap(),
            None
        );
    }
    assert_eq!(read_state(&conn), (0, 1));

    // 9. A second `clear()` advances `clear_epoch` monotonically to 2.
    recorder.clear().unwrap();
    assert_eq!(read_state(&conn), (0, 2));
}
