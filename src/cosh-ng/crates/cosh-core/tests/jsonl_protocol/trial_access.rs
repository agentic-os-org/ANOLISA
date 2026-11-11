use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::process::Stdio;

use serde_json::{json, Value};

fn write_preferences(home: &std::path::Path, document: &str) {
    let directory = home.join(".copilot-shell/cosh");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let path = directory.join("preferences.toml");
    std::fs::write(&path, document).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn exchange(trusted: bool, messages: &[Value]) -> Vec<Value> {
    let home = tempfile::tempdir().unwrap();
    exchange_at_home(trusted, messages, home.path())
}

fn exchange_at_home(trusted: bool, messages: &[Value], home: &std::path::Path) -> Vec<Value> {
    let mut command = super::common::cosh_core_command(home);
    command.args(["--headless", "--bare"]);
    if trusted {
        command.arg("--cosh-shell-transport");
    }
    for key in [
        "COSH_AI_PROVIDER",
        "COSH_MODEL",
        "DASHSCOPE_API_KEY",
        "OPENAI_API_KEY",
        "ALIBABA_CLOUD_ACCESS_KEY_ID",
        "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
        "ALIBABA_CLOUD_SECURITY_TOKEN",
        "COSH_SYSOM_ENDPOINT",
        "COSH_SYSOM_VPC_PROXY_HOST",
        "OPENAI_BASE_URL",
    ] {
        command.env_remove(key);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in messages {
            writeln!(stdin, "{message}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn user(allowed: bool) -> Value {
    json!({"type":"user", "message":{"role":"user","content":"retained question"},
        "ecs_trial_context":{"schema_version":1,"shell_session_id":"shell-one",
            "intent":"user_initiated","auto_use_allowed":allowed}})
}

fn assert_disabled(messages: &[Value], results: usize) {
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["error_code"] == "ModelNotConfigured")
            .count(),
        results,
        "{messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m["request"]["subtype"] == "auth_required"
                || m["type"] == "assistant"
                || m["type"] == "ecs_trial_progress"),
        "{messages:?}"
    );
}

#[test]
fn session_suppression_stops_default_access_without_auth_or_mock() {
    assert_disabled(
        &exchange(true, &[initialize(true), user(false), shutdown()]),
        1,
    );
}

#[test]
fn review5_configure_after_suppression_serves_the_next_question_without_restart() {
    use std::io::Read;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        while !stopped.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|n| n.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&bytes);
            paths.push(request.lines().next().unwrap().to_string());
            let (kind, body) = if request.starts_with("GET ") {
                ("application/json", r#"{"id":"own-model","object":"model"}"#)
            } else {
                ("text/event-stream", "data: {\"choices\":[{\"delta\":{\"content\":\"static provider answered\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n")
            };
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        paths
    });
    let messages = exchange(
        true,
        &[
            initialize(true),
            user(false),
            json!({"type":"registry_request","request_id":"configure-static","domain":"auth",
            "action":"configure","params":{"provider_id":"mine","provider_type":"openai_compat",
                "values":{"model":"own-model","base_url":endpoint,"api_key":"synthetic-key"}}}),
            user(false),
            shutdown(),
        ],
    );
    stop.store(true, Ordering::SeqCst);
    let paths = server.join().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["request_id"] == "configure-static" && m["success"] == true),
        "{messages:?}"
    );
    let results: Vec<_> = messages.iter().filter(|m| m["type"] == "result").collect();
    assert_eq!(results.len(), 2, "{messages:?}");
    assert_eq!(results[0]["error_code"], "ModelNotConfigured");
    assert_eq!(results[1]["is_error"], false, "{messages:?}");
    assert!(
        messages.iter().any(|m| m["type"] == "assistant"),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|m| m["type"] == "ecs_trial_progress"),
        "{messages:?}"
    );
    assert!(
        paths
            .iter()
            .any(|p| p.starts_with("POST /v1/chat/completions ")),
        "{paths:?}"
    );
}

#[test]
fn invalid_trial_context_does_not_authorize_default_access() {
    for (field, value) in [
        ("schema_version", json!(2)),
        ("shell_session_id", json!("")),
    ] {
        let mut request = user(true);
        request["ecs_trial_context"][field] = value;
        assert_disabled(&exchange(true, &[initialize(true), request, shutdown()]), 1);
    }
}

#[test]
fn capable_client_without_trial_context_does_not_open_auth() {
    let mut request = user(true);
    request.as_object_mut().unwrap().remove("ecs_trial_context");
    assert_disabled(&exchange(true, &[initialize(true), request, shutdown()]), 1);
}

#[test]
fn subsequent_user_rechecks_suppression_in_the_same_core() {
    assert_disabled(
        &exchange(
            true,
            &[initialize(true), user(false), user(false), shutdown()],
        ),
        2,
    );
}

#[test]
fn persistent_suppression_overrides_saved_identity_provider() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(home.path(), "version = 1\n[ecs_trial]\nauto_use = false\n");
    std::fs::write(home.path().join(".copilot-shell/config.toml"),
        "[ai]\nactive_provider = 'trial'\n[ai.providers.trial]\ntype = 'aliyun'\nauth_source = 'ecs_identity'\n").unwrap();
    assert_disabled(
        &exchange_at_home(
            true,
            &[initialize(true), user(true), shutdown()],
            home.path(),
        ),
        1,
    );
}

#[test]
fn explicit_other_provider_survives_trial_suppression() {
    let home = tempfile::tempdir().unwrap();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.path().join(".copilot-shell"))
        .unwrap();
    std::fs::write(home.path().join(".copilot-shell/config.toml"),
        "[ai]\nactive_provider = 'local'\n[ai.providers.local]\ntype = 'mock'\nmodel = 'mock-model'\n").unwrap();
    let messages = exchange_at_home(
        true,
        &[initialize(true), user(false), shutdown()],
        home.path(),
    );
    assert!(
        messages.iter().any(|m| m["type"] == "assistant"),
        "{messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m["request"]["subtype"] == "auth_required"),
        "{messages:?}"
    );
}

#[test]
fn unusable_preferences_report_a_safe_code_once() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(home.path(), "version = 99\n");
    let messages = exchange_at_home(
        true,
        &[initialize(true), user(true), user(true), shutdown()],
        home.path(),
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["error_code"] == "trial_preferences_version")
            .count(),
        2,
        "{messages:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["status"] == "trial_preferences_unavailable:trial_preferences_version")
            .count(),
        1,
        "{messages:?}"
    );
    assert!(!messages
        .iter()
        .any(|m| m["request"]["subtype"] == "auth_required" || m["type"] == "assistant"));
}

#[test]
fn unusable_preferences_do_not_change_an_explicit_other_provider() {
    let home = tempfile::tempdir().unwrap();
    write_preferences(home.path(), "version = 99\n");
    std::fs::write(home.path().join(".copilot-shell/config.toml"),
        "[ai]\nactive_provider = 'local'\n[ai.providers.local]\ntype = 'mock'\nmodel = 'mock-model'\n").unwrap();
    let messages = exchange_at_home(
        true,
        &[initialize(true), user(true), shutdown()],
        home.path(),
    );
    assert!(
        messages.iter().any(|m| m["type"] == "assistant"),
        "{messages:?}"
    );
    assert!(!messages
        .iter()
        .any(|m| m["error_code"] == "trial_preferences_version"));
}

fn initialize(capable: bool) -> Value {
    json!({
        "type": "control_request", "request_id": "trial-init",
        "request": {"subtype":"initialize", "protocol_version":1,
            "fire_session_start":false,
            "capabilities":{"can_handle_ecs_trial_v1":capable}}
    })
}

fn shutdown() -> Value {
    json!({"type":"control_request","request_id":"stop","request":{"subtype":"shutdown"}})
}

#[test]
fn trusted_initialization_does_not_request_credentials_or_start_a_turn() {
    let messages = exchange(true, &[initialize(true), shutdown()]);
    assert!(messages
        .iter()
        .any(|m| m["response"]["request_id"] == "trial-init"));
    assert!(!messages
        .iter()
        .any(|m| { m["request"]["subtype"] == "auth_required" || m["type"] == "assistant" }));
}

#[test]
fn trusted_initialization_announces_trial_capability() {
    let messages = exchange(true, &[initialize(true), shutdown()]);
    let response = messages
        .iter()
        .find(|m| m["response"]["request_id"] == "trial-init")
        .unwrap();
    assert_eq!(
        response["response"]["response"]["capabilities"]["can_handle_ecs_trial_v1"],
        true
    );
}

#[test]
fn trusted_user_before_initialize_is_rejected_without_a_mock_answer() {
    let messages = exchange(
        true,
        &[
            json!({"type":"user","message":{"role":"user","content":"hello"}}),
            shutdown(),
        ],
    );
    assert!(messages
        .iter()
        .any(|m| m["error_code"] == "InitializeRequired"));
    assert!(!messages.iter().any(|m| m["type"] == "assistant"));
}

#[test]
fn untrusted_headless_keeps_auth_first_without_trial_admission() {
    let messages = exchange(false, &[initialize(true), shutdown()]);
    assert_eq!(messages[0]["request"]["subtype"], "auth_required");
}

#[test]
fn legacy_trusted_capabilities_do_not_authorize_trial() {
    let messages = exchange(
        true,
        &[
            initialize(false),
            json!({"type":"user","message":{"role":"user","content":"hello"}}),
            shutdown(),
        ],
    );
    let init = messages
        .iter()
        .position(|m| m["response"]["request_id"] == "trial-init")
        .unwrap();
    let auth = messages
        .iter()
        .position(|m| m["request"]["subtype"] == "auth_required")
        .unwrap();
    assert!(init < auth);
    assert!(!messages.iter().any(|m| m["type"] == "assistant"));
}

#[test]
fn trial_fixed_auto_model_is_announced() {
    let messages = exchange(true, &[initialize(true), user(true), shutdown()]);
    assert!(
        messages.iter().any(
            |message| message["type"] == "system" && message["status"] == "model_switched:auto"
        ),
        "{messages:?}"
    );
}
