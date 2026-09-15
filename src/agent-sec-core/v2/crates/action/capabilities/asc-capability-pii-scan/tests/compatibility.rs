//! Frozen v1 outputs plus explicit evidence-completeness regressions.

use asc_capability_pii_scan::{CoverageStatus, PiiScanOptions, PiiScanner, ScanError, Verdict};
use serde_json::Value;

#[test]
fn frozen_v1_builtin_responses_match() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/v1.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(
        cases.len() >= 50,
        "freeze the existing v1 scanner regressions"
    );
    let scanner = PiiScanner::new().unwrap();
    for (index, case) in cases.iter().enumerate() {
        let options: PiiScanOptions = serde_json::from_value(case["options"].clone()).unwrap();
        let result = scanner
            .scan(case["text"].as_str().unwrap(), &options)
            .unwrap();
        let mut actual = serde_json::to_value(result).unwrap();
        actual.as_object_mut().unwrap().remove("elapsed_ms");
        for key in [
            "execution_status",
            "coverage",
            "input_sha256",
            "scanned_input_sha256",
            "scanned_bytes",
            "ruleset_id",
        ] {
            actual["summary"].as_object_mut().unwrap().remove(key);
        }
        let expected = case["expected"].clone();
        assert_eq!(actual, expected, "v1 fixture {index}: {:?}", case["text"]);
    }
}

#[test]
fn prefix_coverage_and_digests_are_explicit() {
    let scanner = PiiScanner::new().unwrap();
    let report = scanner
        .scan(
            "你好a",
            &PiiScanOptions {
                max_bytes: Some(4),
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Partial);
    assert_eq!(report.summary.coverage.reasons, ["input_truncated"]);
    assert_eq!(report.summary.bytes_scanned, 4);
    assert_eq!(report.summary.scanned_bytes, 3);
    assert_ne!(
        report.summary.input_sha256,
        report.summary.scanned_input_sha256
    );
    assert_eq!(report.redacted_text.as_deref(), Some("你"));
    assert!(matches!(
        scanner.scan(
            "",
            &PiiScanOptions {
                max_bytes: Some(0),
                ..Default::default()
            }
        ),
        Err(ScanError::InvalidLimit)
    ));
}

#[test]
fn empty_and_large_clean_inputs_are_not_silently_truncated() {
    let scanner = PiiScanner::new().unwrap();
    for text in [String::new(), "z".repeat(1_048_577)] {
        let result = scanner.scan(&text, &PiiScanOptions::default()).unwrap();
        assert_eq!(result.verdict, Verdict::Pass);
        assert_eq!(result.summary.scanned_bytes, text.len());
        assert_eq!(result.summary.coverage.status, CoverageStatus::Complete);
    }
}

#[test]
fn long_token_candidates_do_not_hide_findings_at_the_end() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let token = format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#),
        URL_SAFE_NO_PAD.encode(r#"{"sub":"123"}"#),
        URL_SAFE_NO_PAD.encode([0; 32]),
    );
    let text = format!("{}.abcdefgh.ijklmnop \n{token}", "z".repeat(1_048_577));
    let report = PiiScanner::new()
        .unwrap()
        .scan(&text, &PiiScanOptions::default())
        .unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].pii_type, "jwt");
    assert_eq!(report.findings[0].span.end, text.chars().count());
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
}

#[test]
fn long_api_key_values_keep_full_spans_without_a_backtracking_stack() {
    let text = format!("sk-{}", "A".repeat(1_048_577));
    let report = PiiScanner::new()
        .unwrap()
        .scan(&text, &PiiScanOptions::default())
        .unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].pii_type, "api_key");
    assert_eq!(report.findings[0].span.end, text.len());
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
}

#[test]
fn long_invalid_email_domains_do_not_hide_later_addresses() {
    let text = format!("alice@{}.com bob@company.cn", "A".repeat(1_048_577));
    let report = PiiScanner::new()
        .unwrap()
        .scan(&text, &PiiScanOptions::default())
        .unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].pii_type, "email");
    assert_eq!(report.findings[0].span.end, text.len());
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
}

#[test]
fn mixed_unicode_positions_and_overlapping_credentials_are_redacted() {
    let scanner = PiiScanner::new().unwrap();
    let text = "🙂e\u{301} 密码=abcdefghijklmnop api_key=sk-abcdefghijklmnopqrstuvwxyz123456";
    let result = scanner
        .scan(
            text,
            &PiiScanOptions {
                redact_output: true,
                raw_evidence: true,
                ..Default::default()
            },
        )
        .unwrap();
    for finding in &result.findings {
        let extracted: String = text
            .chars()
            .skip(finding.span.start)
            .take(finding.span.end - finding.span.start)
            .collect();
        assert_eq!(finding.raw_evidence.as_deref(), Some(extracted.as_str()));
    }
    assert!(result.findings.len() >= 2);
    assert!(!result.redacted_text.unwrap().contains("abcdefghijklmnop"));
}
