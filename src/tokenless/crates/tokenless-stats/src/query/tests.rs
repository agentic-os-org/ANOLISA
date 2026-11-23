use super::*;
use crate::record::{CompressionMode, OperationType, StatsRecord};
use chrono::Local;

fn test_record() -> StatsRecord {
    let mut r = StatsRecord::new(
        OperationType::CompressSchema,
        "copilot-shell".to_string(),
        1000,
        400,
        500,
        200,
    );
    r.id = 1;
    r.timestamp = Local::now();
    r.before_text = Some("original text".to_string());
    r.after_text = Some("compressed".to_string());
    r
}

#[test]
fn test_format_summary() {
    let records = vec![test_record()];
    let output = format_summary(&records, Some("Test Summary"), None, None);

    assert!(output.contains("Test Summary"));
    assert!(output.contains("Total Records: 1"));
    assert!(output.contains("Character Savings"));
    assert!(output.contains("Token Savings"));
}

#[test]
fn summaries_exclude_dry_run_rows_and_report_attribution() {
    let active = test_record();
    let mut dry = test_record();
    dry.id = 2;
    dry.mode = CompressionMode::DryRun;
    let mut truncated = test_record();
    truncated.id = 3;
    truncated.unrecoverable_truncations = Some(2);
    let records = vec![active, dry, truncated];
    let retrieve = RetrieveTotals {
        hits: 3,
        misses: 1,
        errors: 0,
        retrieved_tokens: 150,
    };

    let text = format_summary(&records, None, None, Some(&retrieve));
    assert!(text.contains("Total Records: 2 (1 dry-run records excluded)"));
    // Active gross = 2 x (400 - 200); net = 400 - 150.
    assert!(text.contains("Gross Savings:  400 tokens"));
    assert!(text.contains("Retrieved:      150 tokens"));
    assert!(text.contains("Net Savings:    250 tokens"));
    assert!(text.contains("Retrieves:      3 hits / 1 misses / 0 errors"));
    assert!(text.contains("Unrecoverable:  1 compressions with unmarked truncations, 2 events"));

    let json = format_summary_json(&records, None, Some(&retrieve));
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["dry_run_records_excluded"], 1);
    assert_eq!(parsed["total"]["records"], 2);
    let attribution = &parsed["attribution"];
    assert_eq!(attribution["gross_savings_tokens"], 400);
    assert_eq!(attribution["retrieved_tokens"], 150);
    assert_eq!(attribution["net_savings_tokens"], 250);
    assert_eq!(attribution["retrieve_hits"], 3);
    assert_eq!(attribution["retrieve_misses"], 1);
    assert_eq!(attribution["retrieve_errors"], 0);
    assert_eq!(
        attribution["compressions_with_unrecoverable_truncations"],
        1
    );
    assert_eq!(attribution["unrecoverable_truncation_events"], 2);
}

#[test]
fn summaries_without_retrieve_totals_omit_the_retrieve_lines() {
    let records = vec![test_record()];
    let text = format_summary(&records, None, None, None);
    assert!(text.contains("Gross Savings"));
    assert!(!text.contains("Retrieved:"));
    assert!(!text.contains("Net Savings"));

    let json = format_summary_json(&records, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["attribution"].get("retrieved_tokens").is_none());
    assert!(parsed["attribution"].get("gross_savings_tokens").is_some());
}

#[test]
fn test_format_list() {
    let records = vec![test_record()];
    let output = format_list(&records, 20);

    assert!(output.contains("Showing 1 record"));
    assert!(output.contains("[ID:1]"));
}

#[test]
fn test_format_show_with_compression() {
    let record = test_record();
    let output = format_show(&record);

    assert!(output.contains("=== Before ==="));
    assert!(output.contains("original text"));
    assert!(output.contains("=== After ==="));
    assert!(output.contains("compressed"));
}

#[test]
fn test_format_show_no_compression() {
    let mut r = StatsRecord::new(
        OperationType::CompressSchema,
        "test".to_string(),
        100,
        25,
        100,
        25,
    );
    r.id = 2;
    r.timestamp = Local::now();
    r.before_text = Some("same text".to_string());
    r.after_text = Some("same text".to_string());

    let output = format_show(&r);
    assert!(output.contains("no compression"));
    assert!(output.contains("same text"));
    assert!(!output.contains("=== After ==="));
}

#[test]
fn test_format_show_no_text_stored() {
    let mut r = StatsRecord::new(
        OperationType::CompressSchema,
        "test".to_string(),
        100,
        25,
        80,
        20,
    );
    r.id = 3;
    r.timestamp = Local::now();

    let output = format_show(&r);
    assert!(output.contains("no text content stored"));
}

#[test]
fn test_format_show_empty_after_is_not_no_compression() {
    // A compression that stripped all content records real savings
    // (400 -> 0 tokens here); `stats show` must not relabel it as
    // "(no compression)" and hide the empty result.
    let mut r = test_record();
    r.after_text = Some(String::new());
    r.after_chars = 0;
    r.after_tokens = 0;

    let output = format_show(&r);
    assert!(!output.contains("no compression"));
    assert!(output.contains("=== Before ==="));
    assert!(output.contains("original text"));
    assert!(output.contains("=== After ==="));
    assert!(output.contains("after content is empty"));
}

#[test]
fn test_format_show_missing_after_is_not_no_compression() {
    // before_text stored but after_text absent (legacy rows, external
    // writers): show the stored side, not a false "no compression" label.
    let mut r = test_record();
    r.after_text = None;

    let output = format_show(&r);
    assert!(!output.contains("no compression"));
    assert!(output.contains("=== Before ==="));
    assert!(output.contains("original text"));
    assert!(output.contains("after content not stored"));
}

#[test]
fn test_format_summary_json_valid() {
    let records = vec![test_record()];
    let output = format_summary_json(&records, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

    // schema_version
    assert_eq!(parsed.get("schema_version").unwrap(), "1.1");

    let total = parsed.get("total").unwrap();
    // StatsRecord::new(op, agent, before_chars=1000, before_tokens=400,
    //                  after_chars=500, after_tokens=200)
    assert_eq!(total.get("records").unwrap(), 1);
    assert_eq!(total.get("before_chars").unwrap(), 1000);
    assert_eq!(total.get("after_chars").unwrap(), 500);
    assert_eq!(total.get("before_tokens").unwrap(), 400);
    assert_eq!(total.get("after_tokens").unwrap(), 200);
    // absolute saved values (shiloong review feedback)
    assert_eq!(total.get("chars_saved").unwrap(), 500);
    assert_eq!(total.get("tokens_saved").unwrap(), 200);
    assert!(total.get("chars_saved_percent").unwrap().as_f64().unwrap() > 0.0);
    assert!(total.get("tokens_saved_percent").unwrap().as_f64().unwrap() > 0.0);

    let ops = parsed.get("by_operation").unwrap().as_object().unwrap();
    assert!(ops.contains_key("compress-schema"));
    let op = ops.get("compress-schema").unwrap();
    assert_eq!(op.get("records").unwrap(), 1);
    assert_eq!(op.get("chars_saved").unwrap(), 500);
    assert_eq!(op.get("tokens_saved").unwrap(), 200);
}

#[test]
fn test_format_summary_json_empty() {
    let records: Vec<StatsRecord> = vec![];
    let output = format_summary_json(&records, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

    let total = parsed.get("total").unwrap();
    assert_eq!(total.get("records").unwrap(), 0);
    assert_eq!(
        total.get("chars_saved_percent").unwrap().as_f64().unwrap(),
        0.0
    );
    assert_eq!(
        total.get("tokens_saved_percent").unwrap().as_f64().unwrap(),
        0.0
    );

    let ops = parsed.get("by_operation").unwrap().as_object().unwrap();
    assert!(ops.is_empty());
}

#[test]
fn test_format_summary_json_field_consistency() {
    let records = vec![test_record()];
    let output = format_summary_json(&records, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

    let total_keys: std::collections::BTreeSet<String> = parsed
        .get("total")
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let ops = parsed.get("by_operation").unwrap().as_object().unwrap();
    for (_op, val) in ops {
        let op_keys: std::collections::BTreeSet<String> =
            val.as_object().unwrap().keys().cloned().collect();
        assert_eq!(total_keys, op_keys, "field names must be identical");
    }
}

#[test]
fn test_format_summary_json_ordered_operations() {
    let mut r1 = test_record();
    r1.operation = OperationType::CompressResponse;

    let mut r2 = test_record();
    r2.operation = OperationType::RewriteCommand;

    let mut r3 = test_record();
    r3.operation = OperationType::CompressSchema;

    let records = vec![r2, r1, r3]; // intentionally unordered
    let output = format_summary_json(&records, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

    let ops = parsed.get("by_operation").unwrap().as_object().unwrap();
    let keys: Vec<&String> = ops.keys().collect();
    // BTreeMap sorts lexicographically
    assert_eq!(
        keys,
        vec!["compress-response", "compress-schema", "rewrite-command"]
    );
}

#[test]
fn test_format_summary_with_session_total() {
    let records = vec![test_record()];
    let output = format_summary(&records, Some("Test Summary"), Some(10_000), None);
    assert!(output.contains("Overall Savings vs Total Consumption"));
    assert!(output.contains("10,000 tokens"));
    assert!(output.contains("Tool Response:"));
    assert!(output.contains("Compression Rate:"));
    assert!(output.contains("Actual Savings:"));
    assert!(output.contains("within tool responses"));
}

#[test]
fn test_format_summary_without_session_total() {
    let records = vec![test_record()];
    let output = format_summary(&records, Some("Test Summary"), None, None);
    assert!(!output.contains("Overall Savings vs Total Consumption"));
}

#[test]
fn test_format_summary_json_with_session_total() {
    let records = vec![test_record()];
    let output = format_summary_json(&records, Some(10_000), None);
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    let total = parsed.get("total").unwrap();
    assert_eq!(total.get("session_total_tokens").unwrap(), 10_000);
    assert_eq!(total.get("actual_savings_tokens").unwrap(), 200);
    // 200 / 10000 = 2.0%
    let pct = total
        .get("actual_savings_percent")
        .unwrap()
        .as_f64()
        .unwrap();
    assert!((pct - 2.0).abs() < 0.1);
}

#[test]
fn test_format_summary_json_without_session_total() {
    let records = vec![test_record()];
    let output = format_summary_json(&records, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    let total = parsed.get("total").unwrap();
    assert!(total.get("session_total_tokens").is_none());
    assert!(total.get("actual_savings_percent").is_none());
}

#[test]
fn test_format_diff_no_diff_available() {
    let mut r = StatsRecord::new(
        OperationType::CompressSchema,
        "test".to_string(),
        100,
        25,
        80,
        20,
    );
    r.id = 1;
    r.timestamp = Local::now();

    let output = format_show(&r);
    assert!(output.contains("no text content stored"));
}

fn compare_record(
    op: OperationType,
    mode: CompressionMode,
    before: usize,
    after: usize,
) -> StatsRecord {
    let mut r = StatsRecord::new(op, "cli".to_string(), before * 4, before, after * 4, after)
        .with_mode(mode);
    r.timestamp = Local::now();
    r
}

#[test]
fn test_format_compare_totals_and_delta() {
    // Baseline (dry-run): context reached = before_tokens.
    // Tokenless (active): context reached = after_tokens.
    let baseline = vec![
        compare_record(
            OperationType::CompressSchema,
            CompressionMode::DryRun,
            400,
            200,
        ),
        compare_record(
            OperationType::CompressResponse,
            CompressionMode::DryRun,
            1000,
            300,
        ),
    ];
    let tokenless = vec![
        compare_record(
            OperationType::CompressSchema,
            CompressionMode::Active,
            400,
            200,
        ),
        compare_record(
            OperationType::CompressResponse,
            CompressionMode::Active,
            1000,
            300,
        ),
    ];

    let out = format_compare(&baseline, &tokenless);
    // baseline total = 400+1000 = 1400; tokenless total = 200+300 = 500
    assert!(out.contains("TOTAL"));
    assert!(out.contains(&format!("{:>12}", 1400)));
    assert!(out.contains(&format!("{:>14}", 500)));
    assert!(out.contains("compress-schema"));
    assert!(out.contains("compress-response"));
}

#[test]
fn test_format_compare_json_fields() {
    let baseline = vec![compare_record(
        OperationType::CompressSchema,
        CompressionMode::DryRun,
        400,
        200,
    )];
    let tokenless = vec![compare_record(
        OperationType::CompressSchema,
        CompressionMode::Active,
        400,
        200,
    )];
    let parsed: serde_json::Value =
        serde_json::from_str(&format_compare_json(&baseline, &tokenless)).unwrap();
    assert_eq!(parsed.get("schema_version").unwrap(), "1.0");
    assert_eq!(parsed.get("baseline_tokens").unwrap(), 400);
    assert_eq!(parsed.get("tokenless_tokens").unwrap(), 200);
    assert_eq!(parsed.get("saved_tokens").unwrap(), 200);
    assert!(parsed.get("saved_percent").unwrap().as_f64().unwrap() > 0.0);
}

#[test]
fn test_format_compare_empty() {
    let out = format_compare(&[], &[]);
    assert!(out.contains("TOTAL"));
    // no baseline tokens → saved 0%, no panic
    let parsed: serde_json::Value = serde_json::from_str(&format_compare_json(&[], &[])).unwrap();
    assert_eq!(parsed.get("saved_percent").unwrap().as_f64().unwrap(), 0.0);
}

#[test]
fn test_format_summary_with_zero_session_total() {
    let records = vec![test_record()];
    let output = format_summary(&records, Some("Test"), Some(0), None);
    assert!(output.contains("Overall Savings vs Total Consumption"));
}

#[test]
fn test_format_list_with_more_records_than_limit() {
    let records: Vec<StatsRecord> = (0..5)
        .map(|i| {
            let mut r = test_record();
            r.id = i + 1;
            r
        })
        .collect();
    let output = format_list(&records, 3);
    assert!(output.contains("Showing 3 record"));
    assert!(output.contains("and 2 more"));
}

#[test]
fn test_format_compare_zero_baseline() {
    let baseline = vec![compare_record(
        OperationType::CompressSchema,
        CompressionMode::DryRun,
        0,
        0,
    )];
    let tokenless = vec![compare_record(
        OperationType::CompressSchema,
        CompressionMode::Active,
        0,
        0,
    )];
    let out = format_compare(&baseline, &tokenless);
    assert!(out.contains("0.0%"));
}

#[test]
fn test_format_list_empty() {
    let output = format_list(&[], 10);
    assert!(output.contains("No records found"));
}
