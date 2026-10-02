use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncWriteExt;

const SERVER_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_IO_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(30);

fn accept_with_timeout(listener: &TcpListener, timeout: Duration) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "mock server did not receive the expected connection",
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(SERVER_IO_TIMEOUT))?;
                stream.set_write_timeout(Some(SERVER_IO_TIMEOUT))?;
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[test]
fn mock_server_without_connection_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let result = accept_with_timeout(&listener, Duration::from_millis(50));
        sender.send(result.map(|_| ())).unwrap();
    });

    let error = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("mock server did not finish within its deadline")
        .expect_err("mock server must fail when no client connects");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    server.join().unwrap();
}

fn model_server() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
        let mut request = [0_u8; 2048];
        assert!(stream.read(&mut request).unwrap() > 0);
        let body = r#"{"id":"home-model","object":"model"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    (format!("http://{address}/v1"), thread)
}

fn rejecting_model_server(status: u16) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
        let mut request = [0_u8; 2048];
        assert!(stream.read(&mut request).unwrap() > 0);
        let body = r#"{"error":{"code":"invalid_api_key"}}"#;
        write!(
            stream,
            "HTTP/1.1 {status} Rejected\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    (format!("http://{address}/v1"), thread)
}

fn aliyun_response_server(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
        let mut request = [0_u8; 4096];
        let count = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.starts_with("POST /api/v1/openapi/initial HTTP/1.1"));
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    (format!("http://{address}"), thread)
}

fn aliyun_copilot_rejection_server(
    status: u16,
    body: &'static str,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        for (expected_path, response_status, response_body) in [
            (
                "/api/v1/openapi/initial",
                200,
                r#"{"code":"Success","data":{"role_exist":true}}"#,
            ),
            (
                "/api/v1/copilot/generate_copilot_stream_response",
                status,
                body,
            ),
        ] {
            let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(
                request.starts_with(&format!("POST {expected_path} HTTP/1.1")),
                "{request}"
            );
            write!(
                stream,
                "HTTP/1.1 {response_status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            )
            .unwrap();
        }
    });
    (format!("http://{address}"), thread)
}

fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("cosh-core");
    path
}

fn run_registry_request(domain: &str, action: &str, params: Value) -> Value {
    let home = tempfile::tempdir().expect("temp home");
    run_registry_request_with_context(domain, action, params, home.path(), None)
}

fn run_registry_request_with_context(
    domain: &str,
    action: &str,
    params: Value,
    home: &Path,
    cwd: Option<&Path>,
) -> Value {
    run_registry_request_with_args(domain, action, params, home, cwd, &[])
}

fn run_registry_request_with_args(
    domain: &str,
    action: &str,
    params: Value,
    home: &Path,
    cwd: Option<&Path>,
    args: &[&str],
) -> Value {
    run_registry_request_with_args_and_env(domain, action, params, home, cwd, args, &[])
}

fn run_registry_request_with_args_and_env(
    domain: &str,
    action: &str,
    params: Value,
    home: &Path,
    cwd: Option<&Path>,
    args: &[&str],
    env: &[(&str, &str)],
) -> Value {
    let bin = binary_path();
    let request = serde_json::json!({
        "type": "registry_request",
        "request_id": "test-1",
        "domain": domain,
        "action": action,
        "params": params,
    });

    let mut command = Command::new(&bin);
    command
        .arg("--registry")
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("COSH_AI_PROVIDER")
        .env_remove("COSH_MODEL")
        .env_remove("COSH_SYSOM_ENDPOINT")
        .env_remove("COSH_SYSOM_VPC_PROXY_HOST")
        .env_remove("COSH_SYSOM_PROBE_TIMEOUT_MS")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("DASHSCOPE_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ALIBABA_CLOUD_ACCESS_KEY_ID")
        .env_remove("ALIBABA_CLOUD_ACCESS_KEY_SECRET")
        .env_remove("ALIBABA_CLOUD_SECURITY_TOKEN")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    registry_command_response(command, request)
}

fn registry_command_response(command: Command, request: Value) -> Value {
    let output = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut child = tokio::process::Command::from(command)
                .kill_on_drop(true)
                .spawn()
                .unwrap_or_else(|e| panic!("Failed to spawn registry command: {e}"));
            let mut stdin = child.stdin.take().unwrap();
            let request = format!("{request}\n");
            tokio::time::timeout(REGISTRY_TIMEOUT, async move {
                let write_request = async move { stdin.write_all(request.as_bytes()).await };
                let (written, output) = tokio::join!(write_request, child.wait_with_output());
                written.expect("failed to write registry request");
                output.expect("failed to collect registry output")
            })
            .await
            .expect("registry command exceeded its deadline")
        });
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("bad JSON: {e}: {l}")))
        .next()
        .expect("expected at least one response line")
}

#[test]
fn bare_registry_reports_env_only_auth_as_satisfied() {
    let home = tempfile::tempdir().expect("temp home");
    let resp = run_registry_request_with_args_and_env(
        "auth",
        "state",
        Value::Null,
        home.path(),
        None,
        &["--bare"],
        &[
            ("COSH_AI_PROVIDER", "gate4"),
            ("COSH_MODEL", "gate4-model"),
            ("OPENAI_BASE_URL", "http://127.0.0.1:1/v1"),
            ("OPENAI_API_KEY", "test-env-only-key"),
        ],
    );

    assert_eq!(resp["success"], true);
    assert_eq!(resp["data"]["saved_providers"], serde_json::json!([]));
    assert_eq!(resp["data"]["effective_auth_required"], false);
}

#[test]
fn registry_auth_state_includes_localized_provider_guidance() {
    let resp = run_registry_request("auth", "state", Value::Null);
    let templates = resp["data"]["templates"]
        .as_array()
        .expect("auth templates");
    let dashscope = templates
        .iter()
        .find(|provider| provider["id"] == "dashscope")
        .expect("DashScope template");

    assert_eq!(dashscope["description_zh_cn"], "使用现有的百炼 API Key");
}

#[test]
fn bare_registry_does_not_discover_project_skills() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let skill_dir = project.path().join(".copilot-shell/skills/project-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: project-skill\ndescription: project only\n---\n\nBody.",
    )
    .unwrap();

    let regular = run_registry_request_with_args(
        "skills",
        "list",
        Value::Null,
        home.path(),
        Some(project.path()),
        &[],
    );
    let bare = run_registry_request_with_args(
        "skills",
        "list",
        Value::Null,
        home.path(),
        Some(project.path()),
        &["--bare"],
    );

    assert!(regular["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|skill| skill["name"] == "project-skill"));
    assert!(bare["data"].as_array().unwrap().is_empty(), "{bare}");
}

#[test]
fn registry_extensions_list_returns_success() {
    let resp = run_registry_request("extensions", "list", Value::Null);
    assert_eq!(resp["type"], "registry_response");
    assert_eq!(resp["request_id"], "test-1");
    assert_eq!(resp["success"], true);
    assert!(resp["data"].is_array(), "data should be array: {resp}");
}

#[test]
fn registry_extensions_report_desired_effective_and_health() {
    let home = tempfile::tempdir().expect("temp home");
    let extension = home.path().join(".copilot-shell/extensions/example.ops");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.ops",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"}
        }"#,
    )
    .unwrap();

    let listed =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    let extension = &listed["data"][0];
    assert_eq!(extension["name"], "example.ops");
    assert_eq!(extension["desired_state"], "enabled");
    assert_eq!(extension["effective_state"], "not_loaded");
    assert_eq!(extension["activation"], "next_session");
    assert_eq!(extension["health"], "healthy");
    assert_eq!(extension["schema_version"], "v1");

    let disabled = run_registry_request_with_context(
        "extensions",
        "disable",
        serde_json::json!({"name":"example.ops"}),
        home.path(),
        None,
    );
    assert_eq!(disabled["success"], true);
    assert_eq!(disabled["data"]["desired_state"], "disabled");
    assert_eq!(disabled["data"]["activation"], "next_session");

    let relisted =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    assert_eq!(relisted["data"][0]["desired_state"], "disabled");
    assert_eq!(relisted["data"][0]["effective_state"], "not_loaded");
}

#[test]
fn registry_enable_rolls_back_when_required_runtime_health_fails() {
    let home = tempfile::tempdir().expect("temp home");
    let extension = home
        .path()
        .join(".copilot-shell/extensions/example.required");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.required",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"},
            "settings":[{
                "key":"endpoint",
                "type":"string",
                "description":"required endpoint",
                "required":true
            }]
        }"#,
    )
    .unwrap();
    let disabled = run_registry_request_with_context(
        "extensions",
        "disable",
        serde_json::json!({"name":"example.required"}),
        home.path(),
        None,
    );
    assert_eq!(disabled["success"], true, "{disabled}");

    let enabled = run_registry_request_with_context(
        "extensions",
        "enable",
        serde_json::json!({"name":"example.required"}),
        home.path(),
        None,
    );

    assert_eq!(enabled["success"], false, "{enabled}");
    assert!(enabled["error"]
        .as_str()
        .unwrap()
        .contains("extension_candidate_validation_failed"));
    let listed =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    assert_eq!(listed["data"][0]["desired_state"], "disabled");
}

#[test]
fn registry_extensions_path_copy_requires_preflight_commit() {
    let home = tempfile::tempdir().expect("temp home");
    let source_root = tempfile::tempdir().expect("temp source");
    std::fs::write(
        source_root.path().join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.managed",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"}
        }"#,
    )
    .unwrap();

    let preflight = run_registry_request_with_context(
        "extensions",
        "install-preflight",
        serde_json::json!({"source": source_root.path()}),
        home.path(),
        None,
    );
    assert_eq!(preflight["success"], true, "{preflight}");
    assert_eq!(preflight["data"]["name"], "example.managed");
    assert!(!home
        .path()
        .join(".copilot-shell/extensions/.managed/example.managed")
        .exists());

    let committed = run_registry_request_with_context(
        "extensions",
        "commit",
        serde_json::json!({
            "operation_id": preflight["data"]["operation_id"],
            "fingerprint": preflight["data"]["capability_fingerprint"],
        }),
        home.path(),
        None,
    );
    assert_eq!(committed["success"], true, "{committed}");
    assert_eq!(committed["data"]["activation"], "next_session");

    let listed =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    assert_eq!(listed["data"][0]["name"], "example.managed");
    assert_eq!(listed["data"][0]["source"], "path-copy");
    assert_eq!(listed["data"][0]["update_status"], "not_updatable");

    let update = run_registry_request_with_context(
        "extensions",
        "update-preflight",
        serde_json::json!({"name": "example.managed"}),
        home.path(),
        None,
    );
    assert_eq!(update["success"], false, "{update}");
    assert!(update["error"]
        .as_str()
        .unwrap()
        .starts_with("extension_source_not_updatable:"));

    let update_all = run_registry_request_with_context(
        "extensions",
        "update-all-preflight",
        Value::Null,
        home.path(),
        None,
    );
    assert_eq!(update_all["success"], true, "{update_all}");
    assert_eq!(update_all["data"]["status"], "prepared");
    let batch_id = update_all["data"]["operation_id"].as_str().unwrap();
    let update_all = run_registry_request_with_context(
        "extensions",
        "update-all-commit",
        serde_json::json!({"operation_id": batch_id}),
        home.path(),
        None,
    );
    assert_eq!(update_all["success"], true, "{update_all}");
    assert_eq!(update_all["data"]["status"], "completed");
    assert_eq!(update_all["data"]["summary"]["skipped"], 1);
    assert_eq!(update_all["data"]["items"][0]["outcome"], "skipped");
    let batch_result = run_registry_request_with_context(
        "extensions",
        "result",
        serde_json::json!({"operation_id": batch_id}),
        home.path(),
        None,
    );
    assert_eq!(batch_result["data"], update_all["data"]);

    let reload =
        run_registry_request_with_context("extensions", "reload", Value::Null, home.path(), None);
    assert_eq!(reload["success"], true, "{reload}");
    assert_eq!(reload["data"]["activation"], "next_session");
    assert!(reload["data"]["generation"].is_number());

    let removed = run_registry_request_with_context(
        "extensions",
        "uninstall",
        serde_json::json!({"name": "example.managed"}),
        home.path(),
        None,
    );
    assert_eq!(removed["success"], true, "{removed}");
    let relisted =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    assert_eq!(relisted["data"].as_array().unwrap().len(), 0);
}

#[test]
fn registry_commit_rolls_back_when_required_runtime_health_fails() {
    let home = tempfile::tempdir().expect("temp home");
    let source_root = tempfile::tempdir().expect("temp source");
    std::fs::write(
        source_root.path().join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.unhealthy",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"},
            "settings":[{
                "key":"endpoint",
                "type":"string",
                "description":"required endpoint",
                "required":true
            }]
        }"#,
    )
    .unwrap();
    let preflight = run_registry_request_with_context(
        "extensions",
        "install-preflight",
        serde_json::json!({"source": source_root.path()}),
        home.path(),
        None,
    );
    assert_eq!(preflight["success"], true, "{preflight}");

    let committed = run_registry_request_with_context(
        "extensions",
        "commit",
        serde_json::json!({
            "operation_id": preflight["data"]["operation_id"],
            "fingerprint": preflight["data"]["capability_fingerprint"],
        }),
        home.path(),
        None,
    );

    assert_eq!(committed["success"], false, "{committed}");
    assert!(committed["error"]
        .as_str()
        .unwrap()
        .starts_with("extension_candidate_validation_failed:"));
    assert!(!home
        .path()
        .join(".copilot-shell/extensions/.managed/example.unhealthy")
        .exists());
    let listed =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    assert!(listed["data"].as_array().unwrap().is_empty(), "{listed}");
}

#[test]
fn registry_extensions_new_creates_valid_scaffold_without_installing_it() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let parent = project.path().join("parent with spaces");
    std::fs::create_dir(&parent).unwrap();
    let target = parent.join("sample-extension");
    let response = run_registry_request_with_context(
        "extensions",
        "new",
        serde_json::json!({"path": target, "template": "mcp"}),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["data"]["name"], "sample-extension");
    assert_eq!(response["data"]["template"], "mcp");
    assert!(target.join("cosh-extension.json").is_file());
    assert!(target.join("mcp/README.md").is_file());

    let listed =
        run_registry_request_with_context("extensions", "list", Value::Null, home.path(), None);
    assert_eq!(listed["success"], true, "{listed}");
    assert!(listed["data"].as_array().unwrap().is_empty());
}

#[test]
fn registry_extensions_git_install_rejects_non_https_source() {
    let response = run_registry_request(
        "extensions",
        "install-preflight",
        serde_json::json!({
            "source_kind": "git-https",
            "source": "ssh://example.com/repository.git"
        }),
    );
    assert_eq!(response["success"], false, "{response}");
    assert!(response["error"]
        .as_str()
        .unwrap()
        .starts_with("extension_git_protocol_unsupported:"));
}

#[test]
fn registry_extension_settings_parse_persist_and_fallback() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let extension = home.path().join(".copilot-shell/extensions/example.ops");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.ops",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"},
            "settings":[
                {"key":"region","type":"string","description":"region","default":"default-region"},
                {"key":"retries","type":"integer","description":"retries"}
            ]
        }"#,
    )
    .unwrap();

    let set = run_registry_request_with_context(
        "extensions",
        "settings-set",
        serde_json::json!({
            "name":"example.ops",
            "key":"region",
            "value":"cn-hangzhou",
            "scope":"user"
        }),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(set["success"], true, "{set}");
    assert_eq!(set["data"]["setting"]["value"], "cn-hangzhou");
    assert_eq!(set["data"]["activation"], "pending_safe_reload");
    assert!(set["data"]["candidate_generation"].is_number());

    let get = run_registry_request_with_context(
        "extensions",
        "settings-get",
        serde_json::json!({"name":"example.ops","key":"region"}),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(get["success"], true, "{get}");
    assert_eq!(get["data"]["scope"], "user");
    assert_eq!(get["data"]["value"], "cn-hangzhou");

    let invalid = run_registry_request_with_context(
        "extensions",
        "settings-set",
        serde_json::json!({
            "name":"example.ops",
            "key":"retries",
            "value":"many",
            "scope":"user"
        }),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(invalid["success"], false, "{invalid}");
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .starts_with("extension_setting_type_invalid:"));

    let unset = run_registry_request_with_context(
        "extensions",
        "settings-unset",
        serde_json::json!({"name":"example.ops","key":"region","scope":"user"}),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(unset["success"], true, "{unset}");
    assert_eq!(unset["data"]["setting"]["value"], "default-region");
    assert_eq!(unset["data"]["setting"]["scope"], Value::Null);
}

#[test]
fn registry_required_setting_unset_rolls_back_unhealthy_candidate() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let extension = home
        .path()
        .join(".copilot-shell/extensions/example.required");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.required",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"},
            "settings":[{
                "key":"endpoint",
                "type":"string",
                "description":"required endpoint",
                "required":true
            }]
        }"#,
    )
    .unwrap();

    let set = run_registry_request_with_context(
        "extensions",
        "settings-set",
        serde_json::json!({
            "name":"example.required",
            "key":"endpoint",
            "value":"https://service.example",
            "scope":"user"
        }),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(set["success"], true, "{set}");

    let unset = run_registry_request_with_context(
        "extensions",
        "settings-unset",
        serde_json::json!({
            "name":"example.required",
            "key":"endpoint",
            "scope":"user"
        }),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(unset["success"], false, "{unset}");
    assert!(unset["error"]
        .as_str()
        .unwrap()
        .contains("extension_candidate_validation_failed"));

    let get = run_registry_request_with_context(
        "extensions",
        "settings-get",
        serde_json::json!({"name":"example.required","key":"endpoint"}),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(get["success"], true, "{get}");
    assert_eq!(get["data"]["value"], "https://service.example");
}

#[test]
fn registry_workspace_settings_require_existing_project_trust() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let extension = home.path().join(".copilot-shell/extensions/example.ops");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.ops",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"},
            "settings":[{"key":"region","type":"string","description":"region"}]
        }"#,
    )
    .unwrap();
    let params = serde_json::json!({
        "name":"example.ops",
        "key":"region",
        "value":"workspace-region",
        "scope":"workspace"
    });
    let denied = run_registry_request_with_context(
        "extensions",
        "settings-set",
        params.clone(),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(denied["success"], false, "{denied}");
    assert!(denied["error"]
        .as_str()
        .unwrap()
        .starts_with("extension_workspace_untrusted:"));

    let trust_store = home
        .path()
        .join(".copilot-shell/cosh/trusted-project-hooks");
    std::fs::create_dir_all(trust_store.parent().unwrap()).unwrap();
    std::fs::write(
        trust_store,
        format!("{}\n", project.path().canonicalize().unwrap().display()),
    )
    .unwrap();
    let allowed = run_registry_request_with_context(
        "extensions",
        "settings-set",
        params,
        home.path(),
        Some(project.path()),
    );
    assert_eq!(allowed["success"], true, "{allowed}");
    assert_eq!(allowed["data"]["setting"]["scope"], "workspace");
    assert!(project
        .path()
        .join(".copilot-shell/extension-settings.json")
        .is_file());
}

#[test]
fn registry_extension_info_reports_declared_agents_as_non_executable() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let extension = home.path().join(".copilot-shell/extensions/example.ops");
    std::fs::create_dir_all(extension.join("agents")).unwrap();
    std::fs::write(
        extension.join("agents/reviewer.md"),
        "---\nname: reviewer\ndescription: Review incidents\ntools:\n  - read_file\n---\n\nReview safely.",
    )
    .unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion":1,
            "name":"example.ops",
            "version":"1.0.0",
            "compatibility":{"cosh":">=0.12.0"},
            "agents":["agents"]
        }"#,
    )
    .unwrap();
    let response = run_registry_request_with_context(
        "extensions",
        "info",
        serde_json::json!({"name":"example.ops"}),
        home.path(),
        Some(project.path()),
    );
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(
        response["data"]["agents"][0]["id"],
        "example.ops/agent/reviewer"
    );
    assert_eq!(response["data"]["agents"][0]["executable"], false);
    assert_eq!(
        response["data"]["agents"][0]["effective_tools"][0],
        "read_file"
    );
    assert!(response["data"]["agents"][0].get("prompt").is_none());
}

#[test]
fn registry_skills_list_returns_success() {
    let resp = run_registry_request("skills", "list", Value::Null);
    assert_eq!(resp["type"], "registry_response");
    assert_eq!(resp["request_id"], "test-1");
    assert_eq!(resp["success"], true);
    assert!(resp["data"].is_array(), "data should be array: {resp}");
}

#[test]
fn registry_skills_discovers_raw_user_data_directory() {
    let xdg = tempfile::tempdir().unwrap();
    let dot = xdg.path().join("./data");
    let parent = xdg.path().join("../data");
    for (data_home, use_xdg) in [
        (None, false),
        (Some(xdg.path().to_str().unwrap()), true),
        (Some(""), false),
        (Some("relative-data"), false),
        (Some(dot.to_str().unwrap()), false),
        (Some(parent.to_str().unwrap()), false),
    ] {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let data_dir = if use_xdg {
            xdg.path().to_path_buf()
        } else {
            home.path().join(".local/share")
        };
        let skill_dir = data_dir.join("anolisa/skills/raw-install-probe");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: raw-install-probe\ndescription: raw user install\n---\nRaw skill body.",
        )
        .unwrap();
        let env: Vec<_> = data_home
            .map(|value| ("XDG_DATA_HOME", value))
            .into_iter()
            .collect();
        let response = run_registry_request_with_args_and_env(
            "skills",
            "list",
            Value::Null,
            home.path(),
            Some(project.path()),
            &[],
            &env,
        );
        assert_eq!(response["success"], true);
        let skill = response["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["name"] == "raw-install-probe")
            .unwrap_or_else(|| panic!("raw skill missing for XDG_DATA_HOME={data_home:?}"));
        assert_eq!(skill["level"], "user");
    }
}

#[test]
fn registry_skills_discovers_custom_system_prefix() {
    let binary = binary_path();
    let prefix = tempfile::tempdir_in(binary.parent().unwrap()).unwrap();
    let home = tempfile::tempdir().unwrap();
    for (data_root, description) in [
        ("usr/local/share/anolisa", "raw"),
        ("usr/share/anolisa", "package"),
    ] {
        let skill = prefix.path().join(data_root).join("skills/prefix-probe");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: prefix-probe\ndescription: {description}\n---\nBody."),
        )
        .unwrap();
    }
    for runtime in [
        "usr/local/libexec/anolisa/cosh-ng",
        "usr/libexec/anolisa/cosh-ng",
    ] {
        let executable = prefix.path().join(runtime).join("cosh-core");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        // Relocate without opening the executable for writing during parallel spawns.
        std::fs::hard_link(&binary, &executable).unwrap();
        let mut command = Command::new(executable);
        command
            .arg("--registry")
            .env_clear()
            .env("HOME", home.path())
            .current_dir(home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let response = registry_command_response(
            command,
            serde_json::json!({
                "type": "registry_request", "request_id": "prefix-probe",
                "domain": "skills", "action": "list", "params": null,
            }),
        );
        assert_eq!(response["success"], true);
        let skill = response["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["name"] == "prefix-probe")
            .unwrap_or_else(|| panic!("skill missing for runtime {runtime}"));
        assert_eq!(skill["level"], "system");
        assert_eq!(skill["description"], "raw");
    }
}

#[test]
fn registry_hooks_list_returns_success() {
    let resp = run_registry_request("hooks", "list", Value::Null);
    assert_eq!(resp["type"], "registry_response");
    assert_eq!(resp["request_id"], "test-1");
    assert_eq!(resp["success"], true);
    assert!(resp["data"].is_array(), "data should be array: {resp}");
}

#[test]
fn registry_auth_state_merges_user_auth_with_project_preferences() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let home_config_dir = home.path().join(".copilot-shell");
    let project_config_dir = project.path().join(".copilot-shell");
    std::fs::create_dir_all(&home_config_dir).unwrap();
    std::fs::create_dir_all(&project_config_dir).unwrap();
    std::fs::write(
        home_config_dir.join("config.toml"),
        r#"
[ai]
active_provider = "user-dashscope"

[ai.providers.user-dashscope]
type = "dashscope"
api_key = "sk-user"
model = "user-model"
"#,
    )
    .unwrap();
    std::fs::write(
        project_config_dir.join("config.toml"),
        r#"
[ai]
active_provider = "project-provider"
active_model = "project-model"

[ai.providers.project-provider]
type = "dashscope"
api_key = "sk-project"
"#,
    )
    .unwrap();

    let resp = run_registry_request_with_context(
        "auth",
        "state",
        Value::Null,
        home.path(),
        Some(project.path()),
    );
    assert_eq!(resp["type"], "registry_response");
    assert_eq!(resp["success"], true);
    assert_eq!(resp["data"]["active_provider"], "user-dashscope");

    let saved = resp["data"]["saved_providers"].as_array().unwrap();
    assert_eq!(saved.len(), 1, "project provider must be ignored: {resp}");
    assert_eq!(saved[0]["provider_id"], "user-dashscope");
    assert_eq!(saved[0]["api_key_len"], 7);
    assert_eq!(saved[0]["model"], "user-model");
}

#[test]
fn registry_auth_configure_writes_home_config_only() {
    let home = tempfile::tempdir().expect("temp home");
    let project = tempfile::tempdir().expect("temp project");
    let home_config_dir = home.path().join(".copilot-shell");
    let project_config_dir = project.path().join(".copilot-shell");
    std::fs::create_dir_all(&home_config_dir).unwrap();
    std::fs::create_dir_all(&project_config_dir).unwrap();
    let project_config_path = project_config_dir.join("config.toml");
    std::fs::write(
        &project_config_path,
        r#"
[ai]
active_model = "project-model"

[ai.providers.project-provider]
type = "dashscope"
api_key = "sk-project"
"#,
    )
    .unwrap();

    let (base_url, server) = model_server();
    let resp = run_registry_request_with_context(
        "auth",
        "configure",
        serde_json::json!({
            "provider_id": "home-provider",
            "provider_type": "openai_compat",
            "values": {
                "base_url": base_url,
                "api_key": "sk-home",
                "model": "home-model"
            }
        }),
        home.path(),
        Some(project.path()),
    );
    server.join().unwrap();
    assert_eq!(resp["success"], true);

    let home_config = std::fs::read_to_string(home_config_dir.join("config.toml")).unwrap();
    let project_config = std::fs::read_to_string(project_config_path).unwrap();

    assert!(home_config.contains("[ai.providers.home-provider]"));
    assert!(home_config.contains("api_key = \"sk-home\""));
    assert!(!home_config.contains("project-model"));
    assert!(!home_config.contains("project-provider"));
    assert!(project_config.contains("project-model"));
    assert!(project_config.contains("sk-project"));
}

#[test]
fn registry_auth_configure_rejects_invalid_base_url_without_writing_config() {
    let home = tempfile::tempdir().expect("temp home");
    let config_path = home.path().join(".copilot-shell/config.toml");

    let response = run_registry_request_with_context(
        "auth",
        "configure",
        serde_json::json!({
            "provider_id": "bad-url",
            "provider_type": "openai_compat",
            "values": {
                "base_url": "error-testhttps://api.example.com/v1",
                "api_key": "sk-test",
                "model": "qwen-test"
            }
        }),
        home.path(),
        None,
    );

    assert_eq!(response["success"], false);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("invalid base_url")),
        "{response}"
    );
    assert!(!config_path.exists());
}

#[test]
fn registry_auth_preflight_rejection_is_classified_without_writing_config() {
    for (status, expected_code) in [(401, "invalid_credentials"), (403, "permission_denied")] {
        let home = tempfile::tempdir().expect("temp home");
        let config_path = home.path().join(".copilot-shell/config.toml");
        let (base_url, server) = rejecting_model_server(status);
        let response = run_registry_request_with_context(
            "auth",
            "configure",
            serde_json::json!({
                "provider_id": "rejected-provider",
                "provider_type": "dashscope",
                "values": {
                    "base_url": base_url,
                    "api_key": "sk-not-logged",
                    "model": "home-model"
                }
            }),
            home.path(),
            None,
        );
        server.join().unwrap();

        assert_eq!(response["success"], false, "{response}");
        assert_eq!(response["data"]["error_code"], expected_code, "{response}");
        assert!(!config_path.exists());
        assert!(!response.to_string().contains("sk-not-logged"));
    }
}

#[test]
fn registry_auth_rejects_missing_aliyun_role_without_writing_config() {
    for saved_profile in [true, false] {
        let home = tempfile::tempdir().expect("temp home");
        let config_path = home.path().join(".copilot-shell/config.toml");
        let (sysom_endpoint, server) =
            aliyun_response_server(r#"{"code":"Success","data":{"role_exist":false}}"#);
        let original_config =
            format!("[ai.providers.sysom-not-ready]\nsysom_endpoint = \"{sysom_endpoint}\"\n");
        let env = if saved_profile {
            std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            std::fs::write(&config_path, &original_config).unwrap();
            vec![]
        } else {
            assert!(!config_path.exists());
            vec![("COSH_SYSOM_ENDPOINT", sysom_endpoint.as_str())]
        };
        let response = run_registry_request_with_args_and_env(
            "auth",
            "configure",
            serde_json::json!({
                "provider_id": "sysom-not-ready",
                "provider_type": "aliyun",
                "values": {
                    "base_url": "http://127.0.0.1:1/unused-openai/v1",
                    "access_key_id": "test-access-key",
                    "access_key_secret": "test-secret",
                    "model": "qwen-test"
                }
            }),
            home.path(),
            None,
            &[],
            &env,
        );
        server.join().unwrap();

        assert_eq!(response["success"], false, "{response}");
        assert_eq!(response["data"]["error_code"], "service_not_ready");
        if saved_profile {
            assert_eq!(
                std::fs::read(&config_path).unwrap(),
                original_config.as_bytes()
            );
        } else {
            assert!(!config_path.exists());
        }
        assert!(!response.to_string().contains("test-access-key"));
        assert!(!response.to_string().contains("test-secret"));
    }
}

#[test]
fn registry_auth_rejects_unusable_aliyun_model_without_writing_config() {
    let home = tempfile::tempdir().expect("temp home");
    let config_path = home.path().join(".copilot-shell/config.toml");
    let (sysom_endpoint, server) =
        aliyun_copilot_rejection_server(400, r#"{"Code":"ModelNotFound"}"#);
    let original_config =
        format!("[ai.providers.sysom-bad-model]\nsysom_endpoint = \"{sysom_endpoint}\"\n");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, &original_config).unwrap();
    let response = run_registry_request_with_context(
        "auth",
        "configure",
        serde_json::json!({
            "provider_id": "sysom-bad-model",
            "provider_type": "aliyun",
            "values": {
                "base_url": "http://127.0.0.1:1/unused-openai/v1",
                "access_key_id": "test-access-key",
                "access_key_secret": "test-secret",
                "model": "definitely-not-a-real-sysom-model"
            }
        }),
        home.path(),
        None,
    );
    server.join().unwrap();

    assert_eq!(response["success"], false, "{response}");
    assert_eq!(response["data"]["error_code"], "model_unavailable");
    assert_eq!(
        std::fs::read(&config_path).unwrap(),
        original_config.as_bytes()
    );
    assert!(!response.to_string().contains("test-access-key"));
    assert!(!response.to_string().contains("test-secret"));
}

#[test]
fn registry_auth_activate_clears_a_stale_model() {
    let home = tempfile::tempdir().expect("temp home");
    let home_config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&home_config_dir).unwrap();
    let config_path = home_config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[ai]
active_provider = "old-provider"
active_model = "old-model"

[ai.providers.old-provider]
type = "dashscope"
api_key = "sk-old"
model = "old-model"

[ai.providers.no-model]
type = "dashscope"
api_key = "sk-new"
"#,
    )
    .unwrap();

    let response = run_registry_request_with_context(
        "auth",
        "activate",
        serde_json::json!({ "provider_id": "no-model" }),
        home.path(),
        None,
    );

    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["data"]["active_provider"], "no-model");
    let persisted = std::fs::read_to_string(config_path).unwrap();
    assert!(persisted.contains("active_provider = \"no-model\""));
    assert!(!persisted.contains("active_model = \"old-model\""));
}

#[test]
fn registry_auth_delete_removes_user_provider_and_credentials() {
    let home = tempfile::tempdir().expect("temp home");
    let home_config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&home_config_dir).unwrap();
    let config_path = home_config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[ai]
active_provider = "remove-me"
active_model = "delete-model"

[ai.providers.keep-me]
type = "dashscope"
api_key = "sk-keep"

[ai.providers.remove-me]
type = "openai"
api_key = "sk-remove"
model = "remove-model"
"#,
    )
    .unwrap();

    let response = run_registry_request_with_context(
        "auth",
        "delete",
        serde_json::json!({ "provider_id": "remove-me" }),
        home.path(),
        None,
    );

    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["deleted_provider"], "remove-me");
    assert!(response["data"]["active_provider"].is_null());

    let persisted = std::fs::read_to_string(&config_path).unwrap();
    assert!(!persisted.contains("[ai.providers.remove-me]"));
    assert!(!persisted.contains("sk-remove"));
    assert!(!persisted.contains("active_model = \"delete-model\""));
    assert!(persisted.contains("[ai.providers.keep-me]"));
    assert!(persisted.contains("sk-keep"));
}

#[test]
fn registry_unknown_domain_returns_error() {
    let resp = run_registry_request("unknown_domain", "list", Value::Null);
    assert_eq!(resp["type"], "registry_response");
    assert_eq!(resp["success"], false);
    assert!(resp["error"].as_str().unwrap().contains("unknown domain"));
}

#[test]
fn registry_unsupported_action_returns_error() {
    let resp = run_registry_request("extensions", "invalid_action", Value::Null);
    assert_eq!(resp["type"], "registry_response");
    assert_eq!(resp["success"], false);
    assert!(resp["error"]
        .as_str()
        .unwrap()
        .contains("unsupported action"));
}

#[test]
fn registry_extensions_detail_nonexistent_returns_error() {
    let params = serde_json::json!({ "name": "nonexistent-extension-xyz" });
    let resp = run_registry_request("extensions", "detail", params);
    assert_eq!(resp["success"], false);
    assert!(resp["error"].as_str().unwrap().contains("not found"));
}

#[test]
fn registry_skills_detail_nonexistent_returns_error() {
    let params = serde_json::json!({ "name": "nonexistent-skill-xyz" });
    let resp = run_registry_request("skills", "detail", params);
    assert_eq!(resp["success"], false);
    assert!(resp["error"].as_str().unwrap().contains("not found"));
}
