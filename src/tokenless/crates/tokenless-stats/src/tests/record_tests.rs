#[test]
fn test_operation_type_from_str() {
    assert_eq!(
        OperationType::from_str("compress-schema").unwrap(),
        OperationType::CompressSchema
    );
    assert_eq!(
        OperationType::from_str("compress-response").unwrap(),
        OperationType::CompressResponse
    );
    assert_eq!(
        OperationType::from_str("rewrite-command").unwrap(),
        OperationType::RewriteCommand
    );
    assert_eq!(
        OperationType::from_str("compress-toon").unwrap(),
        OperationType::CompressToon
    );
    assert!(OperationType::from_str("unknown").is_err());
}

#[test]
fn test_savings_calculation() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "copilot-shell".to_string(),
        1000,
        400,
        500,
        200,
    );

    assert_eq!(record.chars_saved(), 500);
    assert_eq!(record.tokens_saved(), 200);
    assert!((record.chars_percent() - 50.0).abs() < 0.1);
    assert!((record.tokens_percent() - 50.0).abs() < 0.1);
}

#[test]
fn test_record_with_text() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "copilot-shell".to_string(),
        16,
        4,
        10,
        3,
    )
    .with_text("original text here".to_string(), "compressed".to_string());

    assert!(record.before_text.is_some());
    assert!(record.after_text.is_some());
}

#[test]
fn test_format_summary_line() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "copilot-shell".to_string(),
        1000,
        400,
        500,
        200,
    )
    .with_session_id("session-123")
    .with_tool_use_id("call_abc");

    let line = record.format_summary_line();
    assert!(line.contains("[ID:-1]"));
    assert!(line.contains("copilot-shell"));
    assert!(line.contains("Session:session-123"));
    assert!(line.contains("Tool:call_abc"));
}

#[test]
fn test_format_summary_line_with_pid() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "copilot-shell".to_string(),
        1000,
        400,
        500,
        200,
    )
    .with_session_id("session-123")
    .with_source_pid(12345);

    let line = record.format_summary_line();
    assert!(line.contains("copilot-shell"));
    assert!(line.contains("pid:12345"));
}

#[test]
fn test_compression_mode_roundtrip() {
    assert_eq!(CompressionMode::Active.as_str(), "active");
    assert_eq!(CompressionMode::DryRun.as_str(), "dry-run");
    assert_eq!(CompressionMode::from_db("active"), CompressionMode::Active);
    assert_eq!(CompressionMode::from_db("dry-run"), CompressionMode::DryRun);
    // Legacy "dryrun" form still readable (backward compatibility)
    assert_eq!(CompressionMode::from_db("dryrun"), CompressionMode::DryRun);
    // Unknown / empty (legacy NULL) fall back to Active
    assert_eq!(CompressionMode::from_db(""), CompressionMode::Active);
    assert_eq!(CompressionMode::from_db("???"), CompressionMode::Active);
}

#[test]
fn test_record_default_mode_active() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "test".to_string(),
        100,
        25,
        80,
        20,
    );
    assert_eq!(record.mode, CompressionMode::Active);

    let record = record.with_mode(CompressionMode::DryRun);
    assert_eq!(record.mode, CompressionMode::DryRun);
}

#[test]
fn test_with_output_builder() {
    let record = StatsRecord::new(
        OperationType::RewriteCommand,
        "cli".to_string(),
        100,
        25,
        80,
        20,
    )
    .with_output(
        "original output".to_string(),
        "rewritten output".to_string(),
    );
    assert_eq!(record.before_output.as_deref(), Some("original output"));
    assert_eq!(record.after_output.as_deref(), Some("rewritten output"));
}

#[test]
fn test_with_stash_builder() {
    let record = StatsRecord::new(
        OperationType::CompressResponse,
        "cli".to_string(),
        100,
        25,
        80,
        20,
    )
    .with_stash(Some(3), Some(1), Some(42));
    assert_eq!(record.stash_writes, Some(3));
    assert_eq!(record.stash_errors, Some(1));
    assert_eq!(record.stash_size, Some(42));
}

#[test]
fn test_with_stash_none_values() {
    let record = StatsRecord::new(
        OperationType::CompressResponse,
        "cli".to_string(),
        100,
        25,
        80,
        20,
    )
    .with_stash(None, None, None);
    assert_eq!(record.stash_writes, None);
    assert_eq!(record.stash_errors, None);
    assert_eq!(record.stash_size, None);
}

#[test]
fn test_chars_percent_zero_before() {
    let record = StatsRecord::new(OperationType::CompressSchema, "cli".to_string(), 0, 0, 0, 0);
    assert_eq!(record.chars_percent(), 0.0);
    assert_eq!(record.tokens_percent(), 0.0);
}

#[test]
fn test_operation_type_as_str_roundtrip() {
    let ops = [
        OperationType::CompressSchema,
        OperationType::CompressResponse,
        OperationType::RewriteCommand,
        OperationType::CompressToon,
    ];
    for op in &ops {
        let s = op.as_str();
        let parsed = OperationType::from_str(s).unwrap();
        assert_eq!(&parsed, op);
    }
}

#[test]
fn test_with_tool_use_id() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "cli".to_string(),
        100,
        25,
        80,
        20,
    )
    .with_tool_use_id("tool_abc");
    assert_eq!(record.tool_use_id.as_deref(), Some("tool_abc"));
}

#[test]
fn test_with_before_and_after_text() {
    let record = StatsRecord::new(
        OperationType::CompressSchema,
        "cli".to_string(),
        100,
        25,
        80,
        20,
    )
    .with_before_text("before".to_string())
    .with_after_text("after".to_string());
    assert_eq!(record.before_text.as_deref(), Some("before"));
    assert_eq!(record.after_text.as_deref(), Some("after"));
}
