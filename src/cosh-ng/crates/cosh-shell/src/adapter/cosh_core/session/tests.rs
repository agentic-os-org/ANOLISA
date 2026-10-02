use super::*;
use std::thread;

#[cfg(target_os = "linux")]
#[test]
fn session_control_spawn_retries_transient_text_file_busy() {
    use std::os::unix::fs::PermissionsExt;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-control-retry-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&temp).expect("session-control tempdir");
    let program = temp.join("cosh-core");
    std::fs::write(
        &program,
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[],"next_cursor":null}}'
"#,
    )
    .expect("write session-control mock");
    let mut permissions = std::fs::metadata(&program)
        .expect("session-control mock metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&program, permissions).expect("chmod session-control mock");
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&program)
        .expect("hold executable open for writing");
    let release_writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        drop(writer);
    });

    let list = SessionManagementClient::new(program.to_string_lossy())
        .list("/tmp", 1, None, false)
        .expect("retry session-control spawn");
    release_writer.join().expect("release executable writer");
    std::fs::remove_dir_all(&temp).expect("remove session-control tempdir");

    assert!(list.sessions.is_empty());
}

#[test]
fn parses_every_core_health_and_error_code() {
    for health in ["ready", "corrupt", "incompatible", "scope_mismatch"] {
        let value = json!({
            "session_id": "00000000-0000-4000-8000-000000000000",
            "workspace_scope": "/tmp",
            "created_at_ms": 1,
            "updated_at_ms": 2,
            "model": null,
            "message_count": 0,
            "first_prompt": null,
            "schema_version": 1,
            "health": health
        });
        let parsed: SessionSummary = serde_json::from_value(value).expect("health");
        assert_eq!(parsed.health.label(), health);
    }

    for code in [
        "invalid_id",
        "invalid_cursor",
        "invalid_request",
        "not_found",
        "io",
        "corrupt",
        "incompatible_version",
        "scope_mismatch",
        "conflict",
        "active_session",
    ] {
        let value = json!({
            "code": code,
            "message": "message",
            "recoverable": true,
            "hint": null
        });
        let parsed: SessionErrorInfo = serde_json::from_value(value).expect("error");
        assert_eq!(parsed.code, code);
    }
}
