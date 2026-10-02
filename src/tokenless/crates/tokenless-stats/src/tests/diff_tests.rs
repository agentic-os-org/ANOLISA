use chrono::{Duration, Local};

fn sample_record(
    id: i64,
    operation: OperationType,
    before: &str,
    after: &str,
    tokens: (usize, usize),
    tool_use_id: Option<&str>,
    mode: CompressionMode,
) -> StatsRecord {
    let mut record = StatsRecord::new(
        operation,
        "test-agent".to_string(),
        before.len(),
        tokens.0,
        after.len(),
        tokens.1,
    )
    .with_session_id("session-1")
    .with_text(before.to_string(), after.to_string())
    .with_mode(mode);
    record.id = id;
    record.timestamp = Local::now() + Duration::seconds(id);
    if let Some(tool_use_id) = tool_use_id {
        record = record.with_tool_use_id(tool_use_id);
    }
    record
}

fn diff_records(records: Vec<StatsRecord>) -> DiffRecords {
    DiffRecords::from_records(records)
}

#[test]
fn record_report_contains_structured_hunks() {
    let record = sample_record(
        1,
        OperationType::CompressResponse,
        "keep\nremove\n",
        "keep\nreplace\n",
        (20, 12),
        Some("tool-1"),
        CompressionMode::Active,
    );
    let report = record_report(&record, 3);
    let json = serde_json::to_value(&report).unwrap();

    assert_eq!(json["schema_version"], "1.0");
    assert_eq!(json["scope"]["kind"], "record");
    assert_eq!(json["chains"][0]["saved_tokens"], 8);
    let lines = json["chains"][0]["diff"]["hunks"][0]["lines"]
        .as_array()
        .unwrap();
    assert!(lines.iter().any(|line| line["kind"] == "delete"));
    assert!(lines.iter().any(|line| line["kind"] == "insert"));
    assert!(!serde_json::to_string(&report).unwrap().contains("\u{1b}["));
}

#[test]
fn record_report_respects_zero_context() {
    let record = sample_record(
        90,
        OperationType::CompressResponse,
        "unchanged before\nremoved\nunchanged after\n",
        "unchanged before\ninserted\nunchanged after\n",
        (20, 12),
        Some("tool-context"),
        CompressionMode::Active,
    );
    let json = serde_json::to_value(record_report(&record, 0)).unwrap();
    let lines = json["chains"][0]["diff"]["hunks"][0]["lines"]
        .as_array()
        .unwrap();

    assert!(lines.iter().all(|line| line["kind"] != "context"));
}

#[test]
fn record_report_truncates_rendered_hunks() {
    let before = (0..600)
        .flat_map(|index| [format!("before-{index}"), format!("keep-{index}")])
        .collect::<Vec<_>>()
        .join("\n");
    let after = (0..600)
        .flat_map(|index| [format!("after-{index}"), format!("keep-{index}")])
        .collect::<Vec<_>>()
        .join("\n");
    let record = sample_record(
        91,
        OperationType::CompressResponse,
        &before,
        &after,
        (1_000, 500),
        Some("tool-truncated"),
        CompressionMode::Active,
    );
    let json = serde_json::to_value(record_report(&record, 3)).unwrap();
    let line_count: usize = json["chains"][0]["diff"]["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hunk| hunk["lines"].as_array().unwrap().len())
        .sum();

    assert_eq!(json["chains"][0]["diff"]["truncated"], true);
    assert_eq!(line_count, MAX_DIFF_LINES);
    for hunk in json["chains"][0]["diff"]["hunks"].as_array().unwrap() {
        let lines = hunk["lines"].as_array().unwrap();
        let old_len = lines
            .iter()
            .filter(|line| !line["old_line"].is_null())
            .count() as u64;
        let new_len = lines
            .iter()
            .filter(|line| !line["new_line"].is_null())
            .count() as u64;
        assert_eq!(hunk["old_len"].as_u64(), Some(old_len));
        assert_eq!(hunk["new_len"].as_u64(), Some(new_len));
        if let Some(first_old_line) = lines.iter().find_map(|line| line["old_line"].as_u64()) {
            assert_eq!(hunk["old_start"].as_u64(), Some(first_old_line));
        }
        if let Some(first_new_line) = lines.iter().find_map(|line| line["new_line"].as_u64()) {
            assert_eq!(hunk["new_start"].as_u64(), Some(first_new_line));
        }
    }
}

#[test]
fn record_report_normalizes_json_for_display() {
    let record = sample_record(
        2,
        OperationType::CompressResponse,
        r#"{"b":2,"a":1}"#,
        r#"{"a":1}"#,
        (8, 4),
        None,
        CompressionMode::Active,
    );
    let json = serde_json::to_value(record_report(&record, 1)).unwrap();

    assert_eq!(json["chains"][0]["diff"]["normalization"], "json");
    let formatted = format_diff_report(&record_report(&record, 1), false);
    assert!(formatted.contains("JSON normalized for display"));
}

#[test]
fn record_report_normalizes_json_scalars_for_display() {
    let record = sample_record(
        92,
        OperationType::CompressResponse,
        " 42 ",
        "42",
        (2, 1),
        None,
        CompressionMode::Active,
    );
    let json = serde_json::to_value(record_report(&record, 1)).unwrap();

    assert_eq!(json["chains"][0]["diff"]["normalization"], "json");
    assert_eq!(
        json["chains"][0]["diff"]["hunks"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn terminal_report_escapes_content_control_characters() {
    let record = sample_record(
        93,
        OperationType::CompressResponse,
        "safe\n\u{1b}[2Jdanger\n",
        "safe\nclean\n",
        (10, 4),
        None,
        CompressionMode::Active,
    );
    let formatted = format_diff_report(&record_report(&record, 1), false);

    assert!(!formatted.contains('\u{1b}'));
    assert!(formatted.contains("danger"));
}

#[test]
fn terminal_report_escapes_persisted_metadata() {
    let mut record = sample_record(
        94,
        OperationType::CompressResponse,
        "before",
        "after",
        (10, 4),
        Some("tool\u{1b}]0;TOOL\u{7}"),
        CompressionMode::Active,
    );
    record.agent_id = "agent\u{1b}]0;AGENT\u{7}".to_string();
    record.session_id = Some("session\u{1b}]0;SESSION\u{7}".to_string());

    let detailed = format_diff_report(&record_report(&record, 1), false);
    let session = format_diff_report(
        &session_report(
            &diff_records(vec![record]),
            "session\u{1b}]0;SESSION\u{7}",
            20,
            DiffSort::Saved,
        ),
        false,
    );

    for formatted in [detailed, session] {
        assert!(!formatted.contains('\u{1b}'));
        assert!(!formatted.contains('\u{7}'));
        assert!(formatted.contains("SESSION"));
    }
}

#[test]
fn record_report_marks_missing_content() {
    let mut record = sample_record(
        3,
        OperationType::CompressSchema,
        "before",
        "after",
        (10, 4),
        None,
        CompressionMode::Active,
    );
    record.after_text = None;
    let json = serde_json::to_value(record_report(&record, 3)).unwrap();

    assert_eq!(
        json["chains"][0]["diff"]["omitted_reason"],
        "missing-content"
    );
    assert_eq!(json["chains"][0]["diff"]["available"], false);
}

#[test]
fn record_report_omits_oversized_content() {
    let before = "x".repeat(MAX_DIFF_INPUT_BYTES + 1);
    let record = sample_record(
        4,
        OperationType::CompressResponse,
        &before,
        "small",
        (300_000, 2),
        None,
        CompressionMode::Active,
    );
    let json = serde_json::to_value(record_report(&record, 3)).unwrap();

    assert_eq!(
        json["chains"][0]["diff"]["omitted_reason"],
        "content-too-large"
    );
}

#[test]
fn active_stages_link_without_double_counting_intermediate_tokens() {
    let first = sample_record(
        10,
        OperationType::RewriteCommand,
        "raw output",
        "filtered",
        (100, 60),
        Some("tool-chain"),
        CompressionMode::Active,
    );
    let second = sample_record(
        11,
        OperationType::CompressResponse,
        "filtered",
        "short",
        (60, 30),
        Some("tool-chain"),
        CompressionMode::Active,
    );
    let report = tool_use_report(
        &diff_records(vec![second, first]),
        "session-1",
        "tool-chain",
        3,
    );
    let json = serde_json::to_value(report).unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 1);
    assert_eq!(json["chains"][0]["status"], "linked");
    assert_eq!(json["chains"][0]["before_tokens"], 100);
    assert_eq!(json["chains"][0]["after_tokens"], 30);
    assert_eq!(json["chains"][0]["saved_tokens"], 70);
    assert_eq!(json["chains"][0]["stages"].as_array().unwrap().len(), 2);
}

#[test]
fn disconnected_records_for_same_tool_are_split() {
    let first = sample_record(
        20,
        OperationType::CompressResponse,
        "one",
        "two",
        (20, 10),
        Some("tool-split"),
        CompressionMode::Active,
    );
    let second = sample_record(
        21,
        OperationType::CompressToon,
        "different",
        "three",
        (10, 5),
        Some("tool-split"),
        CompressionMode::Active,
    );
    let json = serde_json::to_value(tool_use_report(
        &diff_records(vec![first, second]),
        "session-1",
        "tool-split",
        3,
    ))
    .unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 2);
    assert_eq!(json["split_chains"], true);
}

#[test]
fn records_without_tool_ids_remain_standalone() {
    let first = sample_record(
        30,
        OperationType::CompressSchema,
        "one",
        "two",
        (20, 10),
        None,
        CompressionMode::Active,
    );
    let second = sample_record(
        31,
        OperationType::CompressSchema,
        "two",
        "three",
        (10, 5),
        None,
        CompressionMode::Active,
    );
    let json = serde_json::to_value(session_report(
        &diff_records(vec![first, second]),
        "session-1",
        20,
        DiffSort::Saved,
    ))
    .unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 2);
    assert!(json["chains"]
        .as_array()
        .unwrap()
        .iter()
        .all(|chain| chain["status"] == "standalone"));
}

#[test]
fn dry_run_records_do_not_link_and_report_emitted_input() {
    let first = sample_record(
        40,
        OperationType::CompressResponse,
        "raw",
        "predicted",
        (100, 40),
        Some("tool-dry"),
        CompressionMode::DryRun,
    );
    let second = sample_record(
        41,
        OperationType::CompressToon,
        "predicted",
        "smaller",
        (40, 20),
        Some("tool-dry"),
        CompressionMode::DryRun,
    );
    let json = serde_json::to_value(tool_use_report(
        &diff_records(vec![first, second]),
        "session-1",
        "tool-dry",
        3,
    ))
    .unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 2);
    assert_eq!(json["chains"][0]["emitted_tokens"], 100);
    assert_eq!(json["chains"][1]["emitted_tokens"], 40);
}

#[test]
fn mode_changes_split_otherwise_matching_records() {
    let first = sample_record(
        50,
        OperationType::CompressResponse,
        "raw",
        "middle",
        (100, 50),
        Some("tool-mode"),
        CompressionMode::Active,
    );
    let second = sample_record(
        51,
        OperationType::CompressToon,
        "middle",
        "small",
        (50, 20),
        Some("tool-mode"),
        CompressionMode::DryRun,
    );
    let json = serde_json::to_value(tool_use_report(
        &diff_records(vec![first, second]),
        "session-1",
        "tool-mode",
        3,
    ))
    .unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 2);
}

#[test]
fn rewrite_command_prefers_output_fields() {
    let mut record = sample_record(
        60,
        OperationType::RewriteCommand,
        "legacy before",
        "legacy after",
        (100, 10),
        Some("tool-output"),
        CompressionMode::Active,
    );
    record.before_output = Some("actual before".to_string());
    record.after_output = Some("actual after".to_string());
    let json = serde_json::to_value(record_report(&record, 3)).unwrap();
    let lines = json["chains"][0]["diff"]["hunks"][0]["lines"]
        .as_array()
        .unwrap();

    assert!(lines
        .iter()
        .any(|line| line["text"] == "actual before"));
    assert!(!lines
        .iter()
        .any(|line| line["text"] == "legacy before"));
}

#[test]
fn stage_json_includes_stash_metrics_when_recorded() {
    let record = sample_record(
        65,
        OperationType::CompressResponse,
        "before",
        "after",
        (100, 50),
        Some("tool-stash"),
        CompressionMode::Active,
    )
    .with_stash(Some(2), Some(0), Some(9));
    let json = serde_json::to_value(record_report(&record, 3)).unwrap();
    let stash = &json["chains"][0]["stages"][0]["stash"];

    assert_eq!(stash["writes"], 2);
    assert_eq!(stash["errors"], 0);
    assert_eq!(stash["size"], 9);
}

#[test]
fn session_report_sorts_by_savings_and_applies_chain_limit() {
    let small = sample_record(
        70,
        OperationType::CompressSchema,
        "small before",
        "small after",
        (20, 10),
        Some("small"),
        CompressionMode::Active,
    );
    let large = sample_record(
        71,
        OperationType::CompressSchema,
        "large before",
        "large after",
        (200, 20),
        Some("large"),
        CompressionMode::Active,
    );
    let json = serde_json::to_value(session_report(
        &diff_records(vec![small, large]),
        "session-1",
        1,
        DiffSort::Saved,
    ))
    .unwrap();

    assert_eq!(json["chains"].as_array().unwrap().len(), 1);
    assert_eq!(json["chains"][0]["tool_use_id"], "large");
    assert!(json["chains"][0].get("diff").is_none());
}

#[test]
fn session_report_can_sort_by_latest_time() {
    let older = sample_record(
        72,
        OperationType::CompressSchema,
        "older before",
        "older after",
        (200, 20),
        Some("older"),
        CompressionMode::Active,
    );
    let newer = sample_record(
        73,
        OperationType::CompressSchema,
        "newer before",
        "newer after",
        (20, 10),
        Some("newer"),
        CompressionMode::Active,
    );
    let json = serde_json::to_value(session_report(
        &diff_records(vec![older, newer]),
        "session-1",
        20,
        DiffSort::Time,
    ))
    .unwrap();

    assert_eq!(json["chains"][0]["tool_use_id"], "newer");
}

#[test]
fn terminal_color_is_opt_in() {
    let record = sample_record(
        80,
        OperationType::CompressResponse,
        "before",
        "after",
        (10, 5),
        None,
        CompressionMode::Active,
    );
    let report = record_report(&record, 3);

    assert!(!format_diff_report(&report, false).contains("\u{1b}["));
    assert!(format_diff_report(&report, true).contains("\u{1b}["));
}
