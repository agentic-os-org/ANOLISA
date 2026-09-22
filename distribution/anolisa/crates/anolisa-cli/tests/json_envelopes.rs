//! Subprocess coverage for the standard JSON response envelope.

use serde_json::Value;

mod common;

fn run_json(arguments: &[&str]) -> Value {
    let output = common::run(arguments);
    assert_eq!(
        Some(0),
        output.status.code(),
        "unexpected exit code; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be a JSON envelope: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout),
        )
    })
}

fn assert_success_envelope<'a>(value: &'a Value, command: &str) -> &'a Value {
    assert_eq!(value.get("ok"), Some(&Value::Bool(true)));
    assert_eq!(value.get("schema_version"), Some(&Value::from(1)));
    assert_eq!(value.get("command").and_then(Value::as_str), Some(command));
    assert!(value.get("error").is_none(), "success must not carry error");
    value.get("data").expect("success envelope must carry data")
}

#[test]
fn osbase_sandbox_list_json_uses_standard_envelope() {
    let value = run_json(&["--json", "osbase", "sandbox", "list"]);
    let data = assert_success_envelope(&value, "osbase sandbox list");

    assert!(data.get("scenarios").is_some_and(Value::is_array));
    assert!(
        value.get("scenarios").is_none(),
        "sandbox payload must be nested under data"
    );
}

#[test]
fn register_status_json_uses_standard_envelope() {
    let value = run_json(&["--json", "register", "status"]);
    let data = assert_success_envelope(&value, "register status");

    // These fields are stable business data, while their values depend on the
    // machine running the CLI and must not be pinned in an integration test.
    assert!(data.get("product_type").is_some_and(Value::is_string));
    assert!(data.get("consent_state").is_some_and(Value::is_string));
    assert!(data.get("upload_active").is_some_and(Value::is_boolean));
    assert!(
        value.get("product_type").is_none()
            && value.get("consent_state").is_none()
            && value.get("upload_active").is_none(),
        "registration payload must be nested under data"
    );
}
