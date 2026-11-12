//! CJK prose delimits structured tokens without changing generic Unicode values.

use asc_capability_pii_scan::{CoverageStatus, PiiScanOptions, PiiScanner, Verdict};

const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";

#[test]
fn structured_tokens_keep_exact_spans_and_redaction_next_to_cjk_prose() {
    let scanner = PiiScanner::new().unwrap();
    for prose in [
        "中文",
        "かぎ",
        "カギ",
        "ｶｷﾞ",
        "ー",
        "한글",
        "\u{1100}\u{1161}",
        "㐀",
        "𠀀",
    ] {
        for (kind, label, value, masked, verdict) in [
            (
                "api_key",
                "",
                KEY,
                "sk-a...[REDACTED]...GHIJ",
                Verdict::Deny,
            ),
            (
                "api_key",
                "",
                "AKIA1234567890ABCDEF",
                "AKIA...[REDACTED]...CDEF",
                Verdict::Deny,
            ),
            (
                "email",
                "",
                "alice@company.cn",
                "a***@company.cn",
                Verdict::Warn,
            ),
            (
                "cn_id",
                "",
                "11010519491231002X",
                "110***********002X",
                Verdict::Warn,
            ),
            (
                "bearer_token",
                "Bearer ",
                "Abcdefghijklmnop",
                "Abcd...[REDACTED]...mnop",
                Verdict::Deny,
            ),
        ] {
            for (left, right) in [(prose, ""), ("", prose), (prose, prose)] {
                let text = format!("{left}{label}{value}{right}");
                let report = scanner
                    .scan(
                        &text,
                        &PiiScanOptions {
                            raw_evidence: true,
                            redact_output: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
                assert_eq!(report.verdict, verdict, "{text}");
                assert_eq!(report.findings.len(), 1, "{text}");
                let finding = &report.findings[0];
                assert_eq!(finding.pii_type, kind, "{text}");
                assert_eq!(finding.raw_evidence.as_deref(), Some(value), "{text}");
                let start = left.chars().count() + label.chars().count();
                assert_eq!(
                    (finding.span.start, finding.span.end),
                    (start, start + value.chars().count()),
                    "{text}"
                );
                assert_eq!(
                    report.redacted_text.as_deref(),
                    Some(format!("{left}{label}{masked}{right}").as_str()),
                    "{text}"
                );
            }
        }
    }
}

#[test]
fn a_bare_ltai_value_is_not_attributed_to_alibaba_cloud_next_to_cjk() {
    // Version 2.2.0 retired the `LTAI` value-prefix rule: provider ownership now
    // requires a documented environment-variable assignment.
    let scanner = PiiScanner::new().unwrap();
    let report = scanner
        .scan("中文LTAIabcdefghijkl", &PiiScanOptions::default())
        .unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.verdict, Verdict::Pass);
    assert!(report.findings.is_empty());
}

#[test]
fn cjk_ends_the_complete_api_key_including_punctuation() {
    let scanner = PiiScanner::new().unwrap();
    for (suffix, masked) in [
        ("", "sk-a...[REDACTED]...GHIJ"),
        ("_", "sk-a...[REDACTED]...HIJ_"),
        ("-", "sk-a...[REDACTED]...HIJ-"),
        ("=", "sk-a...[REDACTED]...HIJ="),
        ("-=", "sk-a...[REDACTED]...IJ-="),
        ("-BBBB", "sk-a...[REDACTED]...BBBB"),
    ] {
        let value = format!("{KEY}{suffix}");
        let text = format!("中文{value}カギ");
        let report = scanner
            .scan(
                &text,
                &PiiScanOptions {
                    raw_evidence: true,
                    redact_output: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.findings.len(), 1, "{text}");
        assert_eq!(
            report.findings[0].raw_evidence.as_deref(),
            Some(value.as_str())
        );
        assert_eq!(
            (report.findings[0].span.start, report.findings[0].span.end),
            (2, 2 + value.len())
        );
        assert_eq!(report.redacted_text.unwrap(), format!("中文{masked}カギ"));
    }
}

#[test]
fn cjk_boundaries_preserve_other_unicode_and_ascii_continuations() {
    let scanner = PiiScanner::new().unwrap();
    for text in [
        format!("x{KEY}"),
        format!("é{KEY}"),
        format!("{KEY}é"),
        "élise@company.cn".into(),
        "alice@company.cné".into(),
        "alice@company.cn.123".into(),
        "alice@company.cn.é".into(),
        "alice@company.cn.中国".into(),
        "alice@company.cn...中文".into(),
        "_LTAIabcdefghijkl".into(),
        "LTAIabcdefghijkl_".into(),
        "xBearer Abcdefghijklmnop".into(),
        "11010519491231002X_".into(),
    ] {
        let report = scanner
            .scan(
                &text,
                &PiiScanOptions {
                    redact_output: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.verdict, Verdict::Pass, "{text}");
        assert!(report.findings.is_empty(), "{text}");
        assert_eq!(report.redacted_text.as_deref(), Some(text.as_str()));
    }
    for (text, expected) in [
        ("邮箱alice@company.cn。中文", "邮箱a***@company.cn。中文"),
        ("alice@company.cn... 中文", "a***@company.cn... 中文"),
    ] {
        let report = scanner
            .scan(
                text,
                &PiiScanOptions {
                    redact_output: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.redacted_text.as_deref(), Some(expected));
    }
}

#[test]
fn assignment_fallback_detects_keys_without_truncating_unicode_passwords() {
    let scanner = PiiScanner::new().unwrap();
    for (left, right) in [("中文", ""), ("", "中文"), ("中文", "中文")] {
        let text = format!("{left}api_key={KEY}{right}");
        let report = scanner
            .scan(
                &text,
                &PiiScanOptions {
                    raw_evidence: true,
                    redact_output: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.pii_type == "api_key")
            .unwrap();
        assert_eq!(finding.raw_evidence.as_deref(), Some(KEY));
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(!report.redacted_text.unwrap().contains(KEY));
    }
    let value = "复杂密码Abcdefgh987中文";
    let report = scanner
        .scan(
            &format!("password={value}"),
            &PiiScanOptions {
                raw_evidence: true,
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].pii_type, "generic_secret_field");
    assert_eq!(report.findings[0].raw_evidence.as_deref(), Some(value));
    assert!(!report.redacted_text.unwrap().contains(value));
}
