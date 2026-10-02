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
    let report = crate::diff::session_report(
        &records,
        "linked-session",
        20,
        crate::diff::DiffSort::Time,
    );
    let json = serde_json::to_value(report).unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 2);
    assert_eq!(json["chains"][0]["status"], "standalone");
    assert_eq!(json["chains"][0]["mode"], "dry-run");
    assert_eq!(json["chains"][1]["status"], "linked");
    assert_eq!(json["chains"][1]["stages"].as_array().unwrap().len(), 2);
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

    let record =
        StatsRecord::new(OperationType::CompressSchema, "cli".into(), 100, 25, 50, 12)
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
    let record =
        StatsRecord::new(OperationType::CompressSchema, "cli".into(), 100, 25, 50, 12)
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
    let record = StatsRecord::new(OperationType::CompressResponse, "cli".into(), 100, 40, 50, 20)
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
    rec.record_artifacts(id, "response-cleanup", &hashes).unwrap();

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
