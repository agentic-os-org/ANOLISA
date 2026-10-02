use super::extensions::render_extensions_command;
use crate::runtime::prelude::*;

fn zh_state() -> InlineState {
    InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    }
}

fn en_state() -> InlineState {
    InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    }
}

fn mock_core(
    body: &str,
) -> (
    std::sync::MutexGuard<'static, ()>,
    AdapterInstance,
    std::path::PathBuf,
) {
    use std::sync::{Mutex, MutexGuard};
    static EXECUTABLE_LOCK: Mutex<()> = Mutex::new(());
    let executable_guard: MutexGuard<'static, ()> = EXECUTABLE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let script = std::env::temp_dir().join(format!(
        "cosh-extension-slash-test-{}-{}.sh",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&script);
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    let adapter =
        crate::adapter::CoshCoreAdapter::new(script.to_string_lossy().into_owned(), false);
    (executable_guard, AdapterInstance::CoshCore(adapter), script)
}

#[test]
fn extensions_non_cosh_core_shows_unavailable_zh() {
    let adapter = AdapterInstance::Fake(crate::adapter::FakeAgentAdapter);
    let mut state = zh_state();
    let mut buf = Vec::new();
    render_extensions_command("", &adapter, &mut state, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "should contain degradation message: {output}"
    );
}

#[test]
fn extensions_non_cosh_core_shows_unavailable_en() {
    let adapter = AdapterInstance::Fake(crate::adapter::FakeAgentAdapter);
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command("", &adapter, &mut state, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("cosh-core backend"),
        "should contain English degradation message: {output}"
    );
}

#[test]
fn extensions_install_displays_explicit_consent_without_committing() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"install-preflight"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"op-1","action":"install","name":"demo","version":"1.0.0","source_identity":"/tmp/demo path","resolved_revision":null,"content_digest":"digest-1","capabilities":["demo/skill/example"],"capabilities_added":["demo/skill/example"],"capabilities_removed":[],"capability_fingerprint":"fp-1","consent_required":true,"expected_desired_state":"enabled","expected_effective_state":"unchanged","expected_activation":"next_session","risk_summary":{"execution":[],"instruction":["demo/skill/example"],"authorization":[],"credential":[],"filesystem":[]}}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command("install '/tmp/demo path'", &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("/extensions consent op-1"), "{output}");
    assert!(output.contains("demo/skill/example"), "{output}");
    assert!(output.contains("fp-1"), "{output}");
    assert!(output.contains("/tmp/demo path"), "{output}");
    assert!(output.contains("Risk categories"), "{output}");
}

#[test]
fn extensions_consent_reloads_operation_before_committing() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"operation"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"op-2","capability_fingerprint":"fp-2"}}'
    ;;
  *'"action":"commit"'*'"fingerprint":"fp-2"'*)
    printf '%s\n' '{"success":true,"data":{"action":"install","activation":"next_session"}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = zh_state();
    let mut buf = Vec::new();
    render_extensions_command("consent op-2", &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("已完成"), "{output}");
    assert!(output.contains("next_session"), "{output}");
}

#[test]
fn extensions_consent_queries_receipt_after_commit_transport_failure() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"operation"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"op-3","capability_fingerprint":"fp-3"}}'
    ;;
  *'"action":"commit"'*)
    exit 0
    ;;
  *'"action":"result"'*)
    printf '%s\n' '{"success":true,"data":{"action":"install","activation":"next_session"}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command("consent op-3", &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("completed"), "{output}");
    assert!(output.contains("next_session"), "{output}");
}

#[test]
fn extensions_consent_preserves_application_error_without_querying_receipt() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"operation"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"op-4","capability_fingerprint":"fp-4"}}'
    ;;
  *'"action":"commit"'*)
    printf '%s\n' '{"success":false,"error":"extension_candidate_validation_failed: required MCP server failed"}'
    ;;
  *'"action":"result"'*)
    printf '%s\n' '{"success":false,"error":"receipt query must not run"}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command("consent op-4", &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("extension_candidate_validation_failed"),
        "{output}"
    );
    assert!(output.contains("required MCP server failed"), "{output}");
    assert!(!output.contains("commit status is unknown"), "{output}");
    assert!(!output.contains("receipt query must not run"), "{output}");
}

#[test]
fn extensions_operation_queries_durable_result_after_commit() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"operation"'*)
    printf '%s\n' '{"success":false,"error":"extension_operation_not_found: operation is no longer pending"}'
    ;;
  *'"action":"result"'*)
    printf '%s\n' '{"success":true,"data":{"action":"install","activation":"next_session"}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command("operation op-complete", &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("operation install completed"), "{output}");
    assert!(
        !output.contains("extension_operation_not_found"),
        "{output}"
    );
}

#[test]
fn extensions_operation_renders_durable_update_all_checkpoint() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"operation"'*)
    printf '%s\n' '{"success":false,"error":"extension_operation_not_found: operation is no longer pending"}'
    ;;
  *'"action":"result"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"67d7ce25-e50e-4b72-b0f7-304cd066fd88","action":"update-all","status":"in_progress","items":[{"name":"demo","outcome":"updated"}],"summary":{"updated":1}}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command(
        "operation 67d7ce25-e50e-4b72-b0f7-304cd066fd88",
        &adapter,
        &mut state,
        &mut buf,
    )
    .unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("status=in_progress"), "{output}");
    assert!(output.contains("demo: updated"), "{output}");
}

#[test]
fn extensions_update_all_queries_batch_after_transport_failure() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"update-all-preflight"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"67d7ce25-e50e-4b72-b0f7-304cd066fd88","action":"update-all","status":"prepared"}}'
    ;;
  *'"action":"update-all-commit"'*)
    exit 0
    ;;
  *'"action":"result"'*)
    printf '%s\n' '{"success":true,"data":{"operation_id":"67d7ce25-e50e-4b72-b0f7-304cd066fd88","action":"update-all","status":"in_progress","items":[{"name":"demo","outcome":"updated"}],"summary":{"updated":1}}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_extensions_command("update --all", &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("67d7ce25-e50e-4b72-b0f7-304cd066fd88"),
        "{output}"
    );
    assert!(output.contains("in_progress"), "{output}");
    assert!(output.contains("demo: updated"), "{output}");
}

#[test]
fn extensions_settings_redacts_sensitive_registry_values() {
    let (_executable_guard, adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"settings-get"'*)
    printf '%s\n' '{"success":true,"data":{"key":"token","setting_type":"string","scope":"user","configured":true,"sensitive":true,"value":"registry-must-not-leak","display":"registry-must-not-leak","required":true}}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = zh_state();
    let mut buf = Vec::new();
    render_extensions_command(
        "settings get example.ops token",
        &adapter,
        &mut state,
        &mut buf,
    )
    .unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("[redacted]"), "{output}");
    assert!(!output.contains("registry-must-not-leak"), "{output}");
}
