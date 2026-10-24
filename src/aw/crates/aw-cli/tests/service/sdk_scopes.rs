//! Private SDK CLI transport preserves task ownership and never converts failures to allow.

use super::*;
use aw_service::Binding;
use std::os::unix::fs::PermissionsExt;

fn callback(adapter: &str, after: bool, scenario: &str, cwd: &str) -> Value {
    let mut value = if adapter == "pi" {
        json!({"event":if after {"tool.after"} else {"tool.before"},
            "invocation_id":"8a3b4e83-aaba-40d1-af1a-cf062df04113", "session_id":"session",
            "tool_call":{"id":"call","name":"bash","input":{"command":"true"}},
            "tool_response":{"content":[{"type":"text","text":"original"}],"isError":false},
            "scenario":scenario})
    } else {
        json!({"hook_event_name":if after {"PostToolUse"} else {"PreToolUse"},
            "cwd":cwd,"session_id":"session","tool_use_id":"call","tool_name":"Bash",
            "tool_input":{"command":"true"},"tool_response":{"stdout":"original","stderr":"","interrupted":false,"isImage":false},
            "scenario":scenario})
    };
    if !after {
        value.as_object_mut().unwrap().remove("tool_response");
    }
    value
}

fn cli(
    binding: &std::path::Path,
    adapter: &str,
    action: &str,
    input: &Value,
    after: bool,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aw"));
    command.args([
        "scope",
        "--adapter",
        adapter,
        "--binding",
        binding.to_str().unwrap(),
        "--action",
        action,
    ]);
    if action == "invoke" {
        command.args([
            "--event",
            if after { "tool.after" } else { "tool.before" },
            "--step",
            if after { "record" } else { "check" },
            "--on-error",
            "report",
        ]);
    }
    let mut process = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(input).unwrap())
        .unwrap();
    process.wait_with_output().unwrap()
}

fn host(fixture: &Fixture, adapter: &str) -> (Running, Binding, std::path::PathBuf) {
    let mut document = fixture.document();
    document["spec"]["agents"]["target"]["adapter"] = json!(adapter);
    document["spec"]["events"]["tool.before"]["steps"][0]["on_error"] = json!("report");
    let service = Running::start(fixture, &document);
    let mut operation = fixture.bind_operation();
    if let Operation::Bind { capabilities, .. } = &mut operation {
        capabilities.adapter = adapter.into();
    }
    let parent: Binding = serde_json::from_value(
        service
            .client
            .call(operation, Instant::now() + TIMEOUT)
            .unwrap(),
    )
    .unwrap();
    service
        .client
        .call(
            Operation::OwnInstance {
                instance_id: parent.instance_id.clone(),
                pid: std::process::id(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    let binding = fixture.0.join("binding.json");
    fs::write(&binding, serde_json::to_vec(&json!({"adapter":adapter,"binding":parent,"socket":service.socket,"cwd":fixture.0,
            "events":{"tool.before":{"budget_ms":5000,"replacement_step":null,"steps":{"check":"report"}},
                "tool.after":{"budget_ms":5000,"replacement_step":null,"steps":{"record":"report"}}}})).unwrap()).unwrap();
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o600)).unwrap();
    (service, parent, binding)
}

#[test]
fn sdk_scope_cli_isolates_context_and_rejects_failures_and_forwarded_handles() {
    for adapter in ["pi", "claude-sdk"] {
        let fixture = Fixture::new();
        let (service, parent, binding) = host(&fixture, adapter);
        let task = fixture.0.join("task");
        fs::create_dir(&task).unwrap();
        for (scenario, after) in [
            ("allow", false),
            ("block", false),
            ("malformed", false),
            ("malformed", true),
        ] {
            let opened = cli(
                &binding,
                adapter,
                "open",
                &json!({"pid":std::process::id(),"cwd":task,
                "environment":{"AW_SERVICE_MARKER":"task-context"}}),
                false,
            );
            assert!(
                opened.status.success(),
                "{}",
                String::from_utf8_lossy(&opened.stderr)
            );
            let scope: Value = serde_json::from_slice(&opened.stdout).unwrap();
            assert!(cli(&binding, adapter, "acknowledge", &scope, false)
                .status
                .success());
            let native = callback(adapter, after, scenario, task.to_str().unwrap());
            let response = cli(
                &binding,
                adapter,
                "invoke",
                &json!({"scope":scope,"native_input":native.to_string()}),
                after,
            );
            if scenario == "malformed" {
                assert_eq!(
                    response.status.code(),
                    Some(1),
                    "resident callbacks must surface report-mode failures"
                );
            } else if scenario == "block" {
                if adapter == "pi" {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&response.stdout).unwrap()["block"],
                        true
                    );
                } else {
                    assert_eq!(response.status.code(), Some(2));
                }
            } else {
                assert!(
                    response.status.success(),
                    "{}: {}",
                    adapter,
                    String::from_utf8_lossy(&response.stderr)
                );
            }
            let mut forwarded = scope.clone();
            forwarded["pid"] = json!(std::process::id() + 1);
            assert!(!cli(&binding, adapter, "close", &forwarded, false)
                .status
                .success());
            assert!(cli(&binding, adapter, "close", &scope, false)
                .status
                .success());
            assert!(cli(&binding, adapter, "close", &scope, false)
                .status
                .success());
            assert!(!cli(&binding, adapter, "close", &forwarded, false)
                .status
                .success());
            let mut stale = scope.clone();
            stale["binding"]["identity"]["generation"] = json!("stale-generation");
            assert!(!cli(&binding, adapter, "close", &stale, false)
                .status
                .success());
            assert!(!cli(
                &binding,
                adapter,
                "invoke",
                &json!({"scope":scope,"native_input":native.to_string()}),
                after
            )
            .status
            .success());
        }
        assert_eq!(fixture.calls("invoke").len(), 4);
        for call in fixture.calls("invoke") {
            assert_eq!(call["cwd"], task.to_str().unwrap());
            assert_eq!(call["environment"]["AW_SERVICE_MARKER"], "task-context");
        }
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: parent.instance_id,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        let status = service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap();
        assert_eq!(status["bindings"], 0);
        assert_eq!(status["events"], 0);
        fixture.assert_reaped();
    }
}

#[test]
fn expired_sdk_handoff_can_close_without_reviving_callbacks() {
    for adapter in ["pi", "claude-sdk"] {
        let fixture = Fixture::new();
        let (service, parent, binding) = host(&fixture, adapter);
        let input = json!({"pid":std::process::id(), "cwd":fixture.0,
            "environment":{"AW_SERVICE_MARKER":"expired"}});
        let opened = cli(&binding, adapter, "open", &input, false);
        assert!(opened.status.success());
        let scope: Value = serde_json::from_slice(&opened.stdout).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while service.client.call(Operation::Status, deadline).unwrap()["bindings"] != 1 {
            assert!(
                Instant::now() < deadline,
                "expired handoff was not reclaimed"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!cli(&binding, adapter, "acknowledge", &scope, false)
            .status
            .success());
        for _ in 0..2 {
            assert!(cli(&binding, adapter, "close", &scope, false)
                .status
                .success());
        }
        let native = callback(adapter, false, "allow", fixture.0.to_str().unwrap());
        assert!(!cli(
            &binding,
            adapter,
            "invoke",
            &json!({"scope":scope,"native_input":native.to_string()}),
            false
        )
        .status
        .success());
        assert!(fixture.calls("invoke").is_empty());
        let opened = cli(&binding, adapter, "open", &input, false);
        assert!(opened.status.success());
        let replacement: Value = serde_json::from_slice(&opened.stdout).unwrap();
        assert!(cli(&binding, adapter, "acknowledge", &replacement, false)
            .status
            .success());
        assert!(cli(&binding, adapter, "close", &replacement, false)
            .status
            .success());
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: parent.instance_id,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        let status = service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap();
        assert_eq!(status["bindings"], 0);
        assert_eq!(status["events"], 0);
        drop(service);
        let offline = cli(&binding, adapter, "close", &scope, false);
        assert!(
            !offline.status.success(),
            "service loss was accepted as release"
        );
        fixture.assert_reaped();
    }
}
