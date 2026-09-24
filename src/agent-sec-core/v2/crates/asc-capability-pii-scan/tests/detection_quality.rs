//! Labeled synthetic positives and hard negatives for the supported detector scope.

use asc_capability_pii_scan::{CoverageStatus, PiiScanOptions, PiiScanner};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

// HMAC-SHA256 signed with the synthetic key "independent-review-synthetic-key"; no live secret.
const EMPTY_CLAIMS_JWT: &str =
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.e30.y4pnSuvml8A03eqm8Uvz1gZ5ZQX_WGHDAdzFmzhAR5g";

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
