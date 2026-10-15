//! Dense input must not require retaining all matches or redacted text.

use asc_capability_pii_scan::{CoverageStatus, PiiRuleSet, PiiScanOptions, PiiScanner, Verdict};
use std::fmt::Write as _;
use std::sync::Arc;

#[test]
fn dense_input_keeps_complete_counts_and_tail_deny() {
    let scanner = PiiScanner::new().unwrap();
    let mut input = "a@b.cn ".repeat(590_000);
    input.push_str("Authorization: Bearer abcdefghijklmnopqrstuvwx12345678");
    assert!(input.len() < 4 * 1024 * 1024);
    let report = scanner
        .scan(
            &input,
            &PiiScanOptions {
                raw_evidence: true,
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.summary.total, 590_001);
    assert_eq!(report.summary.by_type["email"], 590_000);
    assert_eq!(report.verdict, Verdict::Deny);
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.findings.len(), 2);
    assert!(report.summary.findings_truncated);
    assert!(report.summary.redacted_text_omitted);
    assert!(report.findings.iter().all(|f| f.raw_evidence.is_none()));
    assert!(serde_json::to_vec_pretty(&report).unwrap().len() < 512 * 1024);
}

#[test]
fn large_overlapping_custom_matches_do_not_clone_raw_input() {
    let mut yaml = String::new();
    for n in 0..100 {
        writeln!(
            yaml,
            "- type: custom_{n}\n  regex: '(?s).+'\n  severity: deny"
        )
        .unwrap();
    }
    let scanner = PiiScanner::with_rules(Arc::new(PiiRuleSet::from_yaml(yaml.as_bytes()).unwrap()));
    let input = "😀".repeat(160_000);
    let report = scanner
        .scan(
            &input,
            &PiiScanOptions {
                raw_evidence: true,
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(report.summary.total > 0);
    assert_eq!(report.verdict, Verdict::Deny);
    assert!(report.summary.findings_truncated);
    for finding in &report.findings {
        assert_eq!(finding.span.start, 0);
        assert_eq!(finding.span.end, 160_000);
        assert!(finding.raw_evidence.is_none());
        assert_eq!(finding.metadata["evidence_omitted"], true);
    }
    assert_eq!(report.redacted_text.as_deref(), Some("[CUSTOM_0_REDACTED]"));
}

#[test]
fn email_sentence_long_period_runs_keep_complete_coverage() {
    let input = format!(
        "alice@company.cn{} bob@securecorp.cn.",
        ".".repeat(1_048_577)
    );
    let report = PiiScanner::new()
        .unwrap()
        .scan(
            &input,
            &PiiScanOptions {
                raw_evidence: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.verdict, Verdict::Warn);
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.findings.len(), 2);
    assert_eq!(
        report.findings[0].raw_evidence.as_deref(),
        Some("alice@company.cn")
    );
    assert_eq!(
        report.findings[1].raw_evidence.as_deref(),
        Some("bob@securecorp.cn")
    );
    assert_eq!(report.findings[1].span.end, input.len() - 1);
}

#[test]
fn email_sentence_dense_findings_keep_tail_deny() {
    let input = format!("{} password=abcdefghijklmnop", "a@b.cn. ".repeat(20_000));
    let report = PiiScanner::new()
        .unwrap()
        .scan(&input, &PiiScanOptions::default())
        .unwrap();
    assert_eq!(report.verdict, Verdict::Deny);
    assert_eq!(report.summary.total, 20_001);
    assert_eq!(report.summary.by_type["email"], 20_000);
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
}

#[test]
fn email_sentence_truncated_period_run_retains_partial_coverage() {
    let input = "alice@company.cn...evil";
    let scanner = PiiScanner::new().unwrap();
    assert!(
        scanner
            .scan(input, &PiiScanOptions::default())
            .unwrap()
            .findings
            .is_empty()
    );
    let report = scanner
        .scan(
            input,
            &PiiScanOptions {
                max_bytes: Some("alice@company.cn.".len()),
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.verdict, Verdict::Warn);
    assert_eq!(report.summary.coverage.status, CoverageStatus::Partial);
    assert_eq!(report.summary.coverage.reasons, ["input_truncated"]);
    assert_ne!(
        report.summary.input_sha256,
        report.summary.scanned_input_sha256
    );
    assert_eq!(report.redacted_text.as_deref(), Some("a***@company.cn."));
}
