//! Frozen v1 outputs plus explicit evidence-completeness regressions.

use asc_capability_pii_scan::{
    CoverageStatus, PiiScanOptions, PiiScanner, SCANNER_VERSION, ScanError, Verdict,
};
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
        assert_eq!(result.summary.scanner_version, "2.2.0");
        assert!(
            result
                .findings
                .iter()
                .all(|f| f.metadata["engine"] == "regex_v2")
        );
        let mut actual = serde_json::to_value(result).unwrap();
        actual.as_object_mut().unwrap().remove("elapsed_ms");
        for key in [
            "execution_status",
            "coverage",
            "input_sha256",
            "scanned_input_sha256",
            "scanned_bytes",
            "ruleset_id",
            "scanner_version",
        ] {
            actual["summary"].as_object_mut().unwrap().remove(key);
        }
        let mut expected = case["expected"].clone();
        // Preserve the historical oracle; classify the intentional V2
        // confidence change instead of regenerating Python outputs.
        if case["text"] == "sk-test9876543210ZYXWVUTSRQPONMLKJIHGFEDCBA" {
            assert_eq!(expected["findings"][0]["confidence"], 0.51);
            expected["findings"][0]["confidence"] = serde_json::json!(0.86);
        }
        // Version 2.2.0 supersedes three frozen expectations. Each replacement
        // below first re-asserts the historical value it replaces, so the
        // migration stays auditable instead of silently accepting new output.
        if case["text"]
            .as_str()
            .unwrap()
            .ends_with("\nid=LTAI5tQnKxExampleToken12")
        {
            // Index 3: a contextless `accessKeySecret` no longer claims Alibaba
            // Cloud ownership. The generic type keeps the denial and now uses a
            // whole-value mask. The scored confidence rises from 0.59 to 0.94
            // because the retained negative-context penalty no longer applies.
            let secret = &expected["findings"][3];
            assert_eq!(secret["type"], "aliyun_access_key_secret");
            assert_eq!(secret["confidence"], 0.59);
            assert_eq!(secret["metadata"]["field"], "accessKeySecret");
            expected["findings"][3]["type"] = serde_json::json!("generic_secret_field");
            expected["findings"][3]["confidence"] = serde_json::json!(0.94);
            expected["findings"][3]["evidence_redacted"] = serde_json::json!("[REDACTED_SECRET]");
            // Index 4: the retired value-prefix rule no longer reports a bare
            // `id=LTAI…`. The remaining four findings still produce a denial.
            let bare_id = expected["findings"].as_array().unwrap()[4].clone();
            assert_eq!(bare_id["type"], "aliyun_access_key_id");
            assert_eq!(bare_id["confidence"], 0.69);
            expected["findings"].as_array_mut().unwrap().remove(4);
            expected["summary"]["total"] = serde_json::json!(4);
            expected["summary"]["by_type"] = serde_json::json!({
                "api_key": 1,
                "bearer_token": 1,
                "generic_secret_field": 1,
                "jwt": 1,
            });
            expected["summary"]["by_category"] = serde_json::json!({"credential": 4});
            expected["summary"]["by_severity"] = serde_json::json!({"deny": 4});
            assert_eq!(expected["verdict"], "deny");
        }
        // Cases 198 and 199 place `LTAIabcdefghijklmnop` next to a character that
        // is unassigned in the frozen Unicode 14 word class. Both were positive
        // only because the retired prefix rule ignored word boundaries.
        if matches!(
            case["text"].as_str().unwrap(),
            "\u{1e4d0}LTAIabcdefghijklmnop" | "LTAIabcdefghijklmnop\u{1e4d0}"
        ) {
            assert_eq!(expected["findings"][0]["type"], "aliyun_access_key_id");
            assert_eq!(expected["findings"][0]["confidence"], 0.92);
            expected["findings"] = serde_json::json!([]);
            expected["summary"]["total"] = serde_json::json!(0);
            expected["summary"]["by_type"] = serde_json::json!({});
            expected["summary"]["by_category"] = serde_json::json!({});
            expected["summary"]["by_severity"] = serde_json::json!({});
            expected["verdict"] = serde_json::json!("pass");
        }
        upgrade_cjk_expectation(case["text"].as_str().unwrap(), &mut expected);
        // Engine provenance is intentionally versioned; all remaining fields match.
        for finding in expected["findings"].as_array_mut().unwrap() {
            finding["metadata"]["engine"] = serde_json::json!("regex_v2");
        }
        assert_eq!(actual, expected, "v1 fixture {index}: {:?}", case["text"]);
    }
}

// Keep the six superseded V1 observations in their original corpus. Only these
// named CJK boundary cases change; every other report field is still compared.
fn upgrade_cjk_expectation(text: &str, expected: &mut Value) {
    if matches!(
        text,
        "alice@company.cn中 bob@securecorp.cn." | "中alice@company.cn" | "alice@company.cn中"
    ) {
        let existing = usize::from(text.ends_with(" bob@securecorp.cn."));
        assert_eq!(expected["summary"]["total"], existing);
        assert_eq!(expected["findings"].as_array().unwrap().len(), existing);
        let start = usize::from(text.starts_with('中'));
        expected["findings"].as_array_mut().unwrap().insert(
            0,
            serde_json::json!({
                "type":"email", "category":"personal_data", "severity":"warn", "confidence":0.82,
                "evidence_redacted":"a***@company.cn", "span":{"start":start,"end":start+16},
                "metadata":{"validator":"email_syntax","detector":"regex","engine":"regex_v1"},
                "raw_evidence":"alice@company.cn"
            }),
        );
        expected["verdict"] = serde_json::json!("warn");
        expected["summary"]["total"] = serde_json::json!(existing + 1);
        expected["summary"]["by_type"] = serde_json::json!({"email":existing+1});
        expected["summary"]["by_category"] = serde_json::json!({"personal_data":existing+1});
        expected["summary"]["by_severity"] = serde_json::json!({"warn":existing+1});
        if let Some(redacted) = expected.get_mut("redacted_text") {
            *redacted = serde_json::json!(
                redacted
                    .as_str()
                    .unwrap()
                    .replace("alice@company.cn", "a***@company.cn")
            );
        }
    } else if matches!(
        text,
        "sk-AAAAAAAAAAAAAAAAAAAA中"
            | "中sk-AAAAAAAAAAAAAAAAAAAA"
            | "sk-AAAAAAAAAAAAAAAAAAAA-BBBB中"
    ) {
        let extended = text.contains("-BBBB");
        assert_eq!(expected["summary"]["total"], usize::from(extended));
        if extended {
            assert_eq!(
                expected["findings"][0]["raw_evidence"],
                "sk-AAAAAAAAAAAAAAAAAAAA-"
            );
        } else {
            assert_eq!(expected["findings"], serde_json::json!([]));
        }
        let value = text.trim_matches('中');
        let start = usize::from(text.starts_with('中'));
        expected["findings"] = serde_json::json!([{
            "type":"api_key", "category":"credential", "severity":"deny", "confidence":0.86,
            "evidence_redacted":if extended {"sk-A...[REDACTED]...BBBB"} else {"sk-A...[REDACTED]...AAAA"},
            "span":{"start":start,"end":start+value.len()},
            "metadata":{"pattern":"token_prefix","detector":"regex","engine":"regex_v1"},
            "raw_evidence":value
        }]);
        expected["verdict"] = serde_json::json!("deny");
        expected["summary"]["total"] = serde_json::json!(1);
        expected["summary"]["by_type"] = serde_json::json!({"api_key":1});
        expected["summary"]["by_category"] = serde_json::json!({"credential":1});
        expected["summary"]["by_severity"] = serde_json::json!({"deny":1});
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
fn candidate_search_preserves_versioned_boundaries_and_redacted_spans() {
    let scanner = PiiScanner::new().unwrap();
    let padding = "x".repeat(8192);
    let token = "İabcdefghijklmno";
    let key = "LTAIabcdefghijkl";
    let id = "１１０１０５１９４９１２３１００２Ｘ";
    let phone = "13800138000";
    for (kind, left, value, right, masked) in [
        (
            "bearer_token",
            "🙂bEaReR\u{1c}",
            token,
            "",
            Some("İabc...[REDACTED]...lmno"),
        ),
        (
            "bearer_token",
            "中Bearer ",
            token,
            "",
            Some("İabc...[REDACTED]...lmno"),
        ),
        // A bare LTAI value is no longer a finding on its own: the bare-value
        // rule was removed in favour of the official alias assignments, so
        // this inherited positive becomes the bare negative.
        ("aliyun_access_key_id", "\u{11f02}", key, "!", None),
        (
            "aliyun_access_key_id",
            "ALIBABA_CLOUD_ACCESS_KEY_ID=",
            key,
            "",
            Some("LTAI...[REDACTED]...ijkl"),
        ),
        ("aliyun_access_key_id", "_", key, "", None),
        (
            // A bare `LTAI…` value is no longer attributed to Alibaba Cloud, so
            // CJK adjacency no longer turns it into a finding.
            "aliyun_access_key_id",
            "",
            key,
            "中",
            None,
        ),
        ("cn_id", "中", id, "!", Some("１１０***********００２Ｘ")),
        ("cn_id", "９", id, "", None),
        ("cn_id", "", id, "_", None),
        // Reject the country-prefixed candidate, then retry inside it.
        ("phone_cn", "０86-", phone, "中", Some("138****8000")),
        ("phone_cn", "", phone, "９", None),
    ] {
        let prefix = format!("{padding}\n{left}");
        let text = format!("{prefix}{value}{right}");
        let report = scanner
            .scan(
                &text,
                &PiiScanOptions {
                    redact_output: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(
            report.findings.len(),
            usize::from(masked.is_some()),
            "{kind}: {left}{value}{right}"
        );
        if let Some(finding) = report.findings.first() {
            let start = prefix.chars().count();
            assert_eq!(finding.pii_type, kind);
            assert_eq!(
                (finding.span.start, finding.span.end),
                (start, start + value.chars().count())
            );
        }
        assert_eq!(
            report.redacted_text.unwrap(),
            format!("{prefix}{}{right}", masked.unwrap_or(value))
        );
    }
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

#[test]
fn scanner_version_reflects_detection_semantics() {
    // CJK token boundaries change findings and spans; version the report and
    // ruleset digest while retaining the previously added token prefixes.
    assert_eq!(SCANNER_VERSION, "2.2.0");
    let report = PiiScanner::new()
        .unwrap()
        .scan(
            "github_pat_11ABCDEFG0abcdefghijabcdefghij1234567890ABCDEFGHIJKL",
            &PiiScanOptions::default(),
        )
        .unwrap();
    assert_eq!(report.summary.scanner_version, "2.2.0");
    assert!(report.findings.iter().any(|f| f.pii_type == "api_key"));
}

#[test]
fn repeated_embedded_token_prefixes_stay_bounded_and_reach_the_trailing_credential() {
    // A rejected api_key candidate advances the scan only past its start, so
    // every embedded github_pat_ prefix re-runs the candidate pattern. The
    // bounded github_pat_ tail keeps each rescan constant instead of making
    // the whole remaining word quadratic, and the trailing valid credential
    // is still reached and reported.
    let token = format!("sk-{}", "B".repeat(32));
    let text = format!("x{} {token}", "github_pat_".repeat(32_000));
    let report = PiiScanner::new()
        .unwrap()
        .scan(&text, &PiiScanOptions::default())
        .unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    let start = text.find("sk-").unwrap();
    let api_keys: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.pii_type == "api_key")
        .collect();
    assert_eq!(api_keys.len(), 1);
    assert_eq!(api_keys[0].span.start, start);
    assert_eq!(api_keys[0].span.end, start + token.len());
}
