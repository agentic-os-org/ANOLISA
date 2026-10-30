//! User PII reuses the common scan transport with submission policy and identity.

use super::*;

fn request() -> Value {
    let mut value = prompt::request();
    value["operation"] = json!("scan_user_input");
    value["config"] = json!({"version":1,"mode":"block",
        "pii":{"input_mode":"block","output_mode":"observe"}});
    value["event"]["input"]["text"] = json!("SENSITIVE\n用户输入");
    value
}

#[test]
fn user_pii_uses_shared_stdin_transport_and_submission_identity() {
    let directory = support::Directory::new(json!({"pii":true}));
    let reply = run(request(), &directory.cli());
    assert_eq!(reply["status"], "ok");
    assert_eq!(
        reply["effects"],
        json!([{"type":"block","reason_code":"pii_risk"}])
    );
    assert!(!reply.to_string().contains("SENSITIVE"));
    assert_eq!(
        std::fs::read_to_string(directory.0.join("stdin.txt")).unwrap(),
        "SENSITIVE\n用户输入"
    );
    let args = directory.argv();
    let context: Value = serde_json::from_str(args[1].as_str().unwrap()).unwrap();
    assert_eq!(
        context,
        json!({"agent_name":"qoder-sdk","session_id":"session","run_id":"turn"})
    );
    assert_eq!(
        &args.as_array().unwrap()[6..],
        json!([
            "scan-pii",
            "--stdin",
            "--format",
            "json",
            "--source",
            "user_input"
        ])
        .as_array()
        .unwrap()
    );
    directory.assert_stopped();
}

#[test]
fn user_pii_observes_risk_and_keeps_failures_separate() {
    let mut value = request();
    value["config"]["pii"]["input_mode"] = json!("observe");
    assert_eq!(
        cli_response(value, json!({"pii":true}))["effects"][0]["type"],
        "observe"
    );
    let mut value = request();
    value["event"]["input"]["text"] = json!("public input");
    assert_eq!(
        cli_response(value, json!({"pii":true}))["effects"][0]["reason_code"],
        "pii_pass"
    );
    let reply = cli_response(
        request(),
        json!({"raw":"scanner failed with private input"}),
    );
    assert_eq!(reply["status"], "error");
    assert!(reply.get("effects").is_none());
    assert!(!reply.to_string().contains("private input"));
    let mut value = request();
    value["allowed_effects"] = json!(["observe"]);
    assert_eq!(
        run(value, Path::new("/must-not-run"))["error_code"],
        "block_not_admitted"
    );
    let mut value = request();
    value["config"].as_object_mut().unwrap().remove("pii");
    value["config"]["prompt"] = json!({"mode":"block","scan_mode":"fast"});
    assert_eq!(
        run(value, Path::new("/must-not-run"))["error_code"],
        "pii_not_configured"
    );
}
