//! Regression coverage for assignment handoff, references and ID redaction.

use asc_action_runtime::{
    ActionRuntime, CapabilityExecutor, ExecutionControl, Invocation, SecurityEventSink,
};
use asc_action_types::{ActionId, CallerIdentity};
use asc_capability_pii_scan::{CoverageStatus, PiiScanOptions, PiiScanner, Severity, Verdict};
use asc_capability_pii_scan::{PiiAuditProjector, PiiRuleSet, PiiScanExecutor, PiiScanRequest};
use asc_security_events::{EventResult, SecurityEvent};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn options() -> PiiScanOptions {
    PiiScanOptions {
        redact_output: true,
        ..Default::default()
    }
}

#[test]
fn provider_handoff_preserves_generic_assignment_syntax() {
    let scanner = PiiScanner::new().unwrap();
    let value = "abcdefghijklmnop1234";
    for separator in [": ", "：", "\n= "] {
        let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET{separator}{value}");
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.findings.len(), 1, "{text}");
        let finding = &report.findings[0];
        assert_eq!(finding.pii_type, "generic_secret_field", "{text}");
        assert_eq!(finding.severity, Severity::Deny);
        assert_eq!(finding.evidence_redacted, "[REDACTED_SECRET]");
        assert!(!report.redacted_text.unwrap().contains(value));
    }

    for text in [
        format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}"),
        format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET=\"{value}\""),
        format!("export ALIBABA_CLOUD_ACCESS_KEY_SECRET='{value}'"),
    ] {
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.findings.len(), 1, "{text}");
        assert_eq!(report.findings[0].pii_type, "aliyun_access_key_secret");
        assert_eq!(report.findings[0].metadata["provider"], "alibaba_cloud");
        assert_eq!(report.findings[0].evidence_redacted, "[REDACTED_SECRET]");
    }

    let text = "ALIBABA_CLOUD_ACCESS_KEY_SECRET=abcdefghijkl`mnop1234";
    let report = scanner.scan(text, &options()).unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].pii_type, "aliyun_access_key_secret");
    assert!(!report.redacted_text.unwrap().contains("mnop1234"));
}

#[test]
fn nested_assignments_cannot_be_suppressed_by_the_provider_cursor() {
    let scanner = PiiScanner::new().unwrap();
    for text in [
        "ALIBABA_CLOUD_ACCESS_KEY_ID=\"x ALIBABA_CLOUD_ACCESS_KEY_SECRET=abcdefghijklmnop1234\"",
        "ALIBABA_CLOUD_ACCESS_KEY_ID=\"<ALIBABA_CLOUD_ACCESS_KEY_SECRET=abcdefghijklmnop1234>\"",
        "ALIBABA_CLOUD_ACCESS_KEY_SECRET=\"x ALIBABA_CLOUD_ACCESS_KEY_SECRET=abcdefghijklmnop1234\"",
    ] {
        let report = scanner.scan(text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Deny, "{text}");
        assert!(report.findings.iter().any(|f| f.severity == Severity::Deny));
        assert!(
            report
                .findings
                .iter()
                .all(|f| !f.evidence_redacted.contains("1234")),
            "{text}"
        );
        let redacted = report.redacted_text.unwrap();
        assert!(!redacted.contains("abcdefghijklmnop1234"), "{redacted}");
        assert!(!redacted.contains("1234"), "{redacted}");
    }
}

#[test]
fn concatenated_shell_fragments_are_redacted_as_a_whole() {
    let scanner = PiiScanner::new().unwrap();
    for value in [
        "$(cat file)literal-secret",
        "$NAME,literal-secret",
        "$NAME\"literal-secret\"",
        "${NAME}'literal-secret'",
        "`cat file`literal-secret",
        "\"prefix\\\"literal-secret\"",
    ] {
        let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}");
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Deny, "{text}");
        assert_eq!(report.findings[0].evidence_redacted, "[REDACTED_SECRET]");
        let redacted = report.redacted_text.unwrap();
        assert!(!redacted.contains("literal-secret"), "{redacted}");
    }
}

#[test]
fn shell_control_operators_do_not_hide_following_assignments() {
    let scanner = PiiScanner::new().unwrap();
    for separator in ["&&", "||", "|", "&", ";", ">", "<", "(", ")"] {
        let text = format!(
            "ALIBABA_CLOUD_ACCESS_KEY_ID=abc{separator}ALIBABA_CLOUD_ACCESS_KEY_SECRET=secret42"
        );
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Deny, "{text}");
        assert!(
            report.findings.iter().any(|f| {
                f.pii_type == "aliyun_access_key_secret"
                    && f.evidence_redacted == "[REDACTED_SECRET]"
            }),
            "{text}"
        );
        assert!(
            !report.redacted_text.unwrap().contains("secret42"),
            "{text}"
        );
    }
}

#[test]
fn multiline_values_do_not_hide_later_log_assignments() {
    let scanner = PiiScanner::new().unwrap();
    for text in [
        "ALICLOUD_ACCESS_KEY_ID=\"foo ALICLOUD_ACCESS_KEY_SECRET=short123\"",
        "ALICLOUD_ACCESS_KEY_ID=<ALICLOUD_ACCESS_KEY_SECRET=short123>",
        "ALICLOUD_ACCESS_KEY_ID=\"foo\nALICLOUD_ACCESS_KEY_SECRET=short123",
        "ALICLOUD_ACCESS_KEY_ID=\"foo\r\nALICLOUD_ACCESS_KEY_SECRET=short123\"",
        "ALICLOUD_ACCESS_KEY_ID=$(echo foo\nALICLOUD_ACCESS_KEY_SECRET=short123)",
        "ALICLOUD_ACCESS_KEY_ID=\"foo\nALICLOUD_ACCESS_KEY_SECRET=short123&&OSS_SECURITY_TOKEN=token123\"",
    ] {
        let report = scanner.scan(text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Deny, "{text}");
        assert!(
            report.findings.iter().any(|f| {
                f.pii_type == "aliyun_access_key_secret"
                    && f.evidence_redacted == "[REDACTED_SECRET]"
            }),
            "{text}"
        );
        let redacted = report.redacted_text.unwrap();
        assert!(!redacted.contains("short123"), "{redacted}");
        if text.contains("token123") {
            assert!(!redacted.contains("token123"), "{redacted}");
        }
    }
}

#[test]
fn many_lines_inside_an_unclosed_value_remain_visible() {
    let text = format!(
        "ALICLOUD_ACCESS_KEY_ID=\"begin\n{}",
        "ALICLOUD_ACCESS_KEY_SECRET=short123\n".repeat(512)
    );
    let report = PiiScanner::new().unwrap().scan(&text, &options()).unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.verdict, Verdict::Deny);
    assert_eq!(report.summary.by_type["aliyun_access_key_secret"], 512);
    assert!(!report.redacted_text.unwrap().contains("short123"));
}

#[test]
fn long_provider_value_keeps_complete_coverage() {
    let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={}", "a".repeat(1_100_000));
    let report = PiiScanner::new().unwrap().scan(&text, &options()).unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.verdict, Verdict::Deny);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(
        report.redacted_text.as_deref(),
        Some("ALIBABA_CLOUD_ACCESS_KEY_SECRET=[REDACTED_SECRET]")
    );
}

#[test]
fn only_complete_references_are_excluded() {
    let scanner = PiiScanner::new().unwrap();
    for value in [
        "$MY_KEY",
        "${MY_KEY}",
        "%MY_KEY%",
        "$(cat /run/secrets/alibaba)",
        "your_access_key_secret",
        "your-api-key-here",
    ] {
        for text in [
            format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}"),
            format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET=\"{value}\""),
        ] {
            let report = scanner.scan(&text, &options()).unwrap();
            assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
            assert_eq!(report.summary.total, 0, "{text}");
        }
    }
    for value in [
        "${MY_KEY}suffix1234",
        "$MY_KEY-suffix1234",
        "%MY_KEY%suffix1234",
        "$(cat-secret)suffix1234",
        "$(printf secret123)",
        "$(cat /run/secret extra123)",
        "your_access_key_ABC123456",
        "your-secret-ABC123456",
    ] {
        let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}");
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.findings.len(), 1, "{text}");
        assert_eq!(report.findings[0].evidence_redacted, "[REDACTED_SECRET]");
        assert!(!report.redacted_text.unwrap().contains(value));
    }
}

#[test]
fn deeply_nested_assignments_keep_the_innermost_deny() {
    let scanner = PiiScanner::new().unwrap();
    let explicit = "ALICLOUD_ACCESS_KEY_ID=\"x ALICLOUD_ACCESS_KEY_ID='x ALICLOUD_ACCESS_KEY_SECRET=short123'\"";
    let mut dense = "ALICLOUD_ACCESS_KEY_SECRET=short123".to_owned();
    for _ in 0..32 {
        dense = format!("ALICLOUD_ACCESS_KEY_ID=$(echo {dense})");
    }
    for text in [explicit, dense.as_str()] {
        let report = scanner.scan(text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Deny, "{text}");
        assert_eq!(report.summary.by_type["aliyun_access_key_secret"], 1);
        assert!(!report.redacted_text.unwrap().contains("short123"));
    }
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<SecurityEvent>>);

impl SecurityEventSink for RecordingSink {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[test]
fn single_quoted_shell_reference_shapes_are_literal_credentials() {
    let scanner = PiiScanner::new().unwrap();
    let rules = Arc::new(PiiRuleSet::builtin().unwrap());
    let control = ExecutionControl {
        deadline: Instant::now() + Duration::from_secs(60),
        cancelled: false,
    };
    let caller = CallerIdentity {
        uid: 1201,
        gid: 1202,
        pid: 1203,
    };
    for value in [
        "$MY_LITERAL_SECRET",
        "${MY_LITERAL_SECRET}",
        "$(cat /run/secrets/alibaba)",
    ] {
        let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET='{value}'");
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Deny, "{text}");
        assert_eq!(report.findings.len(), 1, "{text}");
        let finding = &report.findings[0];
        assert_eq!(finding.pii_type, "aliyun_access_key_secret");
        assert_eq!(finding.severity, Severity::Deny);
        assert_eq!(finding.metadata["provider"], "alibaba_cloud");
        assert_eq!(finding.metadata["credential_kind"], "access_key_secret");
        assert_eq!(finding.evidence_redacted, "[REDACTED_SECRET]");
        assert_eq!(
            report.redacted_text.as_deref(),
            Some("ALIBABA_CLOUD_ACCESS_KEY_SECRET='[REDACTED_SECRET]'")
        );

        let sink = Arc::new(RecordingSink::default());
        let runtime = ActionRuntime::new(
            ActionId::PiiScan,
            PiiScanExecutor::new(rules.clone()),
            PiiAuditProjector,
            asc_action_runtime::testing::audit_finalizer(sink.clone()),
        );
        let request = PiiScanRequest {
            text,
            options: PiiScanOptions {
                raw_evidence: true,
                ..options()
            },
            agent_name: None,
        };
        let outcome = runtime.invoke(&control, &caller, &request).unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.data["verdict"], "deny");
        assert_eq!(outcome.data["findings"][0]["raw_evidence"], value);
        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        let findings = events[0].details["result"]["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["type"], "aliyun_access_key_secret");
        assert_eq!(findings[0]["metadata"]["provider"], "alibaba_cloud");
        assert_eq!(
            findings[0]["metadata"]["credential_kind"],
            "access_key_secret"
        );
        assert_eq!(findings[0]["evidence_redacted"], "[REDACTED_SECRET]");
        let serialized = serde_json::to_string(&events[0]).unwrap();
        assert!(!serialized.contains(value));
        assert!(!serialized.contains("raw_evidence"));
        assert!(!serialized.contains("redacted_text"));
    }
}

#[test]
fn expandable_references_and_single_quoted_placeholders_remain_filtered() {
    let scanner = PiiScanner::new().unwrap();
    let mut values = Vec::new();
    for reference in ["$MY_KEY", "${MY_KEY}", "$(cat /run/secrets/alibaba)"] {
        values.push(reference.to_owned());
        values.push(format!("\"{reference}\""));
    }
    for placeholder in [
        "'<your-access-key-secret>'",
        "'changeme'",
        "'your_access_key_secret'",
        "'xxxxxxxx'",
        "%MY_KEY%",
        "\"%MY_KEY%\"",
        "'%MY_KEY%'",
    ] {
        values.push(placeholder.to_owned());
    }
    for value in values {
        let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}");
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.verdict, Verdict::Pass, "{text}");
        assert!(report.findings.is_empty(), "{text}");
        assert_eq!(report.redacted_text.as_deref(), Some(text.as_str()));
    }
}

#[test]
fn exhausted_nested_budget_returns_failed_runtime_outcome() {
    let text = format!(
        "{}ALICLOUD_ACCESS_KEY_SECRET=short123{}",
        "ALICLOUD_ACCESS_KEY_ID=$(echo ".repeat(2_000),
        ")".repeat(2_000)
    );
    let request = PiiScanRequest {
        text,
        options: options(),
        agent_name: None,
    };
    let rules = Arc::new(PiiRuleSet::builtin().unwrap());
    let control = ExecutionControl {
        deadline: Instant::now() + Duration::from_secs(60),
        cancelled: false,
    };
    let executor = PiiScanExecutor::new(rules.clone());
    let direct = executor.execute(&control, &request);
    assert!(!direct.success);
    assert_eq!(direct.error_type, "matching_failed");
    assert_eq!(direct.data["verdict"], "error");
    assert_eq!(direct.data["summary"]["coverage"]["status"], "unavailable");
    assert!(direct.data["findings"].as_array().unwrap().is_empty());

    let sink = Arc::new(RecordingSink::default());
    let runtime = ActionRuntime::new(
        ActionId::PiiScan,
        PiiScanExecutor::new(rules),
        PiiAuditProjector,
        asc_action_runtime::testing::audit_finalizer(sink.clone()),
    );
    let caller = CallerIdentity {
        uid: 1201,
        gid: 1202,
        pid: 1203,
    };
    let outcome = runtime.invoke(&control, &caller, &request).unwrap();
    assert!(!outcome.success);
    assert_eq!(outcome.error_type, "matching_failed");
    assert_eq!(outcome.data["verdict"], "error");
    assert_eq!(outcome.data["summary"]["coverage"]["status"], "unavailable");
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].result, EventResult::Failed);
    assert_eq!(
        events[0].details["result"]["summary"]["error_type"],
        "matching_failed"
    );
    assert!(
        !serde_json::to_string(&events[0])
            .unwrap()
            .contains("short123")
    );
}

#[test]
fn access_key_ids_use_the_approved_partial_mask() {
    let scanner = PiiScanner::new().unwrap();
    for (text, expected_evidence, expected_text) in [
        (
            "ALIBABA_CLOUD_ACCESS_KEY_ID=abcdefghijklmnop1234",
            "abcd...[REDACTED]...1234",
            "ALIBABA_CLOUD_ACCESS_KEY_ID=abcd...[REDACTED]...1234",
        ),
        (
            "ALICLOUD_ACCESS_KEY=\"LTAI5tQnKxExampleToken12\"",
            "LTAI...[REDACTED]...en12",
            "ALICLOUD_ACCESS_KEY=\"LTAI...[REDACTED]...en12\"",
        ),
        (
            "ALICLOUD_ACCESS_KEY=abc12345",
            "[REDACTED_ACCESS_KEY_ID]",
            "ALICLOUD_ACCESS_KEY=[REDACTED_ACCESS_KEY_ID]",
        ),
    ] {
        let report = scanner.scan(text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert_eq!(report.findings.len(), 1, "{text}");
        let finding = &report.findings[0];
        assert_eq!(finding.pii_type, "aliyun_access_key_id");
        assert_eq!(finding.metadata["provider"], "alibaba_cloud");
        assert_eq!(finding.severity, Severity::Warn);
        assert_eq!(report.verdict, Verdict::Warn);
        assert_eq!(finding.evidence_redacted, expected_evidence);
        assert_eq!(report.redacted_text.as_deref(), Some(expected_text));
    }
}

#[test]
fn a_partial_placeholder_does_not_document_a_literal_secret() {
    let scanner = PiiScanner::new().unwrap();
    let literal = "literalSecret3478";
    let value = format!("<placeholder>{literal}<placeholder>");
    let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET=\"{value}\"");
    let report = scanner.scan(&text, &options()).unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.findings.len(), 1, "{text}");
    let finding = &report.findings[0];
    assert_eq!(finding.severity, Severity::Deny);
    assert_eq!(finding.evidence_redacted, "[REDACTED_SECRET]");
    let redacted = report.redacted_text.unwrap();
    assert!(
        !redacted.contains(literal),
        "the literal half of a partial placeholder must not survive: {redacted}"
    );

    // A single placeholder still documents the value, on either side of the
    // one-bracket boundary.
    for value in ["<placeholder>", "<secret>", "<your-api-key-here>"] {
        let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET=\"{value}\"");
        let report = scanner.scan(&text, &options()).unwrap();
        assert_eq!(report.summary.total, 0, "{text}");
    }
}

#[test]
fn a_default_expansion_redacts_the_complete_word() {
    let scanner = PiiScanner::new().unwrap();
    let literal = "literalSecret3478";
    let value = format!("${{UNSET_TEST_VAR:-prefix {literal}}}");
    let text = format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}");
    let report = scanner.scan(&text, &options()).unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.findings.len(), 1, "{text}");
    assert_eq!(report.findings[0].evidence_redacted, "[REDACTED_SECRET]");
    let redacted = report.redacted_text.unwrap();
    assert!(
        !redacted.contains(literal),
        "the default word of a parameter expansion must not survive: {redacted}"
    );

    // A bare expansion still carries no literal value.
    let text = "ALIBABA_CLOUD_ACCESS_KEY_SECRET=${UNSET_TEST_VAR}";
    let report = scanner.scan(text, &options()).unwrap();
    assert_eq!(report.summary.total, 0, "{text}");
}

#[test]
fn a_short_id_is_absent_from_every_masked_form() {
    let scanner = PiiScanner::new().unwrap();
    let id = "A7b9";
    let text = format!("password=ALICLOUD_ACCESS_KEY={id}");
    let options = PiiScanOptions {
        raw_evidence: false,
        redact_output: true,
        ..Default::default()
    };
    let report = scanner.scan(&text, &options).unwrap();
    assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
    assert_eq!(report.findings.len(), 2, "{text}");
    let id_finding = report
        .findings
        .iter()
        .find(|finding| finding.pii_type == "aliyun_access_key_id")
        .expect("the alias is still a positive case");
    assert_eq!(id_finding.severity, Severity::Warn);
    assert_eq!(id_finding.evidence_redacted, "[REDACTED_ACCESS_KEY_ID]");
    for finding in &report.findings {
        assert!(
            !finding.evidence_redacted.contains(id),
            "{} must not republish the short ID: {}",
            finding.pii_type,
            finding.evidence_redacted
        );
    }
    let redacted = report.redacted_text.unwrap();
    assert!(
        !redacted.contains(id),
        "the short ID must not survive redaction: {redacted}"
    );

    // The audit projection is a second serialization: masking the client
    // response is not enough if the emitted event republishes the raw value.
    let request = PiiScanRequest {
        text,
        options,
        agent_name: None,
    };
    let rules = Arc::new(PiiRuleSet::builtin().unwrap());
    let control = ExecutionControl {
        deadline: Instant::now() + Duration::from_secs(60),
        cancelled: false,
    };
    let sink = Arc::new(RecordingSink::default());
    let runtime = ActionRuntime::new(
        ActionId::PiiScan,
        PiiScanExecutor::new(rules),
        PiiAuditProjector,
        asc_action_runtime::testing::audit_finalizer(sink.clone()),
    );
    let caller = CallerIdentity {
        uid: 1201,
        gid: 1202,
        pid: 1203,
    };
    let outcome = runtime.invoke(&control, &caller, &request).unwrap();
    // The alias assignment is a warn, but the generic field that encloses it is
    // not, so the scan verdict escalates; what matters here is that neither the
    // findings nor the event republish the short ID.
    assert_eq!(outcome.data["verdict"], "deny");
    assert_eq!(
        outcome.data["findings"].as_array().unwrap().len(),
        2,
        "the alias assignment and the enclosing field are both findings"
    );
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1, "the invocation must emit one audit event");
    // The absence check below only means something if the event actually
    // projected the findings it decided on: an empty projected list would
    // satisfy it without saying anything about masking.
    let projected = events[0].details["result"]["findings"]
        .as_array()
        .expect("the event must project the findings it decided on");
    let projected_types: Vec<&str> = projected
        .iter()
        .filter_map(|finding| finding["type"].as_str())
        .collect();
    assert!(
        projected_types.contains(&"aliyun_access_key_id"),
        "the projection must keep the alias finding: {projected_types:?}"
    );
    assert!(
        projected_types.contains(&"generic_secret_field"),
        "the projection must keep the enclosing field: {projected_types:?}"
    );
    let serialized = serde_json::to_string(&events[0]).unwrap();
    assert!(
        !serialized.contains(id),
        "the audit event must not republish the short ID: {serialized}"
    );
}

#[test]
fn a_bare_access_key_value_is_not_a_finding() {
    // #2146 settled that an Alibaba Cloud credential is recognized through a
    // documented alias assignment, not through the value's own shape. Keeping a
    // bare positive elsewhere would have made every `LTAI…`-shaped token in a
    // log an `aliyun_access_key_id`, so assert the negative here.
    let scanner = PiiScanner::new().unwrap();
    for text in [
        "LTAI5tQnKxExampleToken12",
        "the request used LTAI5tQnKxExampleToken12 and finished",
    ] {
        let report = scanner.scan(text, &options()).unwrap();
        assert_eq!(report.summary.coverage.status, CoverageStatus::Complete);
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.metadata["provider"] != "alibaba_cloud"),
            "{text} produced {:?}",
            report.findings
        );
        assert_eq!(report.redacted_text.as_deref(), Some(text));
    }
}
