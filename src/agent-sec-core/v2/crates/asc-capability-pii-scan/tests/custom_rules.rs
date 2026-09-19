//! Central configuration, bounded execution, and shared immutable state.

use asc_capability_pii_scan::{
    CoverageStatus, CustomRuleStatus, PiiRuleSet, PiiScanOptions, PiiScanner, Verdict,
};
use std::fmt::Write as _;
use std::sync::Arc;

fn scanner(document: &str) -> PiiScanner {
    PiiScanner::with_rules(Arc::new(
        PiiRuleSet::from_yaml(document.as_bytes()).unwrap(),
    ))
}

fn rule_document(count: usize) -> String {
    let mut document = String::new();
    for i in 0..count {
        writeln!(document, "- type: marker{i}\n  regex: TOKEN{i}").unwrap();
    }
    document
}

#[test]
fn invalid_documents_disable_the_whole_collection_with_safe_codes() {
    for (document, code) in [
        ("[", "invalid_yaml"),
        ("{}", "top_level_not_list"),
        (
            "- type: marker\n  regex: TOKEN\n  unknown: secret\n",
            "invalid_rule_schema",
        ),
        ("- type: 123\n  regex: TOKEN\n", "invalid_rule_schema"),
        ("- type: bad-name\n  regex: TOKEN\n", "invalid_rule_type"),
        ("- type: email\n  regex: TOKEN\n", "reserved_rule_type"),
        ("- type: marker\n  regex: ''\n", "invalid_rule_schema"),
        ("- type: marker\n  regex: '['\n", "invalid_regex"),
        ("- type: marker\n  regex: '(?|a|b)'\n", "invalid_regex"),
        ("- type: marker\n  regex: '(?<=a+)b'\n", "invalid_regex"),
        (
            "- type: marker\n  regex: 'a*'\n",
            "regex_matches_empty_text",
        ),
        (
            "- type: marker\n  regex: TOKEN\n  severity: high\n",
            "invalid_rule_schema",
        ),
        (
            "- type: marker\n  regex: TOKEN\n  regex: OTHER\n",
            "invalid_yaml",
        ),
        (
            "- &rule {type: marker, regex: TOKEN}\n- *rule\n",
            "invalid_yaml",
        ),
        ("[]\n---\n[]\n", "invalid_yaml"),
        (
            "- type: marker\n  regex: TOKEN\n- type: marker\n  regex: OTHER\n",
            "duplicate_rule_type",
        ),
        (
            "- type: first\n  regex: TOKEN\n- type: second\n  regex: '['\n",
            "invalid_regex",
        ),
    ] {
        let report = scanner(document)
            .scan("TOKEN alice@company.cn", &PiiScanOptions::default())
            .unwrap();
        assert_eq!(
            report.summary.custom_rules.status,
            CustomRuleStatus::Invalid,
            "{document}"
        );
        assert_eq!(
            report.summary.custom_rules.error_code.as_deref(),
            Some(code),
            "{document}"
        );
        assert_eq!(report.summary.custom_rules.rule_count, 0);
        assert_eq!(report.summary.coverage.status, CoverageStatus::Partial);
        assert_eq!(report.summary.coverage.reasons, ["custom_rules_invalid"]);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].pii_type, "email");
        assert_eq!(report.verdict, Verdict::Warn);
    }
}

#[test]
fn custom_patterns_use_the_versioned_native_dialect() {
    for (pattern, text, expected) in [
        (r"^INTERNAL-[0-9]+$", "INTERNAL-1234", Some((0, 13))),
        (r"^INTERNAL-[0-9]+$", "INTERNAL-1234\n", None),
        (r"(?m)^INTERNAL-[0-9]+$", "INTERNAL-1234\n", Some((0, 13))),
        (r"INTERNAL\h+[0-9]+", "INTERNALAB1234", Some((0, 14))),
        (r"INTERNAL\h+[0-9]+", "INTERNAL 1234", None),
        (r"INTERNAL[ \t]+[0-9]+", "INTERNAL 1234", Some((0, 13))),
        (r"INTERNAL\H+[0-9]+", "INTERNAL__1234", Some((0, 14))),
        (r"INTERNAL-[0-9]+\Z", "INTERNAL-1234\n", Some((0, 13))),
        (r"INTERNAL-[0-9]+\z", "INTERNAL-1234\n", None),
        (r"\<INTERNAL\>", "INTERNAL", Some((0, 8))),
        (r"\<INTERNAL\>", "XINTERNAL", None),
        (r"(?i)internal", "Internal", Some((0, 8))),
        (r"(?i)internal", "İNTERNAL", None),
        (r"(?i:internal)", "INTERNAL", Some((0, 8))),
        (r"(?x) INTERNAL \- [0-9]+", "INTERNAL-1234", Some((0, 13))),
        (r"[A-Z&&[^X]]+", "ABC", Some((0, 3))),
        (r"[A-Z&&[^X]]+", "XXX", None),
        (r"[a-z--[aeiou]]+", "bcdf", Some((0, 4))),
        (r"[a-z--[aeiou]]+", "aeiou", None),
        (r"[ab~~bc]+", "ac", Some((0, 2))),
        (r"[a||b]+", "ab", Some((0, 2))),
        (r"[\h]+", "0Af", Some((0, 3))),
        (r"[^^]INTERNAL-1234$", "xINTERNAL-1234", Some((0, 14))),
        (r"(?# [)(?i)internal", "INTERNAL", Some((0, 8))),
        (r"^(?P<x>REF)?(?(<x>)-REF|OTHER)$", "REF-REF", Some((0, 7))),
        (r"^(?P<x>REF)?(?(<x>)-REF|OTHER)$", "OTHER", Some((0, 5))),
        (r"^(?P<x>REF)?(?(<x>)-REF|OTHER)$", "REF-OTHER", None),
    ] {
        let document = format!("- type: marker\n  regex: '{pattern}'\n");
        let report = scanner(&document)
            .scan(text, &PiiScanOptions::default())
            .unwrap();
        assert_eq!(
            report.summary.custom_rules.status,
            CustomRuleStatus::Loaded,
            "{pattern}"
        );
        assert_eq!(
            report.summary.coverage.status,
            CoverageStatus::Complete,
            "{pattern}"
        );
        assert_eq!(report.summary.scanner_version, "2.0.0");
        let spans: Vec<_> = report
            .findings
            .iter()
            .map(|finding| {
                assert_eq!(finding.pii_type, "marker");
                assert_eq!(finding.metadata["engine"], "fancy_regex");
                (finding.span.start, finding.span.end)
            })
            .collect();
        assert_eq!(spans, expected.into_iter().collect::<Vec<_>>(), "{pattern}");
    }
}

#[test]
fn supported_syntax_and_escaped_literals_keep_their_match_boundaries() {
    // Literal punctuation and shared syntax keep their intended spans even when
    // nearby characters also have a native regex operator meaning.
    for (pattern, text, expected) in [
        (r"INTERNAL\$", "INTERNAL$", Some((0, 9))),
        (r"[$]+", "$$", Some((0, 2))),
        (r"\\h", r"\h", Some((0, 2))),
        (r"[\[\]$]+", "[$]", Some((0, 3))),
        (r"[\<\>]+", "<>", Some((0, 2))),
        (r"[a\&\&]+", "a&&", Some((0, 3))),
        (r"[]$]+", "]$", Some((0, 2))),
        (r"[^^]+", "REF", Some((0, 3))),
        (r"[^^]+", "^^", None),
        (r"[(?#]+", "(?#", Some((0, 3))),
        (r"(?P<x>REF)-(?P=x)", "🙂 REF-REF", Some((2, 9))),
        (r"(?P<x>REF)-(?P=x)", "REF-OTHER", None),
        (r"(?<=ID:)REF(?=!)", "ID:REF!", Some((3, 6))),
        (r"(?m)^REF", "prefix\nREF", Some((7, 10))),
        (r"(?s:REF.*END)", "REF\nEND", Some((0, 7))),
        (r"(?s:REF(?-s:.))", "REF\n", None),
        (r"编号-[0-9]+", "🙂 编号-123", Some((2, 8))),
    ] {
        let document = format!("- type: marker\n  regex: '{pattern}'\n");
        let report = scanner(&document)
            .scan(text, &PiiScanOptions::default())
            .unwrap();
        assert_eq!(
            report.summary.custom_rules.status,
            CustomRuleStatus::Loaded,
            "{pattern}"
        );
        assert_eq!(
            report.summary.coverage.status,
            CoverageStatus::Complete,
            "{pattern}"
        );
        let spans: Vec<_> = report
            .findings
            .iter()
            .map(|f| {
                assert_eq!(f.pii_type, "marker");
                (f.span.start, f.span.end)
            })
            .collect();
        assert_eq!(spans, expected.into_iter().collect::<Vec<_>>(), "{pattern}");
    }
}

#[test]
fn file_pattern_rule_and_nesting_bounds_are_enforced() {
    let too_long = format!("- type: marker\n  regex: '{}'\n", "x".repeat(2049));
    let too_deep = format!(
        "- type: marker\n  regex: '{}x{}'\n",
        "(".repeat(65),
        ")".repeat(65)
    );
    let rules = rule_document(101);
    let yaml_depth = format!("{}x{}", "[".repeat(65), "]".repeat(65));
    for (content, code) in [
        (vec![b' '; 256 * 1024 + 1], "file_too_large"),
        (vec![0xff], "invalid_utf8"),
        (too_long.into_bytes(), "invalid_rule_schema"),
        (too_deep.into_bytes(), "invalid_regex"),
        (rules.into_bytes(), "too_many_rules"),
        (yaml_depth.into_bytes(), "invalid_yaml"),
    ] {
        let set = PiiRuleSet::from_yaml(&content).unwrap();
        assert_eq!(set.custom_rules().status, CustomRuleStatus::Invalid);
        assert_eq!(set.custom_rules().error_code.as_deref(), Some(code));
    }
    let mut maximum_file = b"- type: marker\n  regex: TOKEN\n#".to_vec();
    maximum_file.resize(256 * 1024, b' ');
    assert_eq!(
        PiiRuleSet::from_yaml(&maximum_file)
            .unwrap()
            .custom_rules()
            .status,
        CustomRuleStatus::Loaded
    );
    let maximum_rules = rule_document(100);
    assert_eq!(
        PiiRuleSet::from_yaml(maximum_rules.as_bytes())
            .unwrap()
            .custom_rules()
            .rule_count,
        100
    );
    let maximum_pattern = format!("- type: marker\n  regex: '{}'\n", "x".repeat(2048));
    assert_eq!(
        PiiRuleSet::from_yaml(maximum_pattern.as_bytes())
            .unwrap()
            .custom_rules()
            .status,
        CustomRuleStatus::Loaded
    );
    let supported_depth = format!(
        "- type: marker\n  regex: '{}x{}'\n",
        "(".repeat(63),
        ")".repeat(63)
    );
    assert_eq!(
        PiiRuleSet::from_yaml(supported_depth.as_bytes())
            .unwrap()
            .custom_rules()
            .status,
        CustomRuleStatus::Loaded
    );
    // The engine counts the root parse frame toward its recursion limit.
    let engine_limited = format!(
        "- type: marker\n  regex: '{}x{}'\n",
        "(".repeat(64),
        ")".repeat(64)
    );
    assert_eq!(
        PiiRuleSet::from_yaml(engine_limited.as_bytes())
            .unwrap()
            .custom_rules()
            .error_code
            .as_deref(),
        Some("invalid_regex")
    );
}

#[test]
fn custom_and_builtin_findings_share_unicode_ordering_and_redaction() {
    let detector = scanner(
        "- type: ticket_id\n  regex: 'TKT-[0-9]{4}'\n  severity: warn\n- type: protected_email\n  regex: 'alice@company\\.cn'\n",
    );
    let text = "🙂 alice@company.cn TKT-1234";
    let report = detector
        .scan(
            text,
            &PiiScanOptions {
                raw_evidence: true,
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.findings.len(), 3);
    assert_eq!(report.verdict, Verdict::Deny);
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.summary.custom_rules.rule_count, 2);
    assert_eq!(report.findings[0].span.start, 2);
    assert_eq!(report.findings[1].span, report.findings[0].span);
    assert_eq!(
        report.redacted_text.as_deref(),
        Some("🙂 [PROTECTED_EMAIL_REDACTED] [TICKET_ID_REDACTED]")
    );
    for finding in &report.findings {
        let value: String = text
            .chars()
            .skip(finding.span.start)
            .take(finding.span.end - finding.span.start)
            .collect();
        assert_eq!(finding.raw_evidence.as_deref(), Some(value.as_str()));
    }
}

#[test]
fn custom_limits_preserve_existing_findings_and_report_partial_coverage() {
    let detector =
        scanner("- type: warning\n  regex: W\n  severity: warn\n- type: denial\n  regex: X\n");
    let exact = detector
        .scan(&"X".repeat(100), &PiiScanOptions::default())
        .unwrap();
    assert_eq!(exact.findings.len(), 100);
    assert_eq!(exact.summary.coverage.status, CoverageStatus::Complete);
    let limited = detector
        .scan(
            &format!("{} W alice@company.cn", "X".repeat(101)),
            &PiiScanOptions::default(),
        )
        .unwrap();
    assert_eq!(limited.summary.by_type.get("denial"), Some(&100));
    assert!(!limited.summary.by_type.contains_key("warning"));
    assert_eq!(limited.summary.by_type.get("email"), Some(&1));
    assert!(limited.summary.custom_rules.truncated);
    assert_eq!(limited.summary.coverage.status, CoverageStatus::Partial);
    assert!(
        limited
            .summary
            .coverage
            .reasons
            .iter()
            .any(|r| r == "custom_findings_limited")
    );
    assert_eq!(limited.verdict, Verdict::Deny);
}

#[test]
fn regex_vm_errors_and_zero_width_matches_never_become_complete_passes() {
    let backtracking =
        scanner("- type: expensive\n  regex: '(a|b|ab)*(?>c)'\n- type: marker\n  regex: MARKER\n");
    let report = backtracking
        .scan(
            &format!("{}! MARKER alice@company.cn", "ab".repeat(32)),
            &PiiScanOptions::default(),
        )
        .unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Partial);
    assert!(report.findings.iter().any(|f| f.pii_type == "email"));
    assert!(
        report.summary.custom_rules.runtime_error_count > 0
            || report.summary.custom_rules.budget_exhausted
    );
    let zero_width = scanner("- type: lookahead\n  regex: '(?=TOKEN)'\n")
        .scan("TOKEN", &PiiScanOptions::default())
        .unwrap();
    assert_eq!(zero_width.summary.custom_rules.runtime_error_count, 1);
    assert_eq!(zero_width.summary.coverage.status, CoverageStatus::Partial);
    assert_eq!(zero_width.summary.coverage.reasons, ["custom_empty_match"]);
    assert_eq!(zero_width.verdict, Verdict::Pass);
}

#[test]
fn file_changes_require_a_new_set_and_never_change_an_inflight_collection() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("rules.yaml");
    std::fs::write(&file, "- type: old_marker\n  regex: OLD\n").unwrap();
    let old = Arc::new(PiiRuleSet::load(Some(&file)).unwrap());
    let old_id = old.id().to_owned();
    let old_scanner = PiiScanner::with_rules(old);
    std::fs::write(&file, "- type: new_marker\n  regex: NEW\n").unwrap();
    let new = Arc::new(PiiRuleSet::load(Some(&file)).unwrap());
    assert_ne!(old_id, new.id());
    let new_scanner = PiiScanner::with_rules(new);
    for (detector, expected) in [(old_scanner, "old_marker"), (new_scanner, "new_marker")] {
        let report = detector
            .scan("OLD NEW", &PiiScanOptions::default())
            .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].pii_type, expected);
    }
    std::fs::write(&file, vec![b' '; 256 * 1024 + 1]).unwrap();
    let large = PiiRuleSet::load(Some(&file)).unwrap();
    assert_eq!(
        large.custom_rules().error_code.as_deref(),
        Some("file_too_large")
    );
    assert!(large.custom_rules().ruleset_sha256.is_none());
    assert_eq!(
        PiiRuleSet::load(Some(directory.path()))
            .unwrap()
            .custom_rules()
            .error_code
            .as_deref(),
        Some("read_error")
    );
    assert_eq!(
        PiiRuleSet::load(Some(std::path::Path::new("relative.yaml")))
            .unwrap()
            .custom_rules()
            .error_code
            .as_deref(),
        Some("invalid_path")
    );
}

#[test]
fn shared_rules_have_no_cross_request_findings_or_limit_counters() {
    let shared = Arc::new(scanner("- type: marker\n  regex: TOKEN\n"));
    let workers: Vec<_> = (0..8)
        .map(|i| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let text = if i % 2 == 0 {
                    "TOKEN ".repeat(101)
                } else {
                    "clean".into()
                };
                let report = shared.scan(&text, &PiiScanOptions::default()).unwrap();
                assert_eq!(report.summary.custom_rules.truncated, i % 2 == 0);
                assert_eq!(report.findings.len(), if i % 2 == 0 { 100 } else { 0 });
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let clean = shared.scan("clean", &PiiScanOptions::default()).unwrap();
    assert_eq!(clean.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(clean.summary.custom_rules.runtime_error_count, 0);
}
