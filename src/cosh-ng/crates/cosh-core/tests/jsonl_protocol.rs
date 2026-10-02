use std::io::Write;
use std::process::Stdio;

use serde_json::Value;

mod common;

fn private_wire_corpus() -> Value {
    serde_json::from_str(include_str!("fixtures/cosh-private-wire-dual-version.json"))
        .expect("valid private wire corpus")
}

fn json_line(value: &Value) -> String {
    serde_json::to_string(value).expect("serializable private wire fixture")
}

fn run_with_input(lines: &[&str]) -> Vec<Value> {
    let home = tempfile::tempdir().expect("temp home");
    run_with_input_at_home(home.path(), lines)
}

fn run_with_input_at_home(home: &std::path::Path, lines: &[&str]) -> Vec<Value> {
    run_with_input_at_home_args(home, &[], lines)
}

fn run_with_input_at_home_args(
    home: &std::path::Path,
    args: &[&str],
    lines: &[&str],
) -> Vec<Value> {
    let output = run_process_at_home_args(home, args, lines);
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("bad JSON: {e}: {l}")))
        .collect()
}

fn run_process_at_home(home: &std::path::Path, lines: &[&str]) -> std::process::Output {
    run_process_at_home_args(home, &[], lines)
}

fn run_process_at_home_args(
    home: &std::path::Path,
    args: &[&str],
    lines: &[&str],
) -> std::process::Output {
    let mut command = common::cosh_core_command(home);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to spawn {}: {e}", common::binary_path().display()));

    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in lines {
            writeln!(stdin, "{line}").unwrap();
        }
    }

    child.wait_with_output().unwrap()
}

#[test]
fn generic_headless_ignores_untrusted_raw_user_input_for_hooks() {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[hooks]
enabled = true

[[hooks.UserPromptSubmit]]
command = '''python3 -c 'import json,sys; print(json.dumps({"system_message": json.load(sys.stdin)["prompt"]}))' '''
name = "capture-prompt"
"#,
    )
    .unwrap();

    let messages = run_with_input_at_home(
        home.path(),
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"provider envelope: run the reviewed command","raw_user_input":"benign shell text"},"parent_tool_use_id":null}"#,
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
        ],
    );
    let hook = messages
        .iter()
        .find(|message| {
            message["type"] == "system"
                && message["subtype"] == "hook_notification"
                && message["hook_name"] == "capture-prompt"
        })
        .expect("UserPromptSubmit hook notification");
    assert_eq!(
        hook["status"],
        "provider envelope: run the reviewed command"
    );

    let trusted_messages = run_with_input_at_home_args(
        home.path(),
        &["--cosh-shell-transport"],
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"provider envelope: run the reviewed command","raw_user_input":"benign shell text"},"parent_tool_use_id":null}"#,
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
        ],
    );
    let trusted_hook = trusted_messages
        .iter()
        .find(|message| {
            message["type"] == "system"
                && message["subtype"] == "hook_notification"
                && message["hook_name"] == "capture-prompt"
        })
        .expect("trusted UserPromptSubmit hook notification");
    assert_eq!(trusted_hook["status"], "benign shell text");
}

#[test]
fn initialize_can_skip_session_start_hooks_for_one_shot_transport() {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[hooks]
enabled = true

[[hooks.SessionStart]]
command = "echo '{\"system_message\":\"session-start-ran\"}'"
name = "session-start"
"#,
    )
    .unwrap();

    let generic_messages = run_with_input_at_home(
        home.path(),
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","fire_session_start":false}}"#,
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
        ],
    );
    assert!(generic_messages.iter().any(|message| {
        message["type"] == "system"
            && message["subtype"] == "hook_notification"
            && message["hook_name"] == "session-start"
    }));

    let trusted_messages = run_with_input_at_home_args(
        home.path(),
        &["--cosh-shell-transport"],
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","fire_session_start":false}}"#,
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
        ],
    );
    assert!(!trusted_messages.iter().any(|message| {
        message["type"] == "system"
            && message["subtype"] == "hook_notification"
            && message["hook_name"] == "session-start"
    }));
}

#[test]
fn initialize_returns_system_init() {
    let msgs = run_with_input(&[
        r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    assert!(!msgs.is_empty(), "expected at least one output message");
    let capability = msgs
        .iter()
        .find(|m| m["type"] == "control_response")
        .expect("initialize capability response");
    assert_eq!(
        capability["response"]["response"]["capabilities"]
            ["can_handle_host_executed_shell_tool_result"],
        true
    );
    assert_eq!(capability["response"]["response"]["protocol_version"], 1);

    let init = msgs
        .iter()
        .find(|m| m["type"] == "system" && m["subtype"] == "init")
        .expect("system init");
    assert!(init["session_id"].is_string());
    assert!(init["model"].is_string());
    assert!(init["tools"].is_array());
}

#[test]
fn versioned_initialize_returns_negotiated_version() {
    let corpus = private_wire_corpus();
    let initialize = json_line(&corpus["legacy_v1"]["initialize_request"]);
    let msgs = run_with_input(&[
        &initialize,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    let response = msgs
        .iter()
        .find(|message| message["type"] == "control_response")
        .expect("initialize response");
    assert_eq!(response, &corpus["legacy_v1"]["initialize_ack"]);
    assert!(msgs
        .iter()
        .any(|message| message["type"] == "system" && message["subtype"] == "init"));
}

#[test]
fn gateway_brokered_profile_acks_v3_without_initializing_local_runtime() {
    let corpus = private_wire_corpus();
    let initialize = json_line(&corpus["gateway_brokered_v3"]["initialize_request"]);
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let hook_marker = home.path().join("hook-ran");
    let mcp_marker = home.path().join("mcp-ran");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"

[agent]
approval_mode = "trust"
allowed_tools = ["shell", "write_file"]

[hooks]
enabled = true

[[hooks.SessionStart]]
command = "touch {}"
name = "must-not-run"

[mcp.servers.must_not_start]
command = "sh"
args = ["-c", "touch {}; exit 1"]
startup_timeout_ms = 1000
"#,
            hook_marker.display(),
            mcp_marker.display()
        ),
    )
    .unwrap();

    let output = run_process_at_home_args(
        home.path(),
        &[
            "--headless",
            "--execution-profile",
            "gateway-brokered-v1",
            "--workspace",
            workspace.path().to_str().unwrap(),
        ],
        &[
            &initialize,
            r#"{"type":"control_request","request_id":"shutdown","request":{"subtype":"shutdown"}}"#,
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let response = messages
        .iter()
        .find(|message| message["type"] == "control_response")
        .expect("profile acknowledgement");
    assert_eq!(response, &corpus["gateway_brokered_v3"]["initialize_ack"]);
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");
    assert_eq!(init["tools"], serde_json::json!(["ask_user_question"]));
    assert!(response["response"]["response"]["capabilities"]
        .get("can_handle_hosted_checkpoint_create")
        .is_none());
    assert!(!messages
        .iter()
        .any(|message| message["hook_name"] == "must-not-run"));
    assert!(!hook_marker.exists());
    assert!(!mcp_marker.exists());
}

#[test]
fn checkpoint_brokered_profile_acks_exact_v4_inventory() {
    let corpus = private_wire_corpus();
    let initialize = json_line(&corpus["gateway_brokered_checkpoint_v4"]["initialize_request"]);
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let output = run_process_at_home_args(
        home.path(),
        &[
            "--headless",
            "--execution-profile",
            "gateway-brokered-checkpoint-v1",
            "--workspace",
            workspace.path().to_str().unwrap(),
        ],
        &[
            &initialize,
            r#"{"type":"control_request","request_id":"shutdown","request":{"subtype":"shutdown"}}"#,
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let response = messages
        .iter()
        .find(|message| message["type"] == "control_response")
        .expect("profile acknowledgement");
    assert_eq!(
        response,
        &corpus["gateway_brokered_checkpoint_v4"]["initialize_ack"]
    );
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");
    assert_eq!(
        init["tools"],
        serde_json::json!(["ask_user_question", "workspace_checkpoint_create"])
    );
}

#[test]
fn workspace_write_brokered_profile_acks_exact_v5_inventory() {
    let corpus = private_wire_corpus();
    let initialize =
        json_line(&corpus["gateway_brokered_workspace_write_v5"]["initialize_request"]);
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let output = run_process_at_home_args(
        home.path(),
        &[
            "--headless",
            "--execution-profile",
            "gateway-brokered-workspace-write-v1",
            "--workspace",
            workspace.path().to_str().unwrap(),
        ],
        &[
            &initialize,
            r#"{"type":"control_request","request_id":"shutdown","request":{"subtype":"shutdown"}}"#,
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let response = messages
        .iter()
        .find(|message| message["type"] == "control_response")
        .expect("profile acknowledgement");
    assert_eq!(
        response,
        &corpus["gateway_brokered_workspace_write_v5"]["initialize_ack"]
    );
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");
    assert_eq!(
        init["tools"],
        serde_json::json!(["ask_user_question", "write_file"])
    );
}

#[test]
fn checkpoint_brokered_profile_rejects_v3_identity_without_fallback() {
    let corpus = private_wire_corpus();
    let initialize = json_line(&corpus["gateway_brokered_v3"]["initialize_request"]);
    let home = tempfile::tempdir().expect("temp home");
    let output = run_process_at_home_args(
        home.path(),
        &[
            "--headless",
            "--execution-profile",
            "gateway-brokered-checkpoint-v1",
        ],
        &[&initialize],
    );

    assert_eq!(output.status.code(), Some(65));
    let messages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let response = messages
        .iter()
        .find(|message| message["type"] == "control_response")
        .expect("profile error");
    assert_eq!(response["response"]["subtype"], "error");
    assert_eq!(response["response"]["response"]["protocol_version"], 4);
    assert_eq!(
        response["response"]["response"]["execution_profile"],
        "gateway_brokered_checkpoint_v1"
    );
    assert!(!messages
        .iter()
        .any(|message| message["type"] == "system" && message["subtype"] == "init"));
}

#[test]
fn gateway_brokered_profile_rejects_legacy_or_missing_ack_without_fallback() {
    let corpus = private_wire_corpus();
    let invalid = corpus["gateway_brokered_v3"]["invalid_initialize_requests"]
        .as_object()
        .expect("invalid initialize request map");
    for (case, initialize) in invalid {
        let home = tempfile::tempdir().expect("temp home");
        let initialize = json_line(initialize);
        let output = run_process_at_home_args(
            home.path(),
            &["--headless", "--execution-profile", "gateway-brokered-v1"],
            &[&initialize],
        );
        assert_eq!(output.status.code(), Some(65), "case {case}");
        let messages = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let response = messages
            .iter()
            .find(|message| message["type"] == "control_response")
            .unwrap_or_else(|| panic!("profile error for {case}"));
        assert_eq!(response["response"]["subtype"], "error");
        assert_eq!(response["response"]["response"]["protocol_version"], 3);
        assert_eq!(
            response["response"]["response"]["execution_profile"],
            "gateway_brokered_v1"
        );
        assert!(!messages
            .iter()
            .any(|message| message["type"] == "system" && message["subtype"] == "init"));
    }
}

#[test]
fn gateway_brokered_profile_rejects_runtime_and_registry_mutation() {
    let corpus = private_wire_corpus();
    let initialize = json_line(&corpus["gateway_brokered_v3"]["initialize_request"]);
    let home = tempfile::tempdir().expect("temp home");
    let output = run_process_at_home_args(
        home.path(),
        &["--headless", "--execution-profile", "gateway-brokered-v1"],
        &[
            &initialize,
            r#"{"type":"control_request","request_id":"config","request":{"subtype":"config_override","approval_mode":"trust","allowed_tools":["shell"]}}"#,
            r#"{"type":"control_request","request_id":"reload","request":{"subtype":"reload_config"}}"#,
            r#"{"type":"registry_request","request_id":"registry","domain":"extensions","action":"install","params":{}}"#,
            r#"{"type":"control_request","request_id":"shutdown","request":{"subtype":"shutdown"}}"#,
        ],
    );
    assert!(output.status.success());
    let messages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["error_code"] == "BrokeredProfileViolation")
            .count(),
        2
    );
    let registry = messages
        .iter()
        .find(|message| message["type"] == "registry_response")
        .expect("registry rejection");
    assert_eq!(registry["success"], false);
    assert!(registry["error"]
        .as_str()
        .unwrap()
        .contains("disabled by the gateway brokered profile"));
}

#[test]
fn unsupported_initialize_version_fails_loud() {
    let home = tempfile::tempdir().expect("temp home");
    let output = run_process_at_home(
        home.path(),
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","protocol_version":9}}"#,
            r#"{"type":"user","message":{"role":"user","content":"must not run"}}"#,
        ],
    );
    assert_eq!(output.status.code(), Some(65));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let messages = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid JSONL"))
        .collect::<Vec<_>>();
    let response = messages
        .iter()
        .find(|message| message["type"] == "control_response")
        .expect("version error response");
    assert_eq!(response["response"]["subtype"], "error");
    assert_eq!(response["response"]["response"]["protocol_version"], 1);
    assert!(response["response"]["response"]["error"]
        .as_str()
        .unwrap()
        .contains("unsupported control protocol version 9"));
    assert!(!messages
        .iter()
        .any(|message| message["type"] == "system" && message["subtype"] == "init"));
    assert!(!messages
        .iter()
        .any(|message| matches!(message["type"].as_str(), Some("assistant" | "result"))));
}

#[test]
fn initial_extension_session_hook_is_registered_once() {
    let home = tempfile::tempdir().expect("temp home");
    let extension = home
        .path()
        .join(".copilot-shell/extensions/example.initial-hook");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "name": "example.initial-hook",
            "version": "1.0.0",
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "echo '{\"system_message\":\"initial hook\"}'",
                        "name": "initial-hook"
                    }]
                }]
            }
        }"#,
    )
    .unwrap();

    let messages = run_with_input_at_home(
        home.path(),
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
        ],
    );
    let notifications = messages
        .iter()
        .filter(|message| {
            message["type"] == "system"
                && message["subtype"] == "hook_notification"
                && message["hook_name"] == "initial-hook"
        })
        .count();

    assert_eq!(notifications, 1, "{messages:?}");
}

#[test]
fn user_message_returns_assistant_and_result() {
    let msgs = run_with_input(&[
        r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
        r#"{"type":"user","message":{"role":"user","content":"hello"},"parent_tool_use_id":null}"#,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    assert!(
        msgs.len() >= 2,
        "expected at least 2 messages, got {}",
        msgs.len()
    );

    assert!(
        msgs.iter()
            .any(|m| m["type"] == "system" && m["subtype"] == "init"),
        "expected system init"
    );

    let has_result = msgs.iter().any(|m| m["type"] == "result");
    assert!(has_result, "expected a result message");

    let init = msgs
        .iter()
        .find(|m| m["type"] == "system" && m["subtype"] == "init")
        .unwrap();
    let result = msgs.iter().find(|m| m["type"] == "result").unwrap();
    assert_eq!(result["session_id"], init["session_id"]);
}

#[test]
fn user_message_cannot_replace_initialized_session_id() {
    let msgs = run_with_input(&[
        r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
        r#"{"type":"user","message":{"role":"user","content":"hello"},"session_id":"default","parent_tool_use_id":null}"#,
        r#"{"type":"user","message":{"role":"user","content":"replace"},"session_id":"00000000-0000-4000-8000-000000000000","parent_tool_use_id":null}"#,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    let init = msgs
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");
    let results = msgs
        .iter()
        .filter(|message| message["type"] == "result")
        .collect::<Vec<_>>();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["session_id"], init["session_id"]);
    assert_eq!(results[1]["session_id"], init["session_id"]);
    assert_eq!(results[1]["is_error"], true);
    assert!(results[1]["result"]
        .as_str()
        .is_some_and(|value| value.contains("session identity conflict")));
}

#[test]
fn shutdown_terminates_process() {
    let msgs = run_with_input(&[
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    assert!(msgs.is_empty() || msgs.iter().all(|m| m["type"] != "result"));
}

#[test]
fn output_format_matches_cosh_shell_expectations() {
    let msgs = run_with_input(&[
        r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    let init = msgs
        .iter()
        .find(|m| m["type"] == "system" && m["subtype"] == "init")
        .expect("system init");

    assert!(
        init.get("session_id").is_some(),
        "system init must have top-level session_id"
    );
    assert!(
        init.get("model").is_some(),
        "system init must have top-level model"
    );
    assert!(
        init.get("tools").is_some(),
        "system init must have top-level tools"
    );
    assert_eq!(init.get("type").unwrap().as_str().unwrap(), "system");
    assert_eq!(init.get("subtype").unwrap().as_str().unwrap(), "init");
}

#[test]
fn invalid_jsonl_input_returns_error_and_fails() {
    let home = tempfile::tempdir().expect("temp home");
    let mut child = common::cosh_core_command(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cosh-core");

    const SECRET_INPUT: &str = "token=must-not-echo";
    writeln!(child.stdin.as_mut().expect("stdin"), "{SECRET_INPUT}").expect("write invalid input");
    let output = child.wait_with_output().expect("wait for cosh-core");
    assert!(!output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(SECRET_INPUT),
        "invalid input must not be echoed"
    );
    let messages = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("valid JSONL output"))
        .collect::<Vec<_>>();
    let error = messages
        .iter()
        .find(|message| message["type"] == "result" && message["is_error"] == true)
        .expect("invalid input error result");
    assert_eq!(error["subtype"], "error");
    assert_eq!(error["error_code"], "InvalidJsonlInput");
    assert_eq!(error["errors"][0], "failed to parse stdin line as JSON");
}

#[test]
fn headless_registry_reload_publishes_into_the_live_generation() {
    let msgs = run_with_input(&[
        r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
        r#"{"type":"registry_request","request_id":"reg-1","domain":"extensions","action":"reload","params":null}"#,
        r#"{"type":"registry_request","request_id":"reg-2","domain":"extensions","action":"reload","params":null}"#,
        r#"{"type":"registry_request","request_id":"reg-3","domain":"extensions","action":"doctor","params":null}"#,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    ]);

    let responses = msgs
        .iter()
        .filter(|message| message["type"] == "registry_response")
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3, "{msgs:?}");
    assert_eq!(responses[0]["success"], true, "{:?}", responses[0]);
    assert_eq!(responses[0]["data"]["activation"], "immediate");
    assert_eq!(responses[0]["data"]["pending"], false);
    let first_generation = responses[0]["data"]["generation"].as_u64().unwrap();
    let second_generation = responses[1]["data"]["generation"].as_u64().unwrap();
    assert_eq!(second_generation, first_generation + 1);
    assert_eq!(
        responses[2]["data"]["runtime"]["generation"],
        second_generation
    );
    assert_eq!(responses[2]["data"]["runtime"]["healthy"], true);
    assert!(responses[2]["data"]["runtime"]["mcp_servers"].is_array());
    assert!(responses[2]["data"]["runtime"]["agents"].is_array());
}

#[test]
fn headless_extension_info_reports_current_runtime_projection() {
    let home = tempfile::tempdir().expect("temp home");
    let extension = home
        .path()
        .join(".copilot-shell/extensions/example.runtime");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("cosh-extension.json"),
        r#"{
            "schemaVersion": 1,
            "name": "example.runtime",
            "version": "1.0.0",
            "compatibility": {"cosh": ">=0.12.0"}
        }"#,
    )
    .unwrap();
    let msgs = run_with_input_at_home(
        home.path(),
        &[
            r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
            r#"{"type":"registry_request","request_id":"reg-info","domain":"extensions","action":"info","params":{"name":"example.runtime"}}"#,
            r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
        ],
    );
    let info = msgs
        .iter()
        .find(|message| message["request_id"] == "reg-info")
        .expect("live extension info response");
    assert_eq!(info["success"], true, "{info}");
    assert_eq!(info["data"]["activation"], "current");
    assert_eq!(info["data"]["effective_state"], "enabled");
    assert_eq!(info["data"]["is_active"], true);
    assert!(info["data"]["runtime"]["generation"].is_number());
    assert_eq!(info["data"]["runtime"]["healthy"], true);
    assert!(info["data"]["runtime"]["mcp_servers"].is_array());
    assert!(info["data"]["runtime"]["agents"].is_array());
}
