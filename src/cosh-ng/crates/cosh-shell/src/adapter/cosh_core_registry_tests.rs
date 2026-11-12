use serde_json::Value;

use super::cosh_core::CoshCoreAdapter;
use super::cosh_core_registry::{
    extension_mutation_requires_reload, registry_timeout, RegistryQueryError,
    AUTH_CONFIGURE_TIMEOUT, REGISTRY_MUTATION_TIMEOUT, REGISTRY_READ_TIMEOUT,
};

#[test]
fn candidate_building_mutations_share_the_mutation_timeout() {
    for action in [
        "enable",
        "disable",
        "select-source",
        "settings-set",
        "settings-unset",
        "commit",
        "update-all-commit",
        "uninstall",
        "recover",
        "reload",
    ] {
        assert!(extension_mutation_requires_reload("extensions", action));
        assert_eq!(
            registry_timeout("extensions", action),
            REGISTRY_MUTATION_TIMEOUT,
            "{action}"
        );
    }
    assert_eq!(
        registry_timeout("extensions", "list"),
        REGISTRY_READ_TIMEOUT
    );
    assert_eq!(
        registry_timeout("auth", "configure"),
        AUTH_CONFIGURE_TIMEOUT
    );
    assert!(AUTH_CONFIGURE_TIMEOUT > std::time::Duration::from_secs(8));
}

fn test_adapter_with_program(program: &str) -> CoshCoreAdapter {
    CoshCoreAdapter::new(program, false)
}

fn write_mock_script(label: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let script = std::env::temp_dir().join(format!(
        "cosh-registry-test-{label}-{}-{n}.sh",
        std::process::id()
    ));
    // Remove stale script if it exists (avoid ETXTBSY)
    let _ = std::fs::remove_file(&script);
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("write mock script");
    let mut permissions = std::fs::metadata(&script)
        .expect("mock script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod mock script");
    // Sync to avoid ETXTBSY race
    std::thread::sleep(std::time::Duration::from_millis(10));
    script
}

#[test]
fn registry_query_parses_success_response() {
    let script = write_mock_script(
        "success",
        r#"read REQUEST
printf '%s\n' '{"type":"registry_response","request_id":"reg-test","success":true,"data":["ext-a","ext-b"]}'
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query("extensions", "list", Value::Null);
    let _ = std::fs::remove_file(&script);

    let data = result.expect("should parse success response");
    assert!(data.is_array(), "data should be array: {data:?}");
    let arr = data.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0], "ext-a");
    assert_eq!(arr[1], "ext-b");
}

#[test]
fn startup_auth_state_keeps_environment_probing_off_and_honors_disabled_auto_access() {
    let script = write_mock_script(
        "startup-auth",
        r#"read REQUEST
case "$REQUEST" in
    *'"params":null'*) ;;
    *) exit 2 ;;
esac
printf '%s\n' '{"type":"registry_response","request_id":"startup-auth","success":true,"data":{"effective_auth_required":true,"trial_preferences":{"auto_use":false}}}'
"#,
    );
    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.startup_auth_state();
    let _ = std::fs::remove_file(&script);
    assert_eq!(result, Ok((false, super::StartupAuthHint::Configure)));
}

#[test]
fn startup_ecs_hint_uses_read_only_prepare_and_preserves_probe_errors() {
    use super::StartupAuthHint;
    for (response, expected) in [
        (
            r#"{"mode":"ecs_ram_role","instance_id":"i-test","console_url":"https://example.test"}"#,
            StartupAuthHint::Ecs,
        ),
        (r#"{"mode":"manual"}"#, StartupAuthHint::Unknown),
        (
            r#"{"error_code":"MetadataAccessDenied"}"#,
            StartupAuthHint::Unknown,
        ),
    ] {
        let success = expected == StartupAuthHint::Ecs || response.contains("manual");
        let body = format!(
            r#"read REQUEST
case "$REQUEST" in
    *'"action":"prepare"'*'"provider_type":"aliyun"'*) ;;
    *) exit 2 ;;
esac
printf '%s\n' '{{"type":"registry_response","request_id":"startup-ecs","success":{success},"data":{response},"error":null}}'
"#
        );
        let script = write_mock_script("startup-ecs", &body);
        let adapter = test_adapter_with_program(&script.to_string_lossy());
        let result = adapter.startup_ecs_hint();
        let _ = std::fs::remove_file(&script);
        assert_eq!(result, expected);
    }
}

#[test]
fn registry_query_returns_error_on_failure() {
    let script = write_mock_script(
        "failure",
        r#"read REQUEST
printf '%s\n' '{"type":"registry_response","request_id":"reg-test","success":false,"error":"extension not found"}'
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query(
        "extensions",
        "detail",
        serde_json::json!({"name": "nonexistent"}),
    );
    let _ = std::fs::remove_file(&script);

    let err = result.expect_err("should return error");
    assert!(
        err.contains("extension not found"),
        "unexpected error: {err}"
    );
}

#[test]
fn registry_query_classifies_failure_response() {
    let script = write_mock_script(
        "classified-failure",
        r#"read REQUEST
printf '%s\n' '{"type":"registry_response","request_id":"reg-test","success":false,"error":"candidate validation failed"}'
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query_classified(
        "extensions",
        "commit",
        serde_json::json!({"operation_id": "op-1"}),
    );
    let _ = std::fs::remove_file(&script);

    assert_eq!(
        result,
        Err(RegistryQueryError::Response {
            provider_updated: false,
            message: "candidate validation failed".to_string(),
            code: None,
        })
    );
}

#[test]
fn registry_query_preserves_structured_auth_error_code() {
    let script = write_mock_script(
        "classified-auth-failure",
        r#"read REQUEST
printf '%s\n' '{"type":"registry_response","request_id":"reg-test","success":false,"data":{"error_code":"invalid_credentials"},"error":"The API key was rejected."}'
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query_classified(
        "auth",
        "configure",
        serde_json::json!({"provider_id": "test"}),
    );
    let _ = std::fs::remove_file(&script);

    assert_eq!(
        result,
        Err(RegistryQueryError::Response {
            provider_updated: false,
            message: "The API key was rejected.".to_string(),
            code: Some("invalid_credentials".to_string()),
        })
    );
}

#[test]
fn registry_query_handles_spawn_failure() {
    let adapter = test_adapter_with_program("/nonexistent/path/to/cosh-core-fake-binary");
    let result = adapter.registry_query("extensions", "list", Value::Null);

    let err = result.expect_err("should fail on spawn");
    assert!(err.contains("failed to spawn"), "unexpected error: {err}");
}

#[test]
fn registry_query_handles_timeout() {
    let script = write_mock_script(
        "timeout",
        // Read request but never respond — just sleep longer than the 5s timeout
        r#"read REQUEST
sleep 60
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query("extensions", "list", Value::Null);
    let _ = std::fs::remove_file(&script);

    let err = result.expect_err("should timeout");
    assert!(
        err.contains("timed out") || err.contains("no response"),
        "unexpected error: {err}"
    );
}

#[test]
fn registry_query_handles_empty_stdout() {
    let script = write_mock_script(
        "empty",
        // Read request then exit immediately without writing anything
        r#"read REQUEST
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query("extensions", "list", Value::Null);
    let _ = std::fs::remove_file(&script);

    let err = result.expect_err("should fail on empty stdout");
    assert!(
        err.contains("no response") || err.contains("EOF"),
        "unexpected error: {err}"
    );
}

#[test]
fn registry_query_handles_invalid_json() {
    let script = write_mock_script(
        "invalid-json",
        r#"read REQUEST
printf '%s\n' 'this is not valid json'
"#,
    );

    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query("extensions", "list", Value::Null);
    let _ = std::fs::remove_file(&script);

    let err = result.expect_err("should fail on invalid json");
    assert!(err.contains("parse error"), "unexpected error: {err}");
}

#[test]
fn access_copy_short_registry_preserves_partial_save_result() {
    let script = write_mock_script(
        "partial-save",
        r#"read REQUEST
printf '%s\n' '{"type":"registry_response","request_id":"reg-test","success":false,"data":{"provider_updated":true,"error_code":"io_error"},"error":"session persistence failed"}'
"#,
    );
    let adapter = test_adapter_with_program(&script.to_string_lossy());
    let result = adapter.registry_query_classified("auth", "configure", Value::Null);
    let _ = std::fs::remove_file(&script);
    let error = format!("{:?}", result.unwrap_err());
    assert!(error.contains("provider_updated: true"), "{error}");
}
