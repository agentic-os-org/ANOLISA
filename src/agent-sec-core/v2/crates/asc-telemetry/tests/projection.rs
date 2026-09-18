//! Negative allowlist cases complement the V1 lifecycle golden fixtures.
use asc_telemetry::{ScanTelemetryInput, TelemetryRecord};
use serde_json::{Value, json};

#[test]
fn invalid_optional_values_and_unknown_fields_never_escape() {
    for verdict in [
        json!("future-verdict"),
        json!(false),
        json!(null),
        json!({"secret":"SENSITIVE"}),
    ] {
        for elapsed in [
            json!(-1),
            json!(true),
            json!("12"),
            json!({"secret":"SENSITIVE"}),
        ] {
            let result = json!({"verdict":verdict, "elapsed_ms":elapsed, "code":"SENSITIVE", "prompt":"SENSITIVE", "path":"SENSITIVE"});
            let record = TelemetryRecord::for_scan(&ScanTelemetryInput {
                event_type: "code_scan",
                category: "code_scan",
                succeeded: false,
                timestamp: "2026-09-16T00:00:00+00:00",
                result: result.as_object().unwrap(),
                error_type: "error contains SENSITIVE",
                exit_code: Some(1),
                agent_name: Some("SENSITIVE"),
            });
            let value = serde_json::to_value(record).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 7);
            assert_eq!(value["component.agent_name"], "");
            assert!(!value.to_string().contains("SENSITIVE"));
        }
    }
}

#[test]
fn scalar_error_grammar_and_optional_exit_code_match_v1() {
    for error_type in [
        "ScanError",
        "scan.Error_1",
        "",
        "1Error",
        "érror",
        "bad-type",
    ] {
        let result = json!({"elapsed_ms":0.5, "verdict":"error"});
        let record = TelemetryRecord::for_scan(&ScanTelemetryInput {
            event_type: "code_scan",
            category: "code_scan",
            succeeded: false,
            timestamp: "2026-09-16T00:00:00+00:00",
            result: result.as_object().unwrap(),
            error_type,
            exit_code: None,
            agent_name: Some(" openclaw "),
        });
        let value: Value = serde_json::to_value(record).unwrap();
        assert_eq!(
            value.get("seccore.error_type").is_some(),
            matches!(error_type, "ScanError" | "scan.Error_1")
        );
        assert!(value.get("seccore.exit_code").is_none());
        assert_eq!(value["seccore.elapsed_ms"], 0.5);
        assert_eq!(value["component.agent_name"], "openclaw");
    }
}
