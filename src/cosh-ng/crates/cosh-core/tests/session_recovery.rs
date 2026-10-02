use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;

use rustix::fs::{flock, FlockOperation};
use serde_json::{json, Value};

mod common;

fn binary_path() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target profile directory")
        .to_path_buf();
    path.push("cosh-core");
    path
}

fn configure(home: &Path, store: &Path) {
    configure_auto_persist(home, store, true);
}

fn configure_auto_persist(home: &Path, store: &Path, auto_persist: bool) {
    let config_dir = home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"

[ai.providers.test]
type = "mock"
model = "mock-history"

[session]
auto_persist = {auto_persist}
persist_dir = "{}"
"#,
            store.display()
        ),
    )
    .expect("write config");
}

fn configure_mock_provider(home: &Path) {
    let config_dir = home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(
        config_dir.join("config.toml"),
        r#"
[ai]
active_provider = "test"

[ai.providers.test]
type = "mock"
model = "mock-history"
"#,
    )
    .expect("write mock provider config");
}

fn authentication_model_server(model: &'static str) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth model server");
    let address = listener.local_addr().expect("auth model server address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept auth model request");
        let mut request = [0_u8; 2048];
        let count = stream.read(&mut request).expect("read auth model request");
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(
            request.starts_with(&format!("GET /v1/models/{model} HTTP/1.1")),
            "unexpected auth model request: {request}"
        );
        let body = format!(r#"{{"id":"{model}","object":"model"}}"#);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write auth model response");
    });
    (format!("http://{address}/v1"), server)
}

#[test]
fn disabled_auto_persist_marks_session_non_resumable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure_auto_persist(&home, &store, false);

    let messages = json_lines(&run_prompt(&home, &workspace, &["ephemeral turn"]));
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("non-resumable system init");
    assert_eq!(init["session_resumable"], false);
    let session_id = messages
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("ephemeral diagnostic ID");

    let list = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 10
        }),
    );
    assert_eq!(list["ok"], true);
    assert!(list["data"]["sessions"]
        .as_array()
        .is_some_and(Vec::is_empty));

    let resumed = run_prompt(
        &home,
        &workspace,
        &["--resume", session_id, "must not resume"],
    );
    let resumed_messages = json_lines(&resumed);
    assert!(resumed_messages.iter().any(|message| {
        message["type"] == "result"
            && message["is_error"] == true
            && message["session_error_code"] == "not_found"
            && message["session_error_phase"] == "load"
            && message["errors"][0]
                .as_str()
                .is_some_and(|error| error.contains("[not_found]"))
    }));
}

fn run_prompt(home: &Path, workspace: &Path, args: &[&str]) -> Output {
    isolated_command(home)
        .args(["--headless", "--workspace"])
        .arg(workspace)
        .args(args)
        .output()
        .expect("run cosh-core")
}

fn run_protocol(home: &Path, workspace: &Path, args: &[&str], messages: &[Value]) -> Output {
    let mut child = isolated_command(home)
        .args(["--headless", "--workspace"])
        .arg(workspace)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cosh-core");
    {
        let stdin = child.stdin.as_mut().expect("protocol stdin");
        for message in messages {
            writeln!(stdin, "{message}").expect("write protocol message");
        }
    }
    child.wait_with_output().expect("wait for cosh-core")
}

fn isolated_command(home: &Path) -> Command {
    let mut command = Command::new(binary_path());
    command
        .env("HOME", home)
        .env_remove("COSH_MODEL")
        .env_remove("COSH_AI_PROVIDER");
    for variable in [
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "DASHSCOPE_API_KEY",
        "ALIBABA_CLOUD_ACCESS_KEY_ID",
        "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
        "ALIBABA_CLOUD_SECURITY_TOKEN",
    ] {
        command.env_remove(variable);
    }
    common::opt_out_telemetry(&mut command, home);
    command
}

fn json_lines(output: &Output) -> Vec<Value> {
    assert!(
        output.status.success(),
        "cosh-core failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSONL"))
        .collect()
}

#[test]
fn second_process_resumes_model_visible_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let first = run_prompt(&home, &workspace, &["remember alpha"]);
    let first_messages = json_lines(&first);
    let session_id = first_messages
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("first session id")
        .to_string();

    let second = run_prompt(
        &home,
        &workspace,
        &["--resume", &session_id, "continue beta"],
    );
    let second_messages = json_lines(&second);
    let assistant = second_messages
        .iter()
        .find(|message| message["type"] == "assistant")
        .and_then(|message| message["message"]["content"][0]["text"].as_str())
        .expect("assistant response");

    assert!(assistant.contains("remember alpha"), "{assistant}");
    assert!(assistant.contains("continue beta"), "{assistant}");
    assert_eq!(
        second_messages
            .iter()
            .find(|message| message["type"] == "result")
            .and_then(|message| message["session_id"].as_str()),
        Some(session_id.as_str())
    );
}

#[test]
fn persistence_conflict_emits_structured_persist_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let first_messages = json_lines(&run_prompt(&home, &workspace, &["remember before lock"]));
    let session_id = first_messages
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("first session id");
    let scoped_dir = fs::read_dir(&store)
        .expect("read session root")
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .expect("workspace-scoped session directory")
        .path();
    let lock_path = scoped_dir.join(format!(".{session_id}.lock"));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open session lock");
    flock(&lock, FlockOperation::LockExclusive).expect("hold session lock");

    let resumed = json_lines(&run_prompt(
        &home,
        &workspace,
        &["--resume", session_id, "turn that cannot persist"],
    ));
    let result = resumed
        .iter()
        .find(|message| message["type"] == "result")
        .expect("persist failure result");

    assert_eq!(result["is_error"], true);
    assert_eq!(result["session_error_code"], "conflict");
    assert_eq!(result["session_error_phase"], "persist");
    assert!(result["errors"][0]
        .as_str()
        .is_some_and(|error| error.contains("session persistence failed [conflict]")));
    flock(&lock, FlockOperation::Unlock).expect("release session lock");
}

#[test]
fn requested_workspace_controls_project_session_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let process_workspace = temp.path().join("process-workspace");
    let requested_workspace = temp.path().join("requested-workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&process_workspace).expect("create process workspace");
    fs::create_dir_all(requested_workspace.join(".copilot-shell"))
        .expect("create requested project config directory");
    configure_mock_provider(&home);
    fs::write(
        requested_workspace.join(".copilot-shell/config.toml"),
        r#"
[session]
auto_persist = false
persist_dir = "project-sessions"
"#,
    )
    .expect("write requested project config");

    let mut command = Command::new(binary_path());
    command
        .env("HOME", &home)
        .current_dir(&process_workspace)
        .args(["--headless", "--workspace"])
        .arg(&requested_workspace)
        .arg("workspace-owned config");
    common::opt_out_telemetry(&mut command, &home);
    let output = command.output().expect("run from another cwd");
    let messages = json_lines(&output);
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");

    assert_eq!(init["session_resumable"], false);
    assert!(!requested_workspace.join("project-sessions").exists());
    assert!(!process_workspace.join("project-sessions").exists());
}

#[test]
fn session_control_loads_persist_dir_from_requested_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let process_workspace = temp.path().join("process-workspace");
    let requested_workspace = temp.path().join("requested-workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&process_workspace).expect("create process workspace");
    fs::create_dir_all(requested_workspace.join(".copilot-shell"))
        .expect("create requested project config directory");
    configure_mock_provider(&home);
    fs::write(
        requested_workspace.join(".copilot-shell/config.toml"),
        r#"
[session]
auto_persist = true
persist_dir = "project-sessions"
"#,
    )
    .expect("write requested project config");

    let mut command = Command::new(binary_path());
    command
        .env("HOME", &home)
        .current_dir(&process_workspace)
        .args(["--headless", "--workspace"])
        .arg(&requested_workspace)
        .arg("persist in requested workspace");
    common::opt_out_telemetry(&mut command, &home);
    let output = command.output().expect("persist with requested config");
    let session_id = json_lines(&output)
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("persisted session ID")
        .to_string();
    let list = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": requested_workspace,
            "limit": 10
        }),
    );

    assert_eq!(list["ok"], true);
    assert_eq!(list["data"]["sessions"][0]["session_id"], session_id);
    assert!(requested_workspace.join("project-sessions").exists());
    assert!(!process_workspace.join("project-sessions").exists());
}

#[test]
fn process_cwd_legacy_session_cannot_be_claimed_by_another_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let process_workspace = temp.path().join("process-workspace");
    let requested_workspace = temp.path().join("requested-workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&process_workspace).expect("create process workspace");
    fs::create_dir_all(&requested_workspace).expect("create requested workspace");
    configure_mock_provider(&home);
    let session_id = "55555555-5555-4555-8555-555555555555";
    let legacy_dir = process_workspace.join("sessions");
    let legacy_file = legacy_dir.join(format!("{session_id}.json"));
    fs::create_dir_all(&legacy_dir).expect("create legacy directory");
    fs::write(
        &legacy_file,
        serde_json::to_vec(&json!([
            {"role": "user", "content": "workspace A secret"}
        ]))
        .expect("serialize legacy history"),
    )
    .expect("write legacy history");

    let mut command = Command::new(binary_path());
    command
        .env("HOME", &home)
        .current_dir(&process_workspace)
        .args(["--headless", "--workspace"])
        .arg(&requested_workspace)
        .args(["--resume", session_id, "must not see A"]);
    common::opt_out_telemetry(&mut command, &home);
    let output = command
        .output()
        .expect("attempt cross-workspace legacy resume");
    let messages = json_lines(&output);

    assert!(messages.iter().any(|message| {
        message["type"] == "result"
            && message["is_error"] == true
            && message["session_error_code"] == "not_found"
            && message["errors"][0]
                .as_str()
                .is_some_and(|error| error.contains("[not_found]"))
    }));
    assert!(legacy_file.exists());
}

#[test]
fn pre_workspace_default_session_is_discovered_and_migrated() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    let config_dir = home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(
        config_dir.join("config.toml"),
        r#"
[ai]
active_provider = "test"

[ai.providers.test]
type = "mock"
model = "mock-history"
"#,
    )
    .expect("write config");
    let session_id = "44444444-4444-4444-8444-444444444444";
    let legacy_dir = workspace.join("sessions");
    let legacy_file = legacy_dir.join(format!("{session_id}.json"));
    fs::create_dir_all(&legacy_dir).expect("create legacy session directory");
    fs::write(
        &legacy_file,
        serde_json::to_vec_pretty(&json!([
            {"role": "user", "content": "legacy default history"}
        ]))
        .expect("serialize legacy history"),
    )
    .expect("write legacy session");

    let mut command = Command::new(binary_path());
    command
        .env("HOME", &home)
        .current_dir(&workspace)
        .args(["--headless", "--workspace"])
        .arg(&workspace)
        .args(["--resume", session_id, "continue migrated history"]);
    common::opt_out_telemetry(&mut command, &home);
    let output = command.output().expect("resume pre-workspace session");
    let messages = json_lines(&output);
    let assistant = messages
        .iter()
        .find(|message| message["type"] == "assistant")
        .and_then(|message| message["message"]["content"][0]["text"].as_str())
        .expect("assistant response");
    let list = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 10
        }),
    );

    assert!(assistant.contains("legacy default history"), "{assistant}");
    assert!(
        assistant.contains("continue migrated history"),
        "{assistant}"
    );
    assert_eq!(list["data"]["sessions"][0]["session_id"], session_id);
    assert!(!legacy_file.exists());
}

#[test]
fn resume_explicit_model_overrides_persisted_model() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let first = json_lines(&run_prompt(&home, &workspace, &["remember model"]));
    let session_id = first
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("persisted session id");

    let resumed = json_lines(&run_protocol(
        &home,
        &workspace,
        &["--resume", session_id, "--model", "mock-explicit"],
        &[
            json!({
                "type": "control_request",
                "request_id": "init-1",
                "request": {"subtype": "initialize"}
            }),
            json!({
                "type": "control_request",
                "request_id": "shutdown-1",
                "request": {"subtype": "shutdown"}
            }),
        ],
    ));
    let init = resumed
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("resumed system init");

    assert_eq!(init["model"], "mock-explicit");
}

#[test]
fn resume_bounds_oversized_persisted_model_in_init() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let first = json_lines(&run_prompt(&home, &workspace, &["remember bounded model"]));
    let session_id = first
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("persisted session id");
    let scoped_dir = fs::read_dir(&store)
        .expect("read session root")
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .expect("workspace-scoped session directory")
        .path();
    let session_path = scoped_dir.join(format!("{session_id}.json"));
    let mut envelope: Value =
        serde_json::from_slice(&fs::read(&session_path).expect("read session"))
            .expect("session envelope");
    envelope["model"] = Value::String("oversized-model-".repeat(65_536));
    fs::write(
        &session_path,
        serde_json::to_vec(&envelope).expect("serialize oversized model"),
    )
    .expect("write oversized model");

    let resumed = json_lines(&run_protocol(
        &home,
        &workspace,
        &["--resume", session_id],
        &[
            json!({
                "type": "control_request",
                "request_id": "init-1",
                "request": {"subtype": "initialize"}
            }),
            json!({
                "type": "control_request",
                "request_id": "shutdown-1",
                "request": {"subtype": "shutdown"}
            }),
        ],
    ));
    let init = resumed
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("resumed system init");
    let model = init["model"].as_str().expect("bounded init model");

    assert!(
        model.len() <= 256,
        "init model not bounded: {}",
        model.len()
    );
    assert!(model.ends_with('…'), "{model}");
}

#[test]
fn authentication_selected_model_initializes_new_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    let (auth_base_url, auth_server) = authentication_model_server("gpt-selected");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    let config_dir = home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "unconfigured"

[ai.providers.unconfigured]
type = "generic"
model = "qwen-max"
base_url = "http://127.0.0.1:9/v1"

[session]
auto_persist = true
persist_dir = "{}"
"#,
            store.display()
        ),
    )
    .expect("write auth config");

    let messages = json_lines(&run_protocol(
        &home,
        &workspace,
        &[],
        &[
            json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": "auth-init",
                    "response": {
                        "behavior": "allow",
                        "provider_id": "selected",
                        "provider_type": "openai_compat",
                        "values": {
                            "api_key": "test-key",
                            "base_url": auth_base_url,
                            "model": "gpt-selected"
                        },
                        "persist": false
                    }
                }
            }),
            json!({
                "type": "control_request",
                "request_id": "init-1",
                "request": {"subtype": "initialize"}
            }),
            json!({
                "type": "control_request",
                "request_id": "shutdown-1",
                "request": {"subtype": "shutdown"}
            }),
        ],
    ));
    auth_server.join().expect("auth model server");
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("authenticated system init");

    assert_eq!(init["model"], "gpt-selected");
}

#[test]
fn management_mode_lists_validates_and_protects_without_provider_auth() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let first = json_lines(&run_prompt(&home, &workspace, &["persist me"]));
    let session_id = first
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("session id");

    let list = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 10
        }),
    );
    assert_eq!(list["ok"], true);
    assert_eq!(list["data"]["sessions"][0]["session_id"], session_id);

    let plan = session_control(
        &home,
        json!({
            "action": "prepare_clear_all",
            "workspace_scope": workspace,
            "protected_session_ids": [session_id]
        }),
    );
    assert_eq!(plan["ok"], true);
    assert!(plan["data"]["session_ids"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(plan["data"]["protected_session_ids"][0], session_id);

    let validate = session_control(
        &home,
        json!({
            "action": "validate",
            "workspace_scope": workspace,
            "session_id": session_id
        }),
    );
    assert_eq!(validate["ok"], true);
    assert_eq!(validate["data"]["session"]["health"], "ready");

    let clear = session_control(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": [session_id],
            "protected_session_ids": [session_id]
        }),
    );
    assert_eq!(clear["ok"], true);
    assert!(clear["data"]["deleted"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(
        clear["data"]["skipped"][0]["error"]["code"],
        "active_session"
    );
}

#[cfg(unix)]
#[test]
fn management_clear_rejects_cross_workspace_scoped_directory_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let first_workspace = temp.path().join("first-workspace");
    let second_workspace = temp.path().join("second-workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&first_workspace).expect("create first workspace");
    fs::create_dir_all(&second_workspace).expect("create second workspace");
    configure(&home, &store);

    let first = json_lines(&run_prompt(&home, &first_workspace, &["first persisted"]));
    let first_id = first
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("first session id");
    let _ = json_lines(&run_prompt(&home, &second_workspace, &["second persisted"]));

    let first_scope = first_workspace
        .canonicalize()
        .expect("canonical first workspace");
    let mut scoped_directories = fs::read_dir(&store)
        .expect("session root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    scoped_directories.sort();
    let first_directory = scoped_directories
        .iter()
        .find(|directory| {
            fs::read_dir(directory)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|value| value == "json")
                })
                .filter_map(|entry| fs::read(entry.path()).ok())
                .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .any(|session| session["workspace_scope"] == first_scope.to_string_lossy().as_ref())
        })
        .expect("first scoped directory")
        .clone();
    let second_directory = scoped_directories
        .into_iter()
        .find(|directory| directory != &first_directory)
        .expect("second scoped directory");
    fs::remove_dir_all(&second_directory).expect("remove second scoped directory");
    std::os::unix::fs::symlink(&first_directory, &second_directory)
        .expect("redirect second scope to first");

    let clear = session_control(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": second_workspace,
            "session_ids": [first_id],
            "protected_session_ids": []
        }),
    );

    assert_eq!(clear["ok"], false);
    assert_eq!(clear["error"]["code"], "io");
    assert!(first_directory.join(format!("{first_id}.json")).exists());
}

#[test]
fn management_clear_requires_an_explicit_protection_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);
    let messages = json_lines(&run_prompt(&home, &workspace, &["keep protected"]));
    let session_id = messages
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("session id");

    for request in [
        json!({
            "action": "prepare_clear_all",
            "workspace_scope": workspace
        }),
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": [session_id]
        }),
    ] {
        let response = session_control(&home, request);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "invalid_request");
    }

    let validate = session_control(
        &home,
        json!({
            "action": "validate",
            "workspace_scope": workspace,
            "session_id": session_id
        }),
    );
    assert_eq!(validate["ok"], true);
}

#[test]
fn management_rejects_oversized_or_malformed_input_as_invalid_request() {
    const REQUEST_LIMIT: usize = 1024 * 1024;

    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    let oversized = vec![b' '; REQUEST_LIMIT + 1];

    for (label, input) in [
        ("oversized", oversized.as_slice()),
        ("malformed JSON", b"{" as &[u8]),
        ("invalid UTF-8", &[0xff_u8, 0xfe]),
    ] {
        let output = session_control_bytes_output(&home, input);
        assert!(!output.status.success(), "{label}: {output:?}");
        let response: Value =
            serde_json::from_slice(&output.stdout).expect("invalid request response");
        assert_eq!(
            response["error"]["code"], "invalid_request",
            "{label}: {response}"
        );
    }
}

#[test]
fn management_clear_all_includes_workspace_owned_legacy_sessions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let legacy_dir = workspace.join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&legacy_dir).expect("create legacy directory");
    configure_mock_provider(&home);
    let valid_id = "11111111-1111-4111-8111-111111111111";
    let corrupt_id = "22222222-2222-4222-8222-222222222222";
    fs::write(
        legacy_dir.join(format!("{valid_id}.json")),
        serde_json::to_vec(&vec![json!({"role":"user","content":"legacy"})])
            .expect("serialize legacy"),
    )
    .expect("write valid legacy session");
    fs::write(
        legacy_dir.join(format!("{corrupt_id}.json")),
        b"{broken legacy",
    )
    .expect("write corrupt legacy session");

    let plan = session_control(
        &home,
        json!({
            "action": "prepare_clear_all",
            "workspace_scope": workspace,
            "protected_session_ids": [valid_id]
        }),
    );
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["data"]["session_ids"], json!([corrupt_id]));
    assert_eq!(plan["data"]["protected_session_ids"], json!([valid_id]));

    let clear = session_control(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": [valid_id, corrupt_id],
            "protected_session_ids": [valid_id]
        }),
    );
    assert_eq!(clear["ok"], true);
    assert_eq!(clear["data"]["deleted"], json!([corrupt_id]));
    assert_eq!(
        clear["data"]["skipped"][0]["error"]["code"],
        "active_session"
    );
    assert!(legacy_dir.join(format!("{valid_id}.json")).exists());
    assert!(!legacy_dir.join(format!("{corrupt_id}.json")).exists());
}

#[test]
fn management_clear_all_pages_stay_below_shell_output_limit() {
    const SESSION_COUNT: usize = 27_000;
    const SHELL_STDOUT_LIMIT: usize = 1024 * 1024;

    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);
    let _ = json_lines(&run_prompt(&home, &workspace, &["create scoped directory"]));
    let scoped_dir = fs::read_dir(&store)
        .expect("read store")
        .find_map(|entry| {
            let entry = entry.ok()?;
            entry.file_type().ok()?.is_dir().then_some(entry.path())
        })
        .expect("workspace-scoped directory");
    for entry in fs::read_dir(&scoped_dir).expect("read scoped directory") {
        let path = entry.expect("scoped entry").path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            fs::remove_file(path).expect("remove seed session");
        }
    }
    for index in 0..SESSION_COUNT {
        let session_id = format!("00000000-0000-4000-8000-{index:012x}");
        fs::write(scoped_dir.join(format!("{session_id}.json")), b"{}")
            .expect("write session placeholder");
    }

    let mut cursor = None;
    let mut planned = 0;
    loop {
        let output = session_control_output(
            &home,
            json!({
                "action": "prepare_clear_all",
                "workspace_scope": workspace,
                "protected_session_ids": [],
                "limit": 4096,
                "cursor": cursor
            }),
        );
        assert!(output.status.success(), "{output:?}");
        assert!(
            output.stdout.len() < SHELL_STDOUT_LIMIT,
            "clear-all page exceeded shell output limit: {}",
            output.stdout.len()
        );
        let response: Value =
            serde_json::from_slice(&output.stdout).expect("clear-all page response");
        planned += response["data"]["session_ids"]
            .as_array()
            .map_or(0, Vec::len);
        cursor = response["data"]["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(planned, SESSION_COUNT);

    let unpaged = session_control(
        &home,
        json!({
            "action": "prepare_clear_all",
            "workspace_scope": workspace,
            "protected_session_ids": []
        }),
    );
    assert_eq!(unpaged["ok"], false);
    assert_eq!(unpaged["error"]["code"], "invalid_request");

    let oversized_clear = (0..129)
        .map(|index| format!("00000000-0000-4000-8000-{index:012x}"))
        .collect::<Vec<_>>();
    let response = session_control(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": oversized_clear,
            "protected_session_ids": []
        }),
    );
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "invalid_request");

    let output = session_control_output(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": ["x".repeat(256 * 1024)],
            "protected_session_ids": []
        }),
    );
    assert!(output.status.success());
    assert!(output.stdout.len() < SHELL_STDOUT_LIMIT);
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("bounded invalid-ID response");
    assert_eq!(
        response["data"]["skipped"][0]["error"]["code"],
        "invalid_id"
    );
    assert!(response["data"]["skipped"][0]["session_id"]
        .as_str()
        .is_some_and(|value| value.len() <= 128));

    let emoji_ids = (0..128)
        .map(|index| format!("{index}{}", "😀".repeat(1_500)))
        .collect::<Vec<_>>();
    let output = session_control_output(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": emoji_ids,
            "protected_session_ids": []
        }),
    );
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.len() < SHELL_STDOUT_LIMIT,
        "multi-byte clear response exceeded shell limit: {}",
        output.stdout.len()
    );
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("multi-byte invalid-ID response");
    assert_eq!(
        response["data"]["skipped"].as_array().map(Vec::len),
        Some(128)
    );
    assert!(response["data"]["skipped"]
        .as_array()
        .is_some_and(|failures| failures.iter().all(|failure| {
            failure["session_id"]
                .as_str()
                .is_some_and(|value| value.len() <= 128)
                && failure["error"]["message"]
                    .as_str()
                    .is_some_and(|value| value.len() <= 2048)
        })));
}

#[test]
fn management_list_cursor_survives_deleted_previous_page() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let first = json_lines(&run_prompt(&home, &workspace, &["first persisted"]));
    let first_id = first
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("first session id");
    let second = json_lines(&run_prompt(&home, &workspace, &["second persisted"]));
    let second_id = second
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("second session id");

    let page = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 1
        }),
    );
    let listed_id = page["data"]["sessions"][0]["session_id"]
        .as_str()
        .expect("first page ID");
    let cursor = page["data"]["next_cursor"].as_str().expect("stable cursor");
    let clear = session_control(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": [listed_id],
            "protected_session_ids": []
        }),
    );
    assert_eq!(clear["data"]["deleted"][0], listed_id);

    let next = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 1,
            "cursor": cursor
        }),
    );
    let remaining_id = if listed_id == first_id {
        second_id
    } else {
        first_id
    };

    assert_eq!(next["ok"], true);
    assert_eq!(next["data"]["sessions"][0]["session_id"], remaining_id);
    assert!(next["data"]["next_cursor"].is_null());
}

#[test]
fn management_list_skips_unreadable_entry_without_hiding_healthy_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let persisted = json_lines(&run_prompt(&home, &workspace, &["healthy persisted"]));
    let healthy_id = persisted
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("healthy session id");
    let scope_dir = fs::read_dir(&store)
        .expect("session store")
        .next()
        .expect("workspace store")
        .expect("workspace entry")
        .path();
    fs::create_dir(scope_dir.join("11111111-1111-4111-8111-111111111111.json"))
        .expect("unreadable summary entry");

    let list = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 10
        }),
    );

    assert_eq!(list["ok"], true);
    assert_eq!(list["data"]["sessions"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["data"]["sessions"][0]["session_id"], healthy_id);
}

#[test]
fn management_list_bounds_prompt_preview_on_wire() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);
    let prompt = "界".repeat(10_000);

    let persisted = json_lines(&run_prompt(&home, &workspace, &[&prompt]));
    assert!(persisted.iter().any(|message| message["type"] == "result"));
    let list = session_control(
        &home,
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 10
        }),
    );
    let preview = list["data"]["sessions"][0]["first_prompt"]
        .as_str()
        .expect("bounded preview");

    assert_eq!(preview.chars().count(), 160);
    assert!(preview.ends_with('…'));
    assert!(
        serde_json::to_vec(&list).expect("list response").len() < 2_000,
        "management response carried the original prompt"
    );
}

#[test]
fn management_summaries_bound_oversized_model_metadata_on_wire() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let persisted = json_lines(&run_prompt(&home, &workspace, &["bounded model"]));
    let session_id = persisted
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("persisted session ID");
    let scoped_dir = fs::read_dir(&store)
        .expect("read store root")
        .next()
        .expect("workspace store directory")
        .expect("workspace store entry")
        .path();
    let session_path = scoped_dir.join(format!("{session_id}.json"));
    let mut envelope: Value =
        serde_json::from_slice(&fs::read(&session_path).expect("read session"))
            .expect("session envelope");
    envelope["model"] = Value::String("🧠".repeat(300_000));
    fs::write(
        &session_path,
        serde_json::to_vec(&envelope).expect("serialize oversized model"),
    )
    .expect("write oversized model");

    for request in [
        json!({
            "action": "list",
            "workspace_scope": workspace,
            "limit": 1
        }),
        json!({
            "action": "inspect",
            "workspace_scope": workspace,
            "session_id": session_id
        }),
        json!({
            "action": "validate",
            "workspace_scope": workspace,
            "session_id": session_id
        }),
    ] {
        let output = session_control_output(&home, request);
        assert!(output.status.success());
        assert!(output.stdout.len() < 1_048_576);
        let response: Value =
            serde_json::from_slice(&output.stdout).expect("bounded management response");
        assert_eq!(response["ok"], true, "{response}");
        let summary = response["data"]
            .get("session")
            .or_else(|| response["data"]["sessions"].get(0))
            .expect("session summary");
        let model = summary["model"].as_str().expect("bounded model");
        assert!(model.len() <= 256);
        assert!(model.ends_with('…'));
    }
}

#[test]
fn management_clear_in_empty_workspace_reports_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);

    let session_id = "00000000-0000-4000-8000-000000000000";
    let clear = session_control(
        &home,
        json!({
            "action": "clear",
            "workspace_scope": workspace,
            "session_ids": [session_id],
            "protected_session_ids": []
        }),
    );

    assert_eq!(clear["ok"], true);
    assert!(clear["data"]["deleted"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(clear["data"]["skipped"][0]["session_id"], session_id);
    assert_eq!(clear["data"]["skipped"][0]["error"]["code"], "not_found");
    assert!(clear["data"]["skipped"][0]["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("session not found")));
}

#[test]
fn recoverable_provider_error_still_persists_mutated_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    configure(&home, &store);
    let config_path = home.join(".copilot-shell/config.toml");
    let config = fs::read_to_string(&config_path)
        .expect("read config")
        .replace("model = \"mock-history\"", "model = \"mock-partial-error\"");
    fs::write(&config_path, config).expect("write partial error config");

    let output = run_prompt(&home, &workspace, &["persist despite error"]);
    let messages = json_lines(&output);
    let result = messages
        .iter()
        .find(|message| message["type"] == "result")
        .expect("error result");
    assert_eq!(result["is_error"], true);
    let session_id = result["session_id"].as_str().expect("session id");

    let validate = session_control(
        &home,
        json!({
            "action": "validate",
            "workspace_scope": workspace,
            "session_id": session_id
        }),
    );
    assert_eq!(validate["ok"], true);
    assert!(
        validate["data"]["session"]["message_count"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{validate}"
    );
}

fn session_control(home: &Path, request: Value) -> Value {
    let output = session_control_output(home, request);
    serde_json::from_slice(&output.stdout).expect("management response")
}

fn session_control_output(home: &Path, request: Value) -> Output {
    let request = serde_json::to_vec(&request).expect("serialize management request");
    session_control_bytes_output(home, &request)
}

fn session_control_bytes_output(home: &Path, request: &[u8]) -> Output {
    let mut command = Command::new(binary_path());
    command.env("HOME", home).arg("--session-control");
    common::opt_out_telemetry(&mut command, home);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn management mode");
    let mut stdin = child.stdin.take().expect("management stdin");
    if let Err(error) = stdin.write_all(request) {
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "write management request: {error}"
        );
    }
    drop(stdin);
    child.wait_with_output().expect("wait management mode")
}
