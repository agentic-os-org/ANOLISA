//! Two-entry settlement parity over the real binary.
//!
//! The legacy JSONL entry and the official ACP entry run against one fixture and
//! must commit the same transcript through the same finalizer. The durable
//! envelope is read back from disk, and the legacy resume path proves an
//! existing recovery reader can consume a record the ACP entry wrote.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, InitializeRequest, NewSessionRequest, PermissionOptionKind,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, StopReason, TextContent,
};
use agent_client_protocol::{on_receive_notification, AcpAgent, AcpAgentConfig, Client};
use serde_json::Value;

fn configure(home: &Path, store: &Path, model: &str) {
    let directory = home.join(".copilot-shell");
    fs::create_dir_all(&directory).expect("config directory");
    fs::write(
        directory.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"

[ai.providers.test]
type = "mock"
model = "{model}"

[session]
auto_persist = true
persist_dir = "{}"
"#,
            store.display()
        ),
    )
    .expect("write config");
}

fn legacy_turn(home: &Path, workspace: &Path, args: &[&str]) -> Output {
    let mut command = common::cosh_core_command(home);
    command
        .args(["--headless", "--workspace"])
        .arg(workspace)
        .args(args)
        .env_remove("COSH_MODEL")
        .env_remove("COSH_AI_PROVIDER")
        .env_remove("OPENAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().expect("run cosh-core --headless")
}

fn json_lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSONL"))
        .collect()
}

fn result_line(lines: &[Value]) -> &Value {
    lines
        .iter()
        .find(|line| line["type"] == "result")
        .expect("a terminal result line")
}

/// Every durable envelope under the store, keyed by session id.
fn envelopes(store: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for scope in fs::read_dir(store).expect("session store root") {
        let scope = scope.expect("scope entry").path();
        if !scope.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&scope).expect("scope directory") {
            let path = entry.expect("envelope entry").path();
            let name = path.file_name().expect("file name").to_string_lossy();
            if let Some(id) = name.strip_suffix(".json") {
                found.push((id.to_string(), path));
            }
        }
    }
    found.sort();
    found
}

fn envelope(store: &Path, session_id: &str) -> Value {
    let matches: Vec<_> = envelopes(store)
        .into_iter()
        .filter(|(id, _)| id == session_id)
        .collect();
    assert_eq!(matches.len(), 1, "one durable envelope for {session_id}");
    serde_json::from_slice(&fs::read(&matches[0].1).expect("envelope bytes"))
        .expect("envelope JSON")
}

fn roles(envelope: &Value) -> Vec<String> {
    envelope["messages"]
        .as_array()
        .expect("transcript array")
        .iter()
        .map(|message| message["role"].as_str().expect("role").to_string())
        .collect()
}

fn text(message: &Value) -> String {
    match &message["content"] {
        Value::String(value) => value.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                block["text"]
                    .as_str()
                    .or_else(|| block["content"].as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("unexpected message content: {other}"),
    }
}

/// Runs one standard ACP turn against the real binary and returns the session
/// identity together with the terminal the peer observed.
async fn acp_turn(
    home: &Path,
    workspace: &Path,
    prompt: &str,
) -> (String, Result<StopReason, String>) {
    let config = AcpAgentConfig::new(common::binary_path())
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.display().to_string())
        .env("HOME", home.display().to_string())
        .env(
            "COSH_TELEMETRY_DISABLED_PATH",
            home.join(".copilot-shell/telemetry_disabled")
                .display()
                .to_string(),
        );
    let agent = AcpAgent::new(config);
    let cwd = workspace.to_path_buf();
    let prompt = prompt.to_string();
    let scenario = Client
        .v1()
        .on_receive_notification(
            async move |_notification: SessionNotification, _connection| Ok(()),
            on_receive_notification!(),
        )
        .connect_with(agent, async move |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = connection
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?
                .session_id;
            let terminal = connection
                .send_request(PromptRequest::new(
                    session.clone(),
                    vec![ContentBlock::Text(TextContent::new(prompt))],
                ))
                .block_task()
                .await;
            Ok::<_, agent_client_protocol::Error>((
                session.0.to_string(),
                terminal
                    .map(|response| response.stop_reason)
                    .map_err(|error| error.message),
            ))
        })
        .await;
    scenario.expect("standard ACP scenario")
}

/// Runs one legacy JSONL turn whose prompt is a CLI argument, answering any
/// tool approval control request with allow or interrupt so the turn ends
/// deterministically instead of waiting for a decision surface.
fn legacy_tool_turn(
    home: &Path,
    workspace: &Path,
    args: &[&str],
    cancel_on_permission: bool,
) -> Vec<Value> {
    let mut command = common::cosh_core_command(home);
    command
        .args(["--headless", "--workspace"])
        .arg(workspace)
        .args(args)
        .env_remove("COSH_MODEL")
        .env_remove("COSH_AI_PROVIDER")
        .env_remove("OPENAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn cosh-core --headless");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("read stdout");
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).expect("valid JSONL");
        if value["type"] == "control_request" && value["request"]["subtype"] == "can_use_tool" {
            let reply = if cancel_on_permission {
                serde_json::json!({
                    "type": "control_request",
                    "request_id": "int-1",
                    "request": { "subtype": "interrupt" }
                })
            } else {
                serde_json::json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": value["request_id"],
                        "response": {
                            "behavior": "allow",
                            "updatedPermissions": [],
                            "toolUseID": value["request"]["tool_use_id"]
                        }
                    }
                })
            };
            writeln!(stdin, "{reply}").expect("write control reply");
        }
        let terminal = value["type"] == "result";
        lines.push(value);
        if terminal {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    lines
}

/// Runs one standard ACP turn that may request tool permission. On cancel the
/// client sends session/cancel and holds the permission responder unanswered,
/// because a permission reply maps to deny and would let the turn continue.
async fn acp_tool_turn(
    home: &Path,
    workspace: &Path,
    prompt: &str,
    cancel_on_permission: bool,
) -> (String, Result<StopReason, String>) {
    let config = AcpAgentConfig::new(common::binary_path())
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.display().to_string())
        .env("HOME", home.display().to_string())
        .env(
            "COSH_TELEMETRY_DISABLED_PATH",
            home.join(".copilot-shell/telemetry_disabled")
                .display()
                .to_string(),
        );
    let agent = AcpAgent::new(config);
    let cwd = workspace.to_path_buf();
    let prompt = prompt.to_string();
    let session_slot: Arc<Mutex<Option<SessionId>>> = Arc::new(Mutex::new(None));
    let handler_slot = Arc::clone(&session_slot);
    let scenario = Client
        .v1()
        .on_receive_notification(
            async move |_notification: SessionNotification, _connection| Ok(()),
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, connection| {
                if cancel_on_permission {
                    let session = handler_slot
                        .lock()
                        .expect("session slot")
                        .clone()
                        .expect("session id before permission");
                    connection.send_notification(CancelNotification::new(session))?;
                    connection.spawn(async move {
                        let _responder = responder;
                        std::future::pending::<Result<(), agent_client_protocol::Error>>().await
                    })?;
                } else {
                    let option = request
                        .options
                        .iter()
                        .find(|option| matches!(option.kind, PermissionOptionKind::AllowOnce))
                        .expect("allow_once option");
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            option.option_id.0.to_string(),
                        )),
                    ))?;
                }
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = connection
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?
                .session_id;
            *session_slot.lock().expect("session slot") = Some(session.clone());
            let terminal = connection
                .send_request(PromptRequest::new(
                    session.clone(),
                    vec![ContentBlock::Text(TextContent::new(prompt))],
                ))
                .block_task()
                .await;
            Ok::<_, agent_client_protocol::Error>((
                session.0.to_string(),
                terminal
                    .map(|response| response.stop_reason)
                    .map_err(|error| error.message),
            ))
        })
        .await;
    scenario.expect("standard ACP scenario")
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    store: PathBuf,
}

fn fixture(model: &str) -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&workspace).expect("workspace");
    configure(&home, &store, model);
    Fixture {
        _temp: temp,
        home,
        workspace,
        store,
    }
}

#[tokio::test]
async fn both_entries_commit_the_same_transcript_shape() {
    let fixture = fixture("mock-history");
    let legacy = json_lines(&legacy_turn(
        &fixture.home,
        &fixture.workspace,
        &["parity seed"],
    ));
    let legacy_id = result_line(&legacy)["session_id"]
        .as_str()
        .expect("legacy session id")
        .to_string();
    let (acp_id, terminal) = acp_turn(&fixture.home, &fixture.workspace, "parity seed").await;
    assert_eq!(terminal, Ok(StopReason::EndTurn));

    let legacy_envelope = envelope(&fixture.store, &legacy_id);
    let acp_envelope = envelope(&fixture.store, &acp_id);
    assert_ne!(legacy_id, acp_id, "each entry owns its session identity");
    assert_eq!(
        envelopes(&fixture.store).len(),
        2,
        "both entries commit into the same workspace scope"
    );
    for field in ["schema_version", "workspace_scope", "model", "generation"] {
        assert_eq!(
            legacy_envelope[field], acp_envelope[field],
            "{field} must agree across entries"
        );
    }
    assert_eq!(
        legacy_envelope["workspace_scope"],
        fs::canonicalize(&fixture.workspace)
            .expect("canonical workspace")
            .display()
            .to_string()
    );
    assert_eq!(roles(&legacy_envelope), roles(&acp_envelope));
    assert_eq!(roles(&acp_envelope), ["user", "assistant"]);
    for index in 0..2 {
        assert_eq!(
            text(&legacy_envelope["messages"][index]),
            text(&acp_envelope["messages"][index]),
            "message {index} must agree across entries"
        );
    }
    assert_eq!(text(&acp_envelope["messages"][0]), "parity seed");
    assert_eq!(
        text(&acp_envelope["messages"][1]),
        "mock history: parity seed"
    );
}

#[tokio::test]
async fn the_legacy_entry_resumes_the_record_written_by_the_acp_entry() {
    let fixture = fixture("mock-history");
    let (acp_id, terminal) = acp_turn(&fixture.home, &fixture.workspace, "alpha").await;
    assert_eq!(terminal, Ok(StopReason::EndTurn));
    assert_eq!(envelope(&fixture.store, &acp_id)["generation"], 1);

    let resumed = json_lines(&legacy_turn(
        &fixture.home,
        &fixture.workspace,
        &["--resume", &acp_id, "beta"],
    ));
    let result = result_line(&resumed);
    assert_eq!(result["is_error"], false, "{result}");
    assert_eq!(result["session_id"], acp_id);
    let envelope = envelope(&fixture.store, &acp_id);
    assert!(
        envelope["generation"].as_u64().expect("generation") > 1,
        "the resumed commit must advance the durable generation: {envelope}"
    );
    assert_eq!(
        text(
            envelope["messages"]
                .as_array()
                .expect("messages")
                .last()
                .expect("last message")
        ),
        "mock history: alpha | beta",
        "the resumed turn must see the transcript the ACP entry committed"
    );
}

#[tokio::test]
async fn a_failed_turn_keeps_the_same_partial_transcript_policy() {
    let fixture = fixture("mock-partial-error");
    let legacy = json_lines(&legacy_turn(
        &fixture.home,
        &fixture.workspace,
        &["failure seed"],
    ));
    let legacy_result = result_line(&legacy);
    assert_eq!(legacy_result["is_error"], true, "{legacy_result}");
    let legacy_id = legacy_result["session_id"]
        .as_str()
        .expect("legacy session id")
        .to_string();
    let (acp_id, terminal) = acp_turn(&fixture.home, &fixture.workspace, "failure seed").await;
    assert!(
        terminal.is_err(),
        "a failed turn must not report a successful stop reason: {terminal:?}"
    );

    let legacy_envelope = envelope(&fixture.store, &legacy_id);
    let acp_envelope = envelope(&fixture.store, &acp_id);
    assert_eq!(
        roles(&legacy_envelope),
        roles(&acp_envelope),
        "legacy={legacy_envelope} acp={acp_envelope}"
    );
    // The kernel discards the unfinished assistant step when the provider
    // fails, so the durable transcript keeps the user turn only. Both entries
    // have to commit that same record instead of inventing a partial reply.
    assert_eq!(roles(&acp_envelope), ["user"]);
    assert_eq!(
        text(&legacy_envelope["messages"][0]),
        text(&acp_envelope["messages"][0])
    );
    assert_eq!(text(&acp_envelope["messages"][0]), "failure seed");
}

#[tokio::test]
async fn both_entries_commit_the_same_tool_turn_transcript() {
    let fixture = fixture("mock-toolread");
    fs::write(fixture.workspace.join("probe.txt"), "probe content alpha").expect("probe file");
    let legacy = legacy_tool_turn(&fixture.home, &fixture.workspace, &["tool seed"], false);
    let legacy_result = result_line(&legacy);
    assert_eq!(legacy_result["subtype"], "success", "{legacy_result}");
    assert_eq!(legacy_result["is_error"], false);
    let legacy_id = legacy_result["session_id"]
        .as_str()
        .expect("legacy session id")
        .to_string();
    let (acp_id, terminal) =
        acp_tool_turn(&fixture.home, &fixture.workspace, "tool seed", false).await;
    assert_eq!(terminal, Ok(StopReason::EndTurn));

    let legacy_envelope = envelope(&fixture.store, &legacy_id);
    let acp_envelope = envelope(&fixture.store, &acp_id);
    for field in ["schema_version", "workspace_scope", "model", "generation"] {
        assert_eq!(
            legacy_envelope[field], acp_envelope[field],
            "{field} must agree across entries"
        );
    }
    assert_eq!(roles(&legacy_envelope), roles(&acp_envelope));
    assert_eq!(
        roles(&acp_envelope),
        ["user", "assistant", "tool", "assistant"]
    );
    for envelope in [&legacy_envelope, &acp_envelope] {
        let call_id = envelope["messages"][1]["tool_calls"][0]["id"]
            .as_str()
            .expect("tool call id");
        assert_eq!(
            envelope["messages"][2]["tool_call_id"]
                .as_str()
                .expect("tool result id"),
            call_id,
            "the tool result must pair with the declared call"
        );
        assert_eq!(
            envelope["messages"][1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
    }
    assert_eq!(
        legacy_envelope["messages"][1]["tool_calls"][0]["function"]["arguments"],
        acp_envelope["messages"][1]["tool_calls"][0]["function"]["arguments"]
    );
    assert_eq!(
        text(&legacy_envelope["messages"][2]),
        text(&acp_envelope["messages"][2])
    );
    assert_eq!(
        text(&acp_envelope["messages"][2]),
        "1\tprobe content alpha\n"
    );
    assert_eq!(
        text(&legacy_envelope["messages"][3]),
        text(&acp_envelope["messages"][3])
    );
    assert_eq!(text(&acp_envelope["messages"][3]), "The file was read.");
    assert_eq!(text(&acp_envelope["messages"][0]), "tool seed");
}

#[tokio::test]
async fn the_legacy_entry_resumes_the_tool_turn_record_written_by_the_acp_entry() {
    let fixture = fixture("mock-toolread");
    fs::write(fixture.workspace.join("probe.txt"), "probe content alpha").expect("probe file");
    let (acp_id, terminal) = acp_tool_turn(&fixture.home, &fixture.workspace, "alpha", false).await;
    assert_eq!(terminal, Ok(StopReason::EndTurn));

    let resumed = legacy_tool_turn(
        &fixture.home,
        &fixture.workspace,
        &["--resume", &acp_id, "beta"],
        false,
    );
    let result = result_line(&resumed);
    assert_eq!(result["is_error"], false, "{result}");
    assert_eq!(result["session_id"], acp_id);
    let envelope = envelope(&fixture.store, &acp_id);
    assert!(
        envelope["generation"].as_u64().expect("generation") > 1,
        "the resumed commit must advance the durable generation: {envelope}"
    );
    assert_eq!(
        roles(&envelope),
        [
            "user",
            "assistant",
            "tool",
            "assistant",
            "user",
            "assistant",
            "tool",
            "assistant"
        ]
    );
    for (call, result_index) in [(1, 2), (5, 6)] {
        let call_id = envelope["messages"][call]["tool_calls"][0]["id"]
            .as_str()
            .expect("tool call id");
        assert_eq!(
            envelope["messages"][result_index]["tool_call_id"]
                .as_str()
                .expect("tool result id"),
            call_id,
            "both tool turns must stay paired after resume"
        );
    }
    assert_eq!(text(&envelope["messages"][4]), "beta");
    assert_eq!(text(&envelope["messages"][3]), "The file was read.");
    assert_eq!(text(&envelope["messages"][7]), "The file was read.");
}

#[tokio::test]
async fn a_cancelled_tool_turn_keeps_the_same_paired_partial_transcript() {
    let fixture = fixture("mock-toolwrite");
    let legacy = legacy_tool_turn(&fixture.home, &fixture.workspace, &["cancel seed"], true);
    let legacy_result = result_line(&legacy);
    assert_eq!(legacy_result["subtype"], "cancelled", "{legacy_result}");
    assert_eq!(legacy_result["is_error"], false);
    let legacy_id = legacy_result["session_id"]
        .as_str()
        .expect("legacy session id")
        .to_string();
    let (acp_id, terminal) =
        acp_tool_turn(&fixture.home, &fixture.workspace, "cancel seed", true).await;
    assert_eq!(terminal, Ok(StopReason::Cancelled));

    let legacy_envelope = envelope(&fixture.store, &legacy_id);
    let acp_envelope = envelope(&fixture.store, &acp_id);
    assert_eq!(roles(&legacy_envelope), roles(&acp_envelope));
    assert_eq!(roles(&acp_envelope), ["user", "assistant", "tool"]);
    for envelope in [&legacy_envelope, &acp_envelope] {
        let call_id = envelope["messages"][1]["tool_calls"][0]["id"]
            .as_str()
            .expect("tool call id");
        assert_eq!(
            envelope["messages"][2]["tool_call_id"]
                .as_str()
                .expect("tool result id"),
            call_id,
            "a cancelled turn must still pair every declared call"
        );
        assert_eq!(
            envelope["messages"][1]["tool_calls"][0]["function"]["name"],
            "write_file"
        );
        assert_eq!(text(&envelope["messages"][2]), "Interrupted by user");
    }
    assert_eq!(
        legacy_envelope["messages"][1]["tool_calls"][0]["function"]["arguments"],
        acp_envelope["messages"][1]["tool_calls"][0]["function"]["arguments"]
    );
    assert_eq!(
        text(&legacy_envelope["messages"][0]),
        text(&acp_envelope["messages"][0])
    );
    assert_eq!(text(&acp_envelope["messages"][0]), "cancel seed");
}
