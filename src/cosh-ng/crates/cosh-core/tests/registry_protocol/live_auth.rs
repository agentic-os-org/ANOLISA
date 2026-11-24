use super::*;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{self, Receiver};

struct LiveCore {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl LiveCore {
    fn start(home: &Path) -> Self {
        std::fs::write(home.join(".copilot-shell/telemetry_disabled"), "").unwrap();
        let mut child = Command::new(binary_path())
            .args(["--headless", "--bare"])
            .current_dir(home)
            .env("HOME", home)
            .env(
                "COSH_TELEMETRY_DISABLED_PATH",
                home.join(".copilot-shell/telemetry_disabled"),
            )
            .env_remove("COSH_HOME")
            .env_remove("COSH_AI_PROVIDER")
            .env_remove("COSH_MODEL")
            .env_remove("OPENAI_API_KEY")
            .env_remove("DASHSCOPE_API_KEY")
            .env_remove("OPENAI_BASE_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(message) = serde_json::from_str(&line) {
                    if sender.send(message).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            child,
            stdin,
            messages,
            reader: Some(reader),
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&self, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + REGISTRY_TIMEOUT;
        loop {
            let message = self
                .messages
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("live core response before deadline");
            if matches(&message) {
                return message;
            }
        }
    }

    fn initialize(&mut self) {
        self.send(serde_json::json!({"type":"control_request","request_id":"init","request":{"subtype":"initialize"}}));
        self.receive(|message| {
            message["type"] == "control_response" && message["response"]["request_id"] == "init"
        });
    }

    fn registry(&mut self, action: &str, params: Value) -> Value {
        self.send(serde_json::json!({"type":"registry_request","request_id":"live-auth","domain":"auth","action":action,"params":params}));
        self.receive(|message| {
            message["type"] == "registry_response" && message["request_id"] == "live-auth"
        })
    }
}

impl Drop for LiveCore {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn seed(home: &Path, needs_auth: bool) -> std::path::PathBuf {
    let dir = home.join(".copilot-shell");
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let path = dir.join("config.toml");
    let provider_type = if needs_auth { "openai" } else { "mock" };
    std::fs::write(
        &path,
        format!(
            r#"[ai]
active_provider = "keepme"
active_model = "keep-model"
[ai.providers.keepme]
type = "{provider_type}"
model = "keep-model"
[ai.providers.removeme]
type = "mock"
api_key = "stale-secret"
model = "remove-model"
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn configure(home: &Path, provider_id: &str) {
    let (base_url, server) = model_server();
    let result = run_registry_request_with_context(
        "auth",
        "configure",
        serde_json::json!({
            "provider_id":provider_id,"provider_type":"openai_compat","guarded_writes":true,
            "values":{"base_url":base_url,"api_key":"new-secret","model":"home-model"}
        }),
        home,
        Some(home),
    );
    server.join().unwrap();
    assert_eq!(result["success"], true, "{result}");
}

fn stale_mutation_preserves_configure(action: &str) {
    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), false);
    let mut live = LiveCore::start(home.path());
    live.initialize();
    configure(home.path(), "new-provider");
    let provider_id = if action == "activate" {
        "keepme"
    } else {
        "removeme"
    };
    let result = live.registry(
        action,
        serde_json::json!({"provider_id":provider_id,"guarded_writes":true}),
    );
    assert_eq!(result["success"], true, "{result}");
    let saved: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(
        saved["ai"]["providers"].get("new-provider").is_some(),
        "{action}: {saved}"
    );
}

#[test]
fn concurrent_auth_stale_activate_preserves_successful_configure() {
    stale_mutation_preserves_configure("activate");
}

#[test]
fn concurrent_auth_stale_delete_preserves_successful_configure() {
    stale_mutation_preserves_configure("delete");
}

#[test]
fn concurrent_auth_delayed_auth_exchange_preserves_successful_configure() {
    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), true);
    let mut live = LiveCore::start(home.path());
    let required = live.receive(|message| {
        message["type"] == "control_request" && message["request"]["subtype"] == "auth_required"
    });
    configure(home.path(), "new-provider");
    let (base_url, server) = model_server();
    live.send(serde_json::json!({"type":"control_response","response":{"subtype":"auth","request_id":required["request_id"],"response":{
        "provider_id":"keepme","provider_type":"openai_compat","persist":true,
        "values":{"base_url":base_url,"api_key":"reauth-secret","model":"home-model"}
    }}}));
    live.receive(|message| message["type"] == "system" && message["status"] == "auth_ok");
    server.join().unwrap();
    let saved: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(
        saved["ai"]["providers"].get("new-provider").is_some(),
        "{saved}"
    );
}

#[test]
fn concurrent_auth_live_configure_removes_externally_deleted_provider() {
    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), false);
    let mut live = LiveCore::start(home.path());
    live.initialize();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (entered_sender, entered_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
        let mut request = [0_u8; 2048];
        assert!(stream.read(&mut request).unwrap() > 0);
        entered_sender.send(()).unwrap();
        release_receiver.recv_timeout(REGISTRY_TIMEOUT).unwrap();
        let body = r#"{"id":"home-model","object":"model"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    live.send(serde_json::json!({"type":"registry_request","request_id":"pending","domain":"auth","action":"configure","params":{
        "provider_id":"new-provider","provider_type":"openai_compat","guarded_writes":true,
        "values":{"base_url":base_url,"api_key":"new-secret","model":"home-model"}
    }}));
    entered_receiver.recv_timeout(REGISTRY_TIMEOUT).unwrap();
    let removed = run_registry_request_with_context(
        "auth",
        "delete",
        serde_json::json!({"provider_id":"removeme","guarded_writes":true}),
        home.path(),
        Some(home.path()),
    );
    assert_eq!(removed["success"], true, "{removed}");
    release_sender.send(()).unwrap();
    let result = live.receive(|message| message["request_id"] == "pending");
    assert_eq!(result["success"], true, "{result}");
    server.join().unwrap();
    let saved: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(saved["ai"]["providers"].get("removeme").is_none());
    let state = live.registry("state", Value::Null);
    assert!(
        !state["data"]["saved_providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|provider| provider["provider_id"] == "removeme"),
        "{state}"
    );
    let activated = live.registry(
        "activate",
        serde_json::json!({"provider_id":"removeme","guarded_writes":true}),
    );
    assert_eq!(activated["success"], false, "{activated}");
}

#[test]
fn concurrent_auth_activation_rechecks_ecs_identity_after_disk_change() {
    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), false);
    let mut live = LiveCore::start(home.path());
    live.initialize();
    let mut saved: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let provider = saved["ai"]["providers"]["removeme"].as_table_mut().unwrap();
    provider.insert("type".into(), toml::Value::String("aliyun".into()));
    provider.insert(
        "auth_source".into(),
        toml::Value::String("ecs_identity".into()),
    );
    std::fs::write(&path, toml::to_string(&saved).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let activated = live.registry(
        "activate",
        serde_json::json!({"provider_id":"removeme","guarded_writes":true}),
    );
    assert_eq!(activated["success"], false, "{activated}");
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn auth_mutations_do_not_expose_secrets_from_malformed_config() {
    for guarded in [false, true] {
        for action in ["activate", "delete"] {
            let home = tempfile::tempdir().unwrap();
            let path = seed(home.path(), false);
            let mut live = LiveCore::start(home.path());
            live.initialize();
            let state_before = live.registry("state", Value::Null);
            let malformed = std::fs::read_to_string(&path)
                .unwrap()
                .replace("\"stale-secret\"", "\"sk-review-secret\" garbage");
            std::fs::write(&path, &malformed).unwrap();
            let result = live.registry(
                action,
                serde_json::json!({"provider_id":"removeme","guarded_writes":guarded}),
            );
            assert_eq!(result["success"], false, "{result}");
            assert!(!result.to_string().contains("sk-review-secret"), "{result}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), malformed);
            let state_after = live.registry("state", Value::Null);
            assert_eq!(
                state_after["data"]["active_provider"],
                state_before["data"]["active_provider"]
            );
            assert_eq!(
                state_after["data"]["saved_providers"],
                state_before["data"]["saved_providers"]
            );
        }
    }
}

#[test]
fn auth_mutations_reject_invalid_provider_types_without_exposing_secrets() {
    for action in ["activate", "delete"] {
        let home = tempfile::tempdir().unwrap();
        let path = seed(home.path(), false);
        let mut live = LiveCore::start(home.path());
        live.initialize();
        let malformed = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"stale-secret\"", "[\"sk-type-review-secret\"]");
        std::fs::write(&path, &malformed).unwrap();
        let result = live.registry(
            action,
            serde_json::json!({"provider_id":"removeme","guarded_writes":true}),
        );
        assert_eq!(result["success"], false, "{result}");
        assert!(
            !result.to_string().contains("sk-type-review-secret"),
            "{result}"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), malformed);
    }
}

#[test]
fn review_auth_object_arrays_survive_repeated_activation_and_removal() {
    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), false);
    let original = std::fs::read_to_string(&path).unwrap().replace(
        "[ai.providers.removeme]",
        "[ai.providers.keepme.extra_params.schema]\noneOf = [{ type = \"string\" }]\n[ai.providers.removeme]",
    );
    std::fs::write(&path, original).unwrap();
    for iteration in 0..3 {
        let result = run_registry_request_with_context(
            "auth",
            "activate",
            serde_json::json!({"provider_id":"keepme"}),
            home.path(),
            Some(home.path()),
        );
        assert_eq!(result["success"], true, "{result}");
        let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved["ai"]["providers"]["keepme"]["extra_params"]["schema"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "iteration={iteration}: {saved}"
        );
    }
    let removed = run_registry_request_with_context(
        "auth",
        "delete",
        serde_json::json!({"provider_id":"keepme"}),
        home.path(),
        Some(home.path()),
    );
    assert_eq!(removed["success"], true, "{removed}");
    let saved: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(saved["ai"]["providers"].get("keepme").is_none(), "{saved}");
    assert!(saved["ai"]["providers"].get("removeme").is_some());
}

#[test]
fn review_auth_activation_preserves_non_ai_multiline_table_text() {
    for delimiter in ["'''", "\"\"\""] {
        let home = tempfile::tempdir().unwrap();
        let path = seed(home.path(), false);
        let extra = format!("\n[notes]\nmessage = {delimiter}\n[[ai.example]]\nkeep this non-AI text\n{delimiter}\n");
        let original = std::fs::read_to_string(&path).unwrap() + &extra;
        std::fs::write(&path, &original).unwrap();
        let expected: toml::Value = toml::from_str(&original).unwrap();
        let result = run_registry_request_with_context(
            "auth",
            "activate",
            serde_json::json!({"provider_id":"keepme"}),
            home.path(),
            Some(home.path()),
        );
        assert_eq!(result["success"], true, "{result}");
        let saved = std::fs::read_to_string(path).unwrap();
        let parsed: toml::Value = toml::from_str(&saved).unwrap();
        assert_eq!(parsed["notes"], expected["notes"]);
        assert!(saved.contains(extra.trim_start_matches('\n')), "{saved}");
    }
}

#[test]
fn review_auth_session_credentials_survive_unrelated_delete_and_next_request() {
    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), true);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for expected_path in ["GET /v1/models/home-model ", "POST /v1/chat/completions "] {
            let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0_u8; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&bytes);
            assert!(request.starts_with(expected_path), "{request}");
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer session-only-key"),
                "{request}"
            );
            let (content_type, body) = if expected_path.starts_with("GET") {
                (
                    "application/json",
                    "{\"id\":\"home-model\",\"object\":\"model\"}",
                )
            } else {
                ("text/event-stream", "data: {\"choices\":[{\"delta\":{\"content\":\"session credentials retained\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n")
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let mut live = LiveCore::start(home.path());
    let required = live.receive(|message| message["request"]["subtype"] == "auth_required");
    live.send(serde_json::json!({"type":"control_response","response":{"subtype":"auth","request_id":required["request_id"],"response":{
        "provider_id":"keepme","provider_type":"openai_compat","persist":false,
        "values":{"base_url":base_url,"api_key":"session-only-key","model":"home-model"}
    }}}));
    live.receive(|message| message["type"] == "system" && message["status"] == "auth_ok");
    live.initialize();
    let removed = live.registry("delete", serde_json::json!({"provider_id":"removeme"}));
    assert_eq!(removed["success"], true, "{removed}");
    let state = live.registry("state", Value::Null);
    let providers = state["data"]["saved_providers"].as_array().unwrap();
    let current = providers
        .iter()
        .find(|provider| provider["provider_id"] == "keepme")
        .unwrap();
    assert_eq!(current["api_key_len"], "session-only-key".len(), "{state}");
    assert_eq!(state["data"]["effective_auth_required"], false, "{state}");
    assert!(!std::fs::read_to_string(path)
        .unwrap()
        .contains("session-only-key"));
    live.send(serde_json::json!({"type":"user","message":{"role":"user","content":"reply once"}}));
    let result = live.receive(|message| message["type"] == "result");
    assert_ne!(result["is_error"], true, "{result}");
    server.join().unwrap();
}

#[test]
fn review_auth_delete_active_provider_replaces_engine_for_next_request() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let home = tempfile::tempdir().unwrap();
    let path = seed(home.path(), false);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let server_stopped = Arc::clone(&stopped);
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        while !server_stopped.load(Ordering::SeqCst) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream.set_read_timeout(Some(SERVER_IO_TIMEOUT)).unwrap();
            stream.set_write_timeout(Some(SERVER_IO_TIMEOUT)).unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0_u8; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"removed-provider-response\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    let mut document: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let active = document["ai"]["providers"]["keepme"]
        .as_table_mut()
        .unwrap();
    active.insert("type".into(), toml::Value::String("openai".into()));
    active.insert("base_url".into(), toml::Value::String(base_url));
    active.insert(
        "api_key".into(),
        toml::Value::String("removed-active-key".into()),
    );
    std::fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
    let mut live = LiveCore::start(home.path());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        live.initialize();
        live.send(serde_json::json!({"type":"user","message":{"role":"user","content":"before deletion"}}));
        let before = live.receive(|message| message["type"] == "result");
        assert_ne!(before["is_error"], true, "{before}");
        let removed = live.registry(
            "delete",
            serde_json::json!({"provider_id":"keepme","guarded_writes":true}),
        );
        assert_eq!(removed["success"], true, "{removed}");
        let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved["ai"]["providers"].get("keepme").is_none());
        live.send(
            serde_json::json!({"type":"user","message":{"role":"user","content":"after deletion"}}),
        );
        live.receive(|message| message["type"] == "result");
    }));
    stopped.store(true, Ordering::SeqCst);
    let requests = server.join().unwrap();
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure);
    }
    assert_eq!(
        requests.len(),
        1,
        "removed provider was contacted after deletion: {requests:?}"
    );
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains("authorization: bearer removed-active-key"));
}
