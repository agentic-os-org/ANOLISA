//! Prompt transport and completeness are verified independently from risk policy.

use super::*;

pub(super) fn request() -> Value {
    let mut value = invocation();
    value["operation"] = json!("scan_prompt");
    value["config"]["prompt"] = json!({"mode":"block", "scan_mode":"fast"});
    value["event"] = json!({"name":"input.submit", "agent":{"adapter":"qoder-sdk", "binding_id":"target", "instance_id":"instance"},
        "session_id":"session", "turn_id":"turn", "input":{"text":"hello\nworld"}, "native":{}});
    value
}

fn result(verdict: &str) -> Value {
    json!({"schema_version":"1.0", "ok":verdict == "pass", "verdict":verdict,
        "input_truncated":false, "input_bytes_scanned":11, "degraded":false,
        "layers_failed":[], "layer_results":[{"layer":"rule_engine"}]})
}

#[test]
fn prompt_scans_whole_stdin_with_fast_mode_and_turn_trace() {
    let directory = support::Directory::new(json!({"stdout":result("pass")}));
    let reply = run(request(), &directory.cli());
    assert_eq!(reply["effects"][0]["reason_code"], "prompt_pass");
    assert_eq!(
        std::fs::read(directory.0.join("stdin.bin")).unwrap(),
        b"hello\nworld"
    );
    let args = directory.argv();
    let args = args.as_array().unwrap();
    let context: Value = serde_json::from_str(args[1].as_str().unwrap()).unwrap();
    assert_eq!(
        context,
        json!({"agent_name":"qoder-sdk", "session_id":"session", "run_id":"turn"})
    );
    assert_eq!(
        &args[6..],
        json!([
            "scan-prompt",
            "--mode",
            "fast",
            "--source",
            "user_input",
            "--format",
            "json"
        ])
        .as_array()
        .unwrap()
    );
    assert!(!args
        .iter()
        .any(|arg| arg.as_str().is_some_and(|arg| arg.contains("hello"))));
    directory.assert_stopped();
}

#[test]
fn prompt_risk_and_scan_failures_are_distinct() {
    for verdict in ["warn", "deny"] {
        let reply = cli_response(request(), json!({"stdout":result(verdict)}));
        assert_eq!(reply["status"], "ok");
        assert_eq!(reply["effects"][0]["type"], "block");
    }
    for case in [
        json!({"raw":""}),
        json!({"raw":"not JSON"}),
        json!({"stdout":result("pass"), "exit":1}),
        json!({"stdout":result("error")}),
        json!({"sleep":1, "stdout":result("pass")}),
    ] {
        let mut value = request();
        value["budget_ms"] = json!(150);
        let reply = cli_response(value, case);
        assert_eq!(reply["status"], "error");
        assert!(reply.get("effects").is_none());
    }
    for (key, invalid) in [
        ("input_truncated", json!(true)),
        ("input_bytes_scanned", json!(2)),
        ("degraded", json!(true)),
        ("layers_failed", json!(["rule_engine"])),
        ("layer_results", json!([])),
    ] {
        let mut output = result("pass");
        output[key] = invalid;
        assert_eq!(
            cli_response(request(), json!({"stdout":output}))["status"],
            "error"
        );
    }
    let mut value = request();
    value["config"]["prompt"]["mode"] = json!("observe");
    assert_eq!(
        cli_response(value, json!({"stdout":result("deny")}))["effects"][0]["type"],
        "observe"
    );
}

#[test]
fn prompt_ml_modes_are_rejected_at_configuration_admission() {
    for mode in ["standard", "strict", "multi_turn"] {
        let mut value = request();
        value["config"]["prompt"]["scan_mode"] = json!(mode);
        assert_eq!(
            run(value, Path::new("/must-not-run"))["error_code"],
            "invalid_config"
        );
    }
}

#[test]
fn input_only_configuration_does_not_require_tool_mappings() {
    let mut value = request();
    value["config"].as_object_mut().unwrap().remove("tools");
    assert_eq!(
        cli_response(value, json!({"stdout":result("pass")}))["status"],
        "ok"
    );
}
