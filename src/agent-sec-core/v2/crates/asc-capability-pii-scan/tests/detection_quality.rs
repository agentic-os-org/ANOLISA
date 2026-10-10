//! Labeled synthetic positives and hard negatives for the supported detector scope.

use asc_capability_pii_scan::{CoverageStatus, PiiScanOptions, PiiScanner, Verdict};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

// HMAC-SHA256 signed with the synthetic key "independent-review-synthetic-key"; no live secret.
const EMPTY_CLAIMS_JWT: &str =
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.e30.y4pnSuvml8A03eqm8Uvz1gZ5ZQX_WGHDAdzFmzhAR5g";

#[test]
fn credit_card_structure_and_complete_spans() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/credit_cards.json")).unwrap();
    let scanner = PiiScanner::new().unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let report = scanner
            .scan(
                text,
                &PiiScanOptions {
                    raw_evidence: true,
                    redact_output: true,
                    include_low_confidence: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let cards: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.pii_type == "credit_card")
            .collect();
        let actual: Vec<_> = cards
            .iter()
            .map(|f| f.raw_evidence.as_deref().unwrap())
            .collect();
        let expected: Vec<_> = case["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(actual, expected, "{}", case["id"]);
        for card in cards {
            let value: String = text
                .chars()
                .skip(card.span.start)
                .take(card.span.end - card.span.start)
                .collect();
            assert_eq!(Some(value.as_str()), card.raw_evidence.as_deref());
        }
        assert_eq!(
            report.redacted_text.as_deref(),
            case["redacted_text"].as_str(),
            "{}",
            case["id"]
        );
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    }
}

#[test]
fn long_invalid_card_expression_preserves_later_card() {
    let text = format!(
        "{}4111111111111111; card-4111111111111111",
        "1-".repeat(50_000)
    );
    let report = PiiScanner::new()
        .unwrap()
        .scan(
            &text,
            &PiiScanOptions {
                raw_evidence: true,
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    let cards: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.pii_type == "credit_card")
        .collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        (cards[0].span.start, cards[0].span.end),
        (text.len() - 16, text.len())
    );
    assert_eq!(
        report.redacted_text.unwrap(),
        format!("{}[REDACTED_CARD:1111]", &text[..text.len() - 16])
    );
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
}

#[test]
fn email_sentence_boundaries_preserve_spans_and_punctuation() {
    let scanner = PiiScanner::new().unwrap();
    let mut suffixes: Vec<String> = [
        "",
        ".",
        "...",
        ". Next sentence",
        ".\n",
        ".\t",
        ".\u{a0}",
        ".\u{1c}",
        ",",
        "。",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    suffixes.extend(
        "\"')]}>,;:!?，。；：！？、）］｝】〕》〉」』”’"
            .chars()
            .map(|c| format!(".{c}")),
    );
    for suffix in suffixes {
        let prefix = "备注🙂e\u{301} ";
        let email = "alice@company.co.uk";
        let input = format!("{prefix}{email}{suffix}");
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
        assert_eq!(report.verdict, Verdict::Warn, "{input}");
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.findings.len(), 1, "{input}");
        let finding = &report.findings[0];
        assert_eq!(finding.pii_type, "email");
        assert_eq!(finding.raw_evidence.as_deref(), Some(email));
        assert_eq!(
            (finding.span.start, finding.span.end),
            (prefix.chars().count(), prefix.chars().count() + email.len())
        );
        assert!((finding.confidence - 0.82).abs() < f64::EPSILON);
        assert_eq!(finding.metadata["validator"], "email_syntax");
        assert_eq!(
            report.redacted_text.as_deref(),
            Some(format!("{prefix}a***@company.co.uk{suffix}").as_str())
        );
    }
}

#[test]
fn email_sentence_rejects_domain_continuations_without_hiding_later_email() {
    let scanner = PiiScanner::new().unwrap();
    let mut suffixes: Vec<String> = [
        ".123", ".c", ".-bad", "._bad", "..evil", ".cn-", "中", "_", "-", ".中", ".🙂", "...123",
        ".../",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    suffixes.extend("/@+%`([{<\\".chars().map(|c| format!(".{c}")));
    suffixes.push(format!(".{}", "a".repeat(64)));
    for suffix in suffixes {
        let input = format!("alice@company.cn{suffix} bob@securecorp.cn.");
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
        assert_eq!(report.verdict, Verdict::Warn, "{input}");
        assert_eq!(report.findings.len(), 1, "{input}");
        assert_eq!(
            report.findings[0].raw_evidence.as_deref(),
            Some("bob@securecorp.cn"),
            "{input}"
        );
        assert_eq!(
            report.redacted_text.as_deref(),
            Some(format!("alice@company.cn{suffix} b***@securecorp.cn.").as_str())
        );
    }
}

#[test]
fn email_sentence_periods_preserve_existing_validation() {
    let scanner = PiiScanner::new().unwrap();
    let mut invalid = vec![
        ".alice@company.cn".to_owned(),
        "alice.@company.cn".into(),
        "alice..bob@company.cn".into(),
        "alice@-company.cn".into(),
        "alice@company-.cn".into(),
        "alice@company..cn".into(),
        "alice@bad_domain.cn".into(),
        format!("{}@company.cn", "a".repeat(65)),
        format!("alice@{}.cn", "a".repeat(64)),
    ];
    invalid.push(format!(
        "{}@{}.{}.{}.cn",
        "a".repeat(64),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(63)
    ));
    for email in invalid {
        let report = scanner
            .scan(&format!("{email}."), &PiiScanOptions::default())
            .unwrap();
        assert!(report.findings.is_empty(), "{email}");
    }
    for (input, context) in [
        ("alice@example.com.", "reserved_domain"),
        ("ssh://alice@company.cn.", "remote_identity"),
    ] {
        assert_eq!(
            scanner
                .scan(input, &PiiScanOptions::default())
                .unwrap()
                .verdict,
            Verdict::Pass
        );
        let report = scanner
            .scan(
                input,
                &PiiScanOptions {
                    include_low_confidence: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert!((report.findings[0].confidence - 0.35).abs() < f64::EPSILON);
        assert_eq!(report.findings[0].metadata["context"], context);
    }
    for suffix in ["/", "@", "+", "%", "`", "(", "🙂", "\u{301}"] {
        let report = scanner
            .scan(
                &format!("alice@company.cn{suffix}"),
                &PiiScanOptions {
                    raw_evidence: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].raw_evidence.as_deref(),
            Some("alice@company.cn")
        );
    }
}

#[test]
fn all_eleven_types_have_positive_and_negative_examples() {
    let scanner = PiiScanner::new().unwrap();
    for (kind, positive, negative) in [
        ("email", "alice@company.cn", "alice..bob@company.cn"),
        ("phone_cn", "13812345678", "1381234567"),
        ("credit_card", "4111111111111111", "4111111111111112"),
        ("cn_id", "11010519491231002X", "110105194912310021"),
        ("api_key", "sk-abcdefghijklmnopqrstuvwxyz123456", "sk-short"),
        (
            "bearer_token",
            "Bearer abcdefghijklmnopqrstuvwxyz",
            "Bearer short",
        ),
        ("aliyun_access_key_id", "LTAIAbCdEfGhIjKlMnOp", "LTAIshort"),
        (
            "aliyun_access_key_secret",
            "access_key_secret=AbCdEfGhIjKlMnOp",
            "access_key_secret=short",
        ),
        (
            "generic_secret_field",
            "password=abcdefghijklmnop",
            "password=short",
        ),
        ("jwt", EMPTY_CLAIMS_JWT, "abcdefgh.ijklmnop.qrstuvwx"),
        (
            "private_key",
            "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PUBLIC KEY-----",
        ),
    ] {
        for (input, expected) in [(positive, true), (negative, false)] {
            let report = scanner.scan(input, &PiiScanOptions::default()).unwrap();
            assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
            assert_eq!(
                report.findings.iter().any(|f| f.pii_type == kind),
                expected,
                "{kind}: {input}"
            );
        }
    }
}

#[test]
fn zero_placeholders_are_not_cards_and_unicode_ids_keep_validation() {
    let scanner = PiiScanner::new().unwrap();
    for input in [
        "0000000000000000",
        "００００００００００００００００",
        "0000 0000 0000 0000",
    ] {
        let report = scanner.scan(input, &PiiScanOptions::default()).unwrap();
        assert!(!report.findings.iter().any(|f| f.pii_type == "credit_card"));
    }
    for (input, expected) in [
        ("1101051949１２31002X", true),
        ("１１０１０５１９４９１２３１００２Ｘ", true),
        ("１１０１０５１９４９１２３１００２ｘ", true),
        ("１１０１０５１９４９１２３１００１１", true),
        ("１１０１０５１９４９０２３１００２Ｘ", false),
        ("１１０１０５１９４９１２３１００２１", false),
    ] {
        let report = scanner
            .scan(
                input,
                &PiiScanOptions {
                    raw_evidence: true,
                    redact_output: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let id = report.findings.iter().find(|f| f.pii_type == "cn_id");
        assert_eq!(id.is_some(), expected, "{input}");
        if let Some(id) = id {
            assert_eq!(id.raw_evidence.as_deref(), Some(input));
            assert_eq!((id.span.start, id.span.end), (0, 18));
            assert_ne!(report.redacted_text.as_deref(), Some(input));
        }
    }
}

#[test]
fn jwt_json_shape_does_not_inherit_python_integer_or_recursion_limits() {
    let scanner = PiiScanner::new().unwrap();
    let nested = format!("{}0{}", "[".repeat(1100), "]".repeat(1100));
    let payloads = [
        "{}".to_owned(),
        " { }  ".to_owned(),
        format!("{{\"n\":{}}}", "7".repeat(5000)),
        format!("{{\"nested\":{nested}}}"),
    ];
    for payload in payloads {
        let token = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#),
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode([0_u8; 32])
        );
        let report = scanner
            .scan(
                &token,
                &PiiScanOptions {
                    raw_evidence: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let jwt = report
            .findings
            .iter()
            .find(|f| f.pii_type == "jwt")
            .unwrap();
        assert_eq!(jwt.raw_evidence.as_deref(), Some(token.as_str()));
        assert_eq!((jwt.span.start, jwt.span.end), (0, token.len()));
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    }
    for payload in [r#"{"n":01}"#, r#"{"n":1e}"#, r#"{"nested":[0}"#, "[]"] {
        let token = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#),
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode([0_u8; 32])
        );
        let report = scanner.scan(&token, &PiiScanOptions::default()).unwrap();
        assert!(
            !report.findings.iter().any(|f| f.pii_type == "jwt"),
            "{payload}"
        );
    }
}
