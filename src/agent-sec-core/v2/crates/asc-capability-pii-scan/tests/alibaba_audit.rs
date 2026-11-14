//! Provider attribution and privacy at the PII audit boundary.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use asc_action_runtime::{
    ActionRuntime, AuditProjector, CapabilityExecutor, ExecutionControl, Invocation,
    SecurityEventSink,
};
use asc_action_types::{ActionId, CallerIdentity};
use asc_capability_pii_scan::{
    PiiAuditProjector, PiiRuleSet, PiiScanExecutor, PiiScanOptions, PiiScanRequest,
};
use asc_security_events::SecurityEvent;
use serde_json::{Value, json};

const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.e30.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
const SECRET: &str = "SecretCredentialValue23456789";

#[derive(Default)]
struct RecordingSink(Mutex<Vec<SecurityEvent>>);

impl SecurityEventSink for RecordingSink {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

fn request(text: String) -> PiiScanRequest {
    PiiScanRequest {
        text,
        options: PiiScanOptions {
            raw_evidence: true,
            redact_output: true,
            ..Default::default()
        },
        agent_name: None,
    }
}

fn control() -> ExecutionControl {
    ExecutionControl {
        deadline: Instant::now() + Duration::from_secs(60),
        cancelled: false,
    }
}

fn executor() -> PiiScanExecutor {
    PiiScanExecutor::new(Arc::new(PiiRuleSet::builtin().unwrap()))
}

#[test]
fn real_events_retain_only_approved_provider_metadata() {
    for (name, kind, credential_kind, evidence) in [
        (
            "ALIBABA_CLOUD_ACCESS_KEY_ID",
            "aliyun_access_key_id",
            "access_key_id",
            "Secr...[REDACTED]...6789",
        ),
        (
            "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
            "aliyun_access_key_secret",
            "access_key_secret",
            "[REDACTED_SECRET]",
        ),
        (
            "ALIBABA_CLOUD_SECURITY_TOKEN",
            "generic_secret_field",
            "security_token",
            "[REDACTED_TOKEN]",
        ),
        (
            "ALIBABA_CLOUD_BEARER_TOKEN",
            "bearer_token",
            "bearer_token",
            "[REDACTED_TOKEN]",
        ),
        (
            "DASHSCOPE_API_KEY",
            "api_key",
            "model_studio_api_key",
            "[REDACTED_API_KEY]",
        ),
    ] {
        let sink = Arc::new(RecordingSink::default());
        let runtime = ActionRuntime::new(
            ActionId::PiiScan,
            executor(),
            PiiAuditProjector,
            asc_action_runtime::testing::audit_finalizer(sink.clone()),
        );
        let request = request(format!("{name}={SECRET}"));
        let caller = CallerIdentity {
            uid: 1201,
            gid: 1202,
            pid: 1203,
        };
        let outcome = runtime.invoke(&control(), &caller, &request).unwrap();
        assert!(outcome.success, "{name}");
        let client_findings = outcome.data["findings"].as_array().unwrap();
        assert!(client_findings.iter().any(|f| f["raw_evidence"] == SECRET));
        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 1, "{name}");
        let event = &records[0];
        let serialized = serde_json::to_string(event).unwrap();
        for forbidden in [SECRET, name, "raw_evidence", "redacted_text"] {
            assert!(
                !serialized.contains(forbidden),
                "{name}: persisted {forbidden}"
            );
        }
        let findings = event.details["result"]["findings"].as_array().unwrap();
        let provider: Vec<_> = findings
            .iter()
            .filter(|f| f["metadata"]["provider"] == "alibaba_cloud")
            .collect();
        assert_eq!(provider.len(), 1, "{name}");
        assert_eq!(provider[0]["type"], kind, "{name}");
        assert_eq!(provider[0]["metadata"]["credential_kind"], credential_kind);
        assert_eq!(provider[0]["evidence_redacted"], evidence);
    }
}

#[test]
fn projector_rejects_mismatched_or_untrusted_provider_metadata() {
    let request = request(format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={SECRET}"));
    let original = executor().execute(&control(), &request);
    assert!(original.success);
    let types = [
        "aliyun_access_key_id",
        "aliyun_access_key_secret",
        "generic_secret_field",
        "bearer_token",
        "api_key",
    ];
    let kinds = [
        "access_key_id",
        "access_key_secret",
        "security_token",
        "bearer_token",
        "model_studio_api_key",
    ];
    for (type_index, pii_type) in types.iter().enumerate() {
        for (kind_index, kind) in kinds.iter().enumerate() {
            let mut outcome = original.clone();
            let finding = &mut outcome.data.get_mut("findings").unwrap()[0];
            finding["type"] = json!(pii_type);
            finding["metadata"]["provider"] = json!("alibaba_cloud");
            finding["metadata"]["credential_kind"] = json!(kind);
            finding["metadata"]["field"] = json!("PRIVATE-FIELD-SPELLING");
            let details = PiiAuditProjector.project(&request, &outcome).into_details();
            let metadata = &details["result"]["findings"][0]["metadata"];
            if type_index == kind_index {
                assert_eq!(metadata["provider"], "alibaba_cloud");
                assert_eq!(metadata["credential_kind"], *kind);
            } else {
                assert!(metadata.get("provider").is_none(), "{pii_type}: {kind}");
                assert!(
                    metadata.get("credential_kind").is_none(),
                    "{pii_type}: {kind}"
                );
            }
            let serialized = serde_json::to_string(&details).unwrap();
            for forbidden in [
                SECRET,
                "PRIVATE-FIELD-SPELLING",
                "raw_evidence",
                "redacted_text",
            ] {
                assert!(!serialized.contains(forbidden), "persisted {forbidden}");
            }
        }
    }

    for (pii_type, provider, kind) in [
        ("jwt", json!("alibaba_cloud"), json!("security_token")),
        (
            "api_key",
            json!("other_provider"),
            json!("model_studio_api_key"),
        ),
        ("api_key", json!("alibaba_cloud"), json!("unknown_kind")),
        ("api_key", json!(null), json!("model_studio_api_key")),
        ("api_key", json!("alibaba_cloud"), json!(42)),
    ] {
        let mut outcome = original.clone();
        let finding = &mut outcome.data.get_mut("findings").unwrap()[0];
        finding["type"] = json!(pii_type);
        finding["metadata"]["provider"] = provider;
        finding["metadata"]["credential_kind"] = kind;
        let details = PiiAuditProjector.project(&request, &outcome).into_details();
        let metadata = &details["result"]["findings"][0]["metadata"];
        assert!(metadata.get("provider").is_none(), "{pii_type}");
        assert!(metadata.get("credential_kind").is_none(), "{pii_type}");
    }
    for missing in ["provider", "credential_kind"] {
        let mut outcome = original.clone();
        outcome.data.get_mut("findings").unwrap()[0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove(missing);
        let details = PiiAuditProjector.project(&request, &outcome).into_details();
        let metadata = &details["result"]["findings"][0]["metadata"];
        assert!(metadata.get("provider").is_none(), "missing {missing}");
        assert!(
            metadata.get("credential_kind").is_none(),
            "missing {missing}"
        );
    }
}

#[test]
fn sts_jwt_overlap_keeps_both_audit_classifications_without_raw_token() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = ActionRuntime::new(
        ActionId::PiiScan,
        executor(),
        PiiAuditProjector,
        asc_action_runtime::testing::audit_finalizer(sink.clone()),
    );
    let request = request(format!("ALIBABA_CLOUD_SECURITY_TOKEN={JWT}"));
    let caller = CallerIdentity {
        uid: 1201,
        gid: 1202,
        pid: 1203,
    };
    let outcome = runtime.invoke(&control(), &caller, &request).unwrap();
    assert!(outcome.success);
    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 1);
    let serialized = serde_json::to_string(&records[0]).unwrap();
    for forbidden in [
        JWT,
        "ALIBABA_CLOUD_SECURITY_TOKEN",
        "raw_evidence",
        "redacted_text",
    ] {
        assert!(!serialized.contains(forbidden), "persisted {forbidden}");
    }
    let findings = records[0].details["result"]["findings"].as_array().unwrap();
    let provider = findings
        .iter()
        .find(|finding| finding["metadata"]["provider"] == "alibaba_cloud")
        .unwrap();
    assert_eq!(provider["type"], "generic_secret_field");
    assert_eq!(provider["metadata"]["credential_kind"], "security_token");
    assert_eq!(provider["evidence_redacted"], "[REDACTED_TOKEN]");
    let jwt = findings
        .iter()
        .find(|finding| finding["type"] == "jwt")
        .unwrap();
    // The independent JWT finding keeps its type without token fragments.
    assert_eq!(jwt["evidence_redacted"], "[REDACTED]");
    assert_eq!(jwt["metadata"].get("provider"), None::<&Value>);
    assert_eq!(jwt["metadata"].get("credential_kind"), None::<&Value>);
}

#[test]
fn overlapping_api_key_and_secret_are_fully_masked_in_result_and_event() {
    let value = "sk-abcdefghijklmnop123456";
    let sink = Arc::new(RecordingSink::default());
    let runtime = ActionRuntime::new(
        ActionId::PiiScan,
        executor(),
        PiiAuditProjector,
        asc_action_runtime::testing::audit_finalizer(sink.clone()),
    );
    let request = request(format!("ALIBABA_CLOUD_ACCESS_KEY_SECRET={value}"));
    let caller = CallerIdentity {
        uid: 1201,
        gid: 1202,
        pid: 1203,
    };
    let outcome = runtime.invoke(&control(), &caller, &request).unwrap();
    assert!(outcome.success);
    let client = outcome.data["findings"].as_array().unwrap();
    assert_eq!(client.len(), 2);
    assert_eq!(
        client
            .iter()
            .find(|f| f["type"] == "aliyun_access_key_secret")
            .unwrap()["evidence_redacted"],
        "[REDACTED_SECRET]"
    );
    assert_eq!(
        client.iter().find(|f| f["type"] == "api_key").unwrap()["evidence_redacted"],
        "[REDACTED]"
    );
    assert!(
        !outcome.data["redacted_text"]
            .as_str()
            .unwrap()
            .contains(value)
    );
    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 1);
    let audit = &records[0].details["result"]["findings"];
    assert!(
        audit
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["type"] == "api_key" && f["evidence_redacted"] == "[REDACTED]")
    );
    let event = serde_json::to_string(&records[0]).unwrap();
    for secret in [value, "sk-a", "3456", "raw_evidence", "redacted_text"] {
        assert!(!event.contains(secret), "persisted {secret}");
    }
}

#[test]
fn overlapping_api_key_and_provider_token_use_whole_text_mask() {
    let token = "sk-abcdefghijklmnop1234";
    for (name, token_type, token_mask) in [
        (
            "ALICLOUD_SECURITY_TOKEN",
            "generic_secret_field",
            "[REDACTED_TOKEN]",
        ),
        (
            "ALIBABA_CLOUD_BEARER_TOKEN",
            "bearer_token",
            "[REDACTED_TOKEN]",
        ),
    ] {
        let sink = Arc::new(RecordingSink::default());
        let runtime = ActionRuntime::new(
            ActionId::PiiScan,
            executor(),
            PiiAuditProjector,
            asc_action_runtime::testing::audit_finalizer(sink.clone()),
        );
        let request = request(format!("{name}={token}"));
        let caller = CallerIdentity {
            uid: 1201,
            gid: 1202,
            pid: 1203,
        };
        let outcome = runtime.invoke(&control(), &caller, &request).unwrap();
        assert!(outcome.success);
        assert_eq!(
            outcome.data["redacted_text"],
            format!("{name}={token_mask}")
        );
        let findings = outcome.data["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .any(|f| f["type"] == token_type && f["evidence_redacted"] == token_mask)
        );
        assert!(
            findings
                .iter()
                .any(|f| f["type"] == "api_key" && f["evidence_redacted"] == "[REDACTED]")
        );
        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        let findings = records[0].details["result"]["findings"].as_array().unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f["type"] == token_type && f["evidence_redacted"] == token_mask)
        );
        assert!(
            findings
                .iter()
                .any(|f| f["type"] == "api_key" && f["evidence_redacted"] == "[REDACTED]")
        );
        let serialized = serde_json::to_string(&records[0]).unwrap();
        for forbidden in [token, "sk-a", "1234", "raw_evidence", "redacted_text"] {
            assert!(!serialized.contains(forbidden), "persisted {forbidden}");
        }
    }
}

#[test]
fn jwt_without_provider_overlap_keeps_its_original_partial_evidence() {
    let report = asc_capability_pii_scan::PiiScanner::new()
        .unwrap()
        .scan(JWT, &PiiScanOptions::default())
        .unwrap();
    let jwt = report
        .findings
        .iter()
        .find(|f| f.pii_type == "jwt")
        .unwrap();
    assert_eq!(jwt.evidence_redacted, "eyJh...[REDACTED]...sw5c");
}

#[test]
fn nested_api_key_cannot_expose_part_of_provider_token() {
    let input = "ALICLOUD_SECURITY_TOKEN=prefix!sk-abcdefghijklmnop1234";
    let report = asc_capability_pii_scan::PiiScanner::new()
        .unwrap()
        .scan(
            input,
            &PiiScanOptions {
                redact_output: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.pii_type == "generic_secret_field")
    );
    assert!(report.findings.iter().any(|f| f.pii_type == "api_key"));
    let text = report.redacted_text.unwrap();
    assert!(!text.contains("prefix!"));
    assert!(!text.contains("sk-a"));
    assert!(!text.contains("1234"));
}

#[test]
fn composite_id_hides_overlapping_generic_evidence() {
    let report = asc_capability_pii_scan::PiiScanner::new()
        .unwrap()
        .scan(
            "ALICLOUD_ACCESS_KEY=\"x password=abcdefghijklmnop1234\"",
            &PiiScanOptions::default(),
        )
        .unwrap();
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.pii_type == "aliyun_access_key_id")
    );
    let secret = report
        .findings
        .iter()
        .find(|f| f.pii_type == "generic_secret_field")
        .unwrap();
    assert_eq!(secret.evidence_redacted, "[REDACTED]");
}

#[test]
fn earlier_enclosing_evidence_is_masked_but_adjacent_jwt_is_not() {
    let input = format!("password=ALIBABA_CLOUD_ACCESS_KEY_SECRET=sk-abcdefghijklmnop123456 {JWT}");
    let report = asc_capability_pii_scan::PiiScanner::new()
        .unwrap()
        .scan(&input, &PiiScanOptions::default())
        .unwrap();
    let generic = report
        .findings
        .iter()
        .find(|f| f.pii_type == "generic_secret_field")
        .unwrap();
    assert_eq!(generic.evidence_redacted, "[REDACTED]");
    let jwt = report
        .findings
        .iter()
        .find(|f| f.pii_type == "jwt")
        .unwrap();
    assert_eq!(jwt.evidence_redacted, "eyJh...[REDACTED]...sw5c");
}

#[test]
fn overlap_protection_survives_findings_compaction() {
    let input = format!(
        "{}password=ALIBABA_CLOUD_ACCESS_KEY_SECRET=sk-abcdefghijklmnop123456",
        "a@b.co ".repeat(20_000)
    );
    let report = asc_capability_pii_scan::PiiScanner::new()
        .unwrap()
        .scan(
            &input,
            &PiiScanOptions {
                raw_evidence: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(report.summary.findings_truncated);
    let generic = report
        .findings
        .iter()
        .find(|f| f.pii_type == "generic_secret_field")
        .unwrap();
    assert_eq!(generic.evidence_redacted, "[REDACTED]");
    let api_key = report
        .findings
        .iter()
        .find(|f| f.pii_type == "api_key")
        .unwrap();
    assert_eq!(api_key.evidence_redacted, "[REDACTED]");
}
