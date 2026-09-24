//! Terminal lifecycle and privacy checks using the shared runtime and real sinks.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use asc_action_runtime::{
    ActionRuntime, AuditProjector, CapabilityExecutor, ExecutionControl, Invocation,
    SecurityEventSink,
};
use asc_action_types::{ActionId, CallerIdentity};
use asc_capability_pii_scan::{
    PiiAuditProjector, PiiRuleSet, PiiScanExecutor, PiiScanOptions, PiiScanRequest, Source,
};
use asc_event_sink::ConfiguredSecurityEventSinks;
use asc_observability::{Context, bind_trace_context_input};
use asc_persistence_sqlite::security_events::{EventFilters, SqliteEventReader};
use asc_security_events::{EventResult, SecurityEvent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TEXT: &str =
    "NEVER-PERSIST-TEXT alice@company.cn Authorization: Bearer abcdefghijklmnopqrstuvwx12345678";
const TOKEN: &str = "abcdefghijklmnopqrstuvwx12345678";

#[derive(Default)]
struct RecordingSink(Mutex<Vec<SecurityEvent>>);

impl SecurityEventSink for RecordingSink {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

fn request() -> PiiScanRequest {
    PiiScanRequest {
        text: TEXT.to_owned(),
        options: PiiScanOptions {
            source: Source::ToolOutput,
            raw_evidence: true,
            redact_output: true,
            ..Default::default()
        },
        agent_name: Some("fixture-agent".to_owned()),
    }
}

fn caller() -> CallerIdentity {
    CallerIdentity {
        uid: 1201,
        gid: 1202,
        pid: 1203,
    }
}

fn context() -> Context {
    bind_trace_context_input(
        &Context::new(),
        &json!({
            "agent_name": "fixture-agent", "trace_id": "fixture-trace",
            "session_id": "fixture-session", "run_id": "fixture-run",
            "call_id": "fixture-call", "tool_call_id": "fixture-tool",
        }),
    )
    .unwrap()
}

fn control() -> ExecutionControl {
    ExecutionControl {
        deadline: Instant::now(),
        cancelled: true,
    }
}

fn runtime(
    rules: Arc<PiiRuleSet>,
    sink: Arc<dyn SecurityEventSink>,
) -> ActionRuntime<PiiScanExecutor, PiiAuditProjector> {
    ActionRuntime::new(
        ActionId::PiiScan,
        PiiScanExecutor::new(rules),
        PiiAuditProjector,
        asc_action_runtime::testing::audit_finalizer(sink),
    )
}

fn assert_private(event: &SecurityEvent) {
    let serialized = serde_json::to_string(event).unwrap();
    for forbidden in [
        TEXT,
        TOKEN,
        "alice@company.cn",
        "NEVER-PERSIST-TEXT",
        "raw_evidence",
        "redacted_text",
    ] {
        assert!(!serialized.contains(forbidden), "persisted {forbidden}");
    }
    assert_eq!(event.event_type, "pii_scan");
    assert_eq!(event.uid, 1201);
    assert_eq!(event.pid, 1203);
    assert_eq!(event.trace_id, "fixture-trace");
    assert_eq!(event.tool_call_id.as_deref(), Some("fixture-tool"));
}

#[test]
fn each_normal_partial_and_failed_call_has_one_terminal_event() {
    let _guard = context().attach();
    let sink = Arc::new(RecordingSink::default());
    let rules = Arc::new(PiiRuleSet::builtin().unwrap());
    let runtime = runtime(rules, sink.clone());
    let request = request();
    let completed = runtime.invoke(&control(), &caller(), &request).unwrap();
    assert!(completed.success);
    assert_eq!(completed.exit_code, 0);
    assert_eq!(completed.data["verdict"], "deny");
    assert_eq!(completed.data["summary"]["scanner_version"], "2.0.0");
    assert_eq!(completed.data["summary"]["coverage"]["status"], "complete");
    assert!(
        completed.data["redacted_text"]
            .as_str()
            .unwrap()
            .contains("NEVER-PERSIST-TEXT")
    );
    assert!(
        completed.data["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f.get("raw_evidence").is_some())
    );

    let mut partial = request.clone();
    partial.options.input_truncated = true;
    assert_eq!(
        runtime
            .invoke(&control(), &caller(), &partial)
            .unwrap()
            .data["summary"]["coverage"]["status"],
        "partial"
    );

    let mut invalid = request.clone();
    invalid.options.max_bytes = Some(0);
    let failed = runtime.invoke(&control(), &caller(), &invalid).unwrap();
    assert!(!failed.success);
    assert_eq!(failed.exit_code, 1);
    assert_eq!(failed.data["verdict"], "error");
    assert_eq!(failed.data["summary"]["scanner_version"], "2.0.0");
    assert_eq!(failed.data["summary"]["coverage"]["status"], "unavailable");
    assert_eq!(failed.data["summary"]["scanned_bytes"], 0);
    assert_eq!(
        failed.data["summary"]["input_sha256"],
        format!("{:x}", Sha256::digest(TEXT))
    );
    assert!(
        !failed.data["summary"]["ruleset_id"]
            .as_str()
            .unwrap()
            .is_empty()
    );

    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 3);
    for record in &records[..3] {
        assert_eq!(
            record.details["result"]["summary"]["scanner_version"],
            "2.0.0"
        );
    }
    for record in &*records {
        assert_private(record);
    }
    assert_eq!(
        records.iter().map(|e| e.result).collect::<Vec<_>>(),
        [
            EventResult::Succeeded,
            EventResult::Succeeded,
            EventResult::Failed,
        ]
    );
    assert_eq!(
        records[0].details["request"]["text_length"],
        TEXT.chars().count()
    );
    assert_eq!(records[0].details["request"]["agent_name"], "fixture-agent");
}

#[test]
fn invalid_rule_content_is_absent_from_partial_audit() {
    let _guard = context().attach();
    let rules = Arc::new(
        PiiRuleSet::from_yaml(b"- type: marker\n  regex: '[DO-NOT-PERSIST-RULE'\n").unwrap(),
    );
    let sink = Arc::new(RecordingSink::default());
    let outcome = runtime(rules, sink.clone())
        .invoke(&control(), &caller(), &request())
        .unwrap();
    assert!(outcome.success);
    assert_eq!(outcome.data["summary"]["coverage"]["status"], "partial");
    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert_private(&records[0]);
    assert!(
        !serde_json::to_string(&records[0])
            .unwrap()
            .contains("DO-NOT-PERSIST-RULE")
    );
    assert_eq!(
        records[0].details["result"]["summary"]["custom_rules"]["error_code"],
        "invalid_regex"
    );
}

#[test]
fn projector_drops_unknown_fields_and_untrusted_error_details() {
    let executor = PiiScanExecutor::new(Arc::new(PiiRuleSet::builtin().unwrap()));
    let request = request();
    let mut outcome = executor.execute(&control(), &request);
    let secret = "DO-NOT-PERSIST-EXCEPTION";
    outcome.success = false;
    outcome.error = Some(secret.to_owned());
    outcome.error_type = secret.to_owned();
    outcome.data.insert("unknown".to_owned(), json!(secret));
    outcome.data.get_mut("summary").unwrap()["unknown"] = json!(secret);
    outcome.data.get_mut("summary").unwrap()["error"] = json!(secret);
    outcome.data.get_mut("summary").unwrap()["error_type"] = json!(secret);
    outcome.data.get_mut("findings").unwrap()[0]["metadata"]["unknown"] = json!(secret);
    outcome.data.get_mut("findings").unwrap()[0]["metadata"]["engine"] = json!(secret);
    let details = PiiAuditProjector.project(&request, &outcome).into_details();
    let serialized = serde_json::to_string(&details).unwrap();
    assert!(!serialized.contains(secret));
    assert!(!serialized.contains("unknown"));
    assert!(!serialized.contains(TOKEN));
    assert_eq!(details["error_type"], "scan_failed");
    outcome.data.clear();
    let invalid = PiiAuditProjector.project(&request, &outcome).into_details();
    assert_eq!(invalid["error_type"], "invalid_outcome");
    assert!(!serde_json::to_string(&invalid).unwrap().contains(secret));
}

struct DurableSink(ConfiguredSecurityEventSinks);

impl SecurityEventSink for DurableSink {
    fn write(&self, event: &SecurityEvent) {
        self.0.log_event(event);
    }
}

#[test]
fn real_sinks_persist_private_events_and_fail_independently_of_scanning() {
    let _guard = context().attach();
    let rules = Arc::new(PiiRuleSet::builtin().unwrap());
    for (fail_jsonl, fail_sqlite) in [(false, false), (true, false), (false, true), (true, true)] {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("blocked");
        fs::write(&blocked, "not a directory").unwrap();
        let jsonl = if fail_jsonl {
            blocked.join("events.jsonl")
        } else {
            directory.path().join("events.jsonl")
        };
        let sqlite = if fail_sqlite {
            blocked.join("events.db")
        } else {
            directory.path().join("events.db")
        };
        let sink = Arc::new(DurableSink(ConfiguredSecurityEventSinks::new(
            jsonl.clone(),
            sqlite.clone(),
        )));
        assert_eq!(sink.0.warm_jsonl().is_err(), fail_jsonl);
        assert_eq!(sink.0.warm_sqlite().is_err(), fail_sqlite);
        let outcome = runtime(Arc::clone(&rules), sink.clone())
            .invoke(&control(), &caller(), &request())
            .unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.data["verdict"], "deny");
        sink.0.close();
        let mut persisted = Vec::new();
        if !fail_jsonl {
            let records: Vec<SecurityEvent> = fs::read_to_string(jsonl)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(records.len(), 1);
            persisted.extend(records);
        }
        if !fail_sqlite {
            let reader = SqliteEventReader::new(&sqlite).unwrap();
            let records = reader.query(&EventFilters::default(), 10, 0);
            assert_eq!(records.len(), 1);
            persisted.extend(records);
            reader.close();
        }
        for event in &persisted {
            assert_private(event);
        }
        if persisted.len() == 2 {
            assert_eq!(persisted[0], persisted[1]);
        }
        assert!(
            outcome.data["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| finding.get("raw_evidence").and_then(Value::as_str) == Some(TOKEN))
        );
    }
}

#[test]
fn bounded_report_keeps_tail_deny_totals_full_redaction_and_one_terminal_event() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = runtime(Arc::new(PiiRuleSet::builtin().unwrap()), sink.clone());
    let request = PiiScanRequest {
        text: format!("{}\npassword=abcdefghijklmnop", "a@b.co ".repeat(20_000)),
        options: PiiScanOptions {
            redact_output: true,
            ..Default::default()
        },
        agent_name: None,
    };
    let result = runtime.invoke(&control(), &caller(), &request).unwrap();
    assert!(result.success);
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.data["verdict"], "deny");
    let summary = &result.data["summary"];
    assert_eq!(summary["total"], 20_001);
    assert_eq!(summary["by_severity"], json!({"deny": 1, "warn": 20_000}));
    assert_eq!(summary["findings_truncated"], true);
    assert_eq!(summary["coverage"]["status"], "complete");
    assert_eq!(summary["truncated"], false);
    assert!(summary.get("redacted_text_omitted").is_none());
    let findings = result.data["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2);
    assert_eq!(findings.last().unwrap()["severity"], "deny");
    let redacted = result.data["redacted_text"].as_str().unwrap();
    assert_eq!(redacted.matches("a***@b.co").count(), 20_000);
    assert!(!redacted.contains("abcdefghijklmnop"));
    assert!(serde_json::to_vec_pretty(&result.data).unwrap().len() <= 512 * 1024);
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].details["result"]["summary"], *summary);
    let audit = serde_json::to_string(&events[0]).unwrap();
    assert!(!audit.contains("abcdefghijklmnop"));
    assert!(!audit.contains("\"redacted_text\""));
}

#[test]
fn oversized_single_raw_finding_retains_its_classification_without_raw_evidence() {
    let rules = PiiRuleSet::from_yaml(
        b"- type: large_secret\n  regex: 'BEGIN[A-Z]+END'\n  severity: deny\n",
    )
    .unwrap();
    let request = PiiScanRequest {
        text: format!("BEGIN{}END", "Q".repeat(600 * 1024)),
        options: PiiScanOptions {
            raw_evidence: true,
            ..Default::default()
        },
        agent_name: None,
    };
    let outcome = PiiScanExecutor::new(Arc::new(rules)).execute(&control(), &request);
    assert_eq!(outcome.data["verdict"], "deny");
    assert_eq!(outcome.data["summary"]["total"], 1);
    assert_eq!(outcome.data["summary"]["findings_truncated"], true);
    let finding = &outcome.data["findings"][0];
    assert_eq!(finding["severity"], "deny");
    assert_eq!(finding["metadata"]["evidence_omitted"], true);
    assert!(finding.get("raw_evidence").is_none());
    assert!(serde_json::to_vec_pretty(&outcome.data).unwrap().len() <= 512 * 1024);
}

#[test]
fn escaped_redacted_output_is_omitted_without_claiming_partial_scanning() {
    let sink = Arc::new(RecordingSink::default());
    let runtime = runtime(Arc::new(PiiRuleSet::builtin().unwrap()), sink.clone());
    let request = PiiScanRequest {
        // JSON escaping expands this 90 KB input beyond the output budget.
        text: "\0".repeat(90_000),
        options: PiiScanOptions {
            redact_output: true,
            ..Default::default()
        },
        agent_name: None,
    };
    let outcome = runtime.invoke(&control(), &caller(), &request).unwrap();
    assert!(outcome.success);
    assert_eq!(outcome.data["verdict"], "pass");
    assert_eq!(outcome.data["summary"]["coverage"]["status"], "complete");
    assert_eq!(outcome.data["summary"]["scanned_bytes"], 90_000);
    assert_eq!(outcome.data["summary"]["redacted_text_omitted"], true);
    assert!(outcome.data["summary"].get("findings_truncated").is_none());
    assert_eq!(
        outcome.data["redacted_text"],
        "[REDACTED: output size limit]"
    );
    assert!(outcome.data["findings"].as_array().unwrap().is_empty());
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].details["result"]["summary"]["redacted_text_omitted"],
        true
    );
    assert!(events[0].details["result"].get("redacted_text").is_none());
}
