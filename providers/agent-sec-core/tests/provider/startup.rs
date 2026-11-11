use super::*;
use std::{fs, os::unix::net::UnixListener, path::PathBuf, thread};

struct Backend(PathBuf);

impl Backend {
    fn new(directory: &support::Directory) -> Self {
        let path = std::env::temp_dir().join(format!(
            "aw-startup-{}",
            directory.0.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("s")
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn waiting(value: Value, directory: &support::Directory, backend: &Backend) -> Value {
    let mut output = Vec::new();
    let encoded = encode(value);
    aw_provider_sec_core::run_with_backend_wait(
        &mut Cursor::new(&encoded),
        &mut output,
        &directory.cli(),
        &backend.socket(),
        Duration::from_secs(2),
    )
    .unwrap();
    let request = Protocol::new().unwrap().parse_request(&encoded).unwrap();
    let checked = Protocol::new().unwrap().check_response(&request, &output);
    assert!(checked.is_ok() || matches!(checked, Err(aw_provider::Error::ProviderFailure { .. })));
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn delayed_backend_waits_before_scanning() {
    let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"deny"}}));
    let backend = Backend::new(&directory);
    thread::scope(|scope| {
        let pending = scope.spawn(|| waiting(invocation(), &directory, &backend));
        thread::sleep(Duration::from_millis(40));
        assert!(!directory.0.join("argv.json").exists());
        let _listener = UnixListener::bind(backend.socket()).unwrap();
        let reply = pending.join().unwrap();
        assert_eq!(reply["effects"][0]["type"], "block");
        let args = directory.argv();
        let remaining: u64 = args[5].as_str().unwrap().parse().unwrap();
        assert!(remaining < 1000);
    });
}

#[test]
fn missing_or_invalid_backend_returns_error_without_a_scan() {
    let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"pass"}}));
    let backend = Backend::new(&directory);
    let mut request = invocation();
    request["budget_ms"] = json!(25);
    let before = Instant::now();
    assert_eq!(
        waiting(request, &directory, &backend)["error_code"],
        "deadline_exceeded"
    );
    assert!(before.elapsed() < Duration::from_secs(1));
    fs::write(backend.socket(), "not a daemon").unwrap();
    assert_eq!(
        waiting(invocation(), &directory, &backend)["error_code"],
        "backend_socket_invalid"
    );
    assert!(!directory.0.join("argv.json").exists());
}

#[test]
fn waiting_keeps_discovery_validation_and_observation_offline() {
    let directory = support::Directory::new(json!({"stdout":{"ok":true,"verdict":"pass"}}));
    let backend = Backend::new(&directory);
    for request in [
        json!({"api_version":VERSION,"method":"describe","request_id":"d"}),
        json!({"api_version":VERSION,"method":"validate_config","request_id":"v","config":config()}),
    ] {
        assert_eq!(waiting(request, &directory, &backend)["status"], "ok");
    }
    let mut request = invocation();
    request["operation"] = json!("observe_tool");
    request["event"]["name"] = json!("tool.after");
    request["event"]["tool"]["result"] = json!({});
    request["allowed_effects"] = json!(["observe"]);
    assert_eq!(waiting(request, &directory, &backend)["status"], "ok");
    assert!(!directory.0.join("argv.json").exists());
}

#[test]
fn backend_free_invocations_ignore_missing_and_invalid_socket_paths() {
    let directory = support::Directory::new(json!({"exit":1}));
    let backend = Backend::new(&directory);
    let mut unmapped = invocation();
    unmapped["config"]["unmapped_tool"] = json!("observe");
    unmapped["event"]["tool"]["name"] = json!("unmapped");
    let mut plain = invocation();
    plain["operation"] = json!("check_skill");
    plain["config"]["skill"] = json!(true);
    plain["event"] = json!({"name":"input.submit", "input":{"text":"hello"},
        "agent":{"adapter":"pi","binding_id":"target","instance_id":"instance"},
        "session_id":"session","turn_id":"turn", "native":{"aw_skill":null,"aw_skill_request":null}});
    let mut empty = super::prompt::request();
    empty["event"]["input"]["text"] = json!(" \n\t\u{1c}");
    for invalid in [false, true] {
        if invalid {
            fs::write(backend.socket(), "not a daemon").unwrap();
        }
        for (request, reason) in [
            (&unmapped, "tool_unmapped"),
            (&plain, "skill_not_requested"),
            (&empty, "prompt_pass"),
        ] {
            let reply = waiting(request.clone(), &directory, &backend);
            assert_eq!(reply["status"], "ok");
            assert_eq!(
                reply["effects"],
                json!([{"type":"observe","reason_code":reason}])
            );
        }
    }
    assert!(!directory.0.join("argv.json").exists());
}

#[test]
fn invalid_inputs_are_rejected_before_waiting_for_backend() {
    let directory = support::Directory::new(json!({"exit":1}));
    let backend = Backend::new(&directory);
    fs::write(backend.socket(), "not a daemon").unwrap();
    let mut malformed = invocation();
    malformed["event"]["tool"]["input"]["command"] = json!({});
    assert_eq!(
        waiting(malformed, &directory, &backend)["error_code"],
        "invalid_tool_input"
    );
    assert!(!directory.0.join("argv.json").exists());
}

#[test]
fn a_bound_socket_does_not_turn_a_failed_scan_into_success() {
    let directory =
        support::Directory::new(json!({"exit":1,"stdout":{"ok":true,"verdict":"pass"}}));
    let backend = Backend::new(&directory);
    let _listener = UnixListener::bind(backend.socket()).unwrap();
    assert_eq!(
        waiting(invocation(), &directory, &backend)["error_code"],
        "cli_failed"
    );
}
