use tokenless_stats::{
    CompressionMode, OperationType, StatsRecord, StatsRecorder, format_summary_window_json,
};

#[test]
fn incomplete_window_never_queries_unavailable_retrieval_table() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stats.db");
    let recorder = StatsRecorder::new(&path).unwrap();
    for mode in [CompressionMode::Active, CompressionMode::DryRun] {
        recorder
            .record(
                &StatsRecord::new(
                    OperationType::CompressResponse,
                    "test".into(),
                    100,
                    20,
                    50,
                    10,
                )
                .with_mode(mode),
            )
            .unwrap();
    }
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE retrieve_events")
        .unwrap();
    let partial = recorder.summary_window(Some(1)).unwrap();
    assert!(!partial.is_complete());
    assert!(partial.retrieve_totals.unwrap().is_none());
    let complete = recorder.summary_window(None).unwrap();
    assert!(complete.is_complete());
    // Strict callers can surface the error; best-effort formatting preserves
    // compression statistics without fabricating retrieval or net totals.
    assert!(complete.retrieve_totals.is_err());
    let output: serde_json::Value =
        serde_json::from_str(&format_summary_window_json(&complete)).unwrap();
    assert!(output["attribution"].get("net_savings_tokens").is_none());
    assert_eq!(output["total"]["tokens_saved"], 10);
}

#[test]
fn skipped_corrupt_record_makes_window_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stats.db");
    let recorder = StatsRecorder::new(&path).unwrap();
    recorder
        .record(&StatsRecord::new(
            OperationType::CompressResponse,
            "test".into(),
            100,
            20,
            50,
            10,
        ))
        .unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE stats SET before_tokens='corrupt'", [])
        .unwrap();
    let window = recorder.summary_window(None).unwrap();
    assert_eq!(window.total_records, 1);
    assert!(window.records.is_empty());
    assert!(!window.is_complete());
    assert!(window.retrieve_totals.unwrap().is_none());
}
