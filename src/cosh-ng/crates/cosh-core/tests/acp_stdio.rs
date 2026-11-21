//! Real stdio interop: the official ACP client drives `cosh-core --acp`.
//!
//! The client uses only standard v1 methods and no Cosh extension, so a pass
//! here is evidence about the standard surface, not about a private adapter.

mod common;

use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, SessionNotification,
    SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::{on_receive_notification, AcpAgent, AcpAgentConfig, Client};

fn mock_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"mock\"\n\n[ai.providers.mock]\ntype = \"mock\"\n",
    )
    .unwrap();
    std::fs::write(config_dir.join("telemetry_disabled"), "").unwrap();
    home
}

#[tokio::test]
async fn official_client_completes_a_text_turn_over_stdio() {
    let home = mock_home();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let config = AcpAgentConfig::new(common::binary_path())
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.path().display().to_string())
        .env("HOME", home.path().display().to_string())
        .env(
            "COSH_TELEMETRY_DISABLED_PATH",
            home.path()
                .join(".copilot-shell/telemetry_disabled")
                .display()
                .to_string(),
        );
    let agent = AcpAgent::new(config);

    let updates: Arc<Mutex<Vec<SessionUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    let cwd = workspace.path().to_path_buf();
    let scenario = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                sink.lock().unwrap().push(notification.update);
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(agent, async move |connection| {
            let initialize = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            assert_eq!(initialize.protocol_version, ProtocolVersion::V1);
            // session/load is implemented and advertised (execution record section 78).
            assert!(initialize.agent_capabilities.load_session);
            let session = connection
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            let response = connection
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new("hello over stdio"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, StopReason::EndTurn);
            Ok(())
        })
        .await;

    scenario.expect("standard client scenario");
    let text = updates
        .lock()
        .unwrap()
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("hello over stdio"),
        "the prompt must reach the model and stream back: {text:?}"
    );
}

#[test]
fn an_oversized_inbound_frame_closes_the_connection() {
    let home = mock_home();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let mut command = common::cosh_core_command(home.path());
    command
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn cosh-core --acp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        // Two frames' worth of bytes with no newline: the reader must refuse
        // before allocating the whole line.
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..32 {
            if stdin.write_all(&chunk).is_err() {
                break;
            }
        }
    }
    let output = child.wait_with_output().expect("wait for the agent");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an oversized frame must not be served: {stderr}"
    );
    assert!(
        stderr.contains("inbound limit"),
        "the refusal must name the frame cap: {stderr}"
    );
}

/// Configures a hook that forces an approval decision, so the standard
/// permission surface is exercised without a tool-executing provider.
fn ask_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"

[hooks]
enabled = true

[[hooks.UserPromptSubmit]]
command = '''python3 -c 'print("{\"decision\":\"ask\",\"reason\":\"needs review\"}")' '''
name = "prompt-ask"
"#,
    )
    .unwrap();
    std::fs::write(config_dir.join("telemetry_disabled"), "").unwrap();
    home
}

/// Drives one hook-gated prompt over real stdio and returns the stop reason,
/// the streamed text and the permission options the agent offered.
async fn hook_gated_prompt(choose: &'static str) -> (StopReason, String, Vec<(String, String)>) {
    let home = ask_home();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let config = AcpAgentConfig::new(common::binary_path())
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.path().display().to_string())
        .env("HOME", home.path().display().to_string())
        .env(
            "COSH_STATES_DIR",
            home.path().join("states").display().to_string(),
        )
        .env(
            "COSH_TELEMETRY_DISABLED_PATH",
            home.path()
                .join(".copilot-shell/telemetry_disabled")
                .display()
                .to_string(),
        );
    let agent = AcpAgent::new(config);

    let updates: Arc<Mutex<Vec<SessionUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    let options: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&options);
    let cwd = workspace.path().to_path_buf();
    let scenario = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                sink.lock().unwrap().push(notification.update);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: agent_client_protocol::v1::schema::RequestPermissionRequest,
                        responder,
                        _connection| {
                seen.lock().unwrap().extend(
                    request.options.iter().map(|option| {
                        (option.option_id.0.to_string(), format!("{:?}", option.kind))
                    }),
                );
                responder.respond(
                    agent_client_protocol::v1::schema::RequestPermissionResponse::new(
                        agent_client_protocol::v1::schema::RequestPermissionOutcome::Selected(
                            agent_client_protocol::v1::schema::SelectedPermissionOutcome::new(
                                choose,
                            ),
                        ),
                    ),
                )
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
                .await?;
            let response = connection
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new("review this prompt"))],
                ))
                .block_task()
                .await?;
            Ok(response.stop_reason)
        })
        .await;

    let reason = scenario.expect("standard permission scenario");
    let text = updates
        .lock()
        .unwrap()
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let offered = options.lock().unwrap().clone();
    (reason, text, offered)
}

#[tokio::test]
async fn a_generic_client_can_allow_a_hook_gated_prompt_over_stdio() {
    let (reason, text, options) = hook_gated_prompt("allow_once").await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(
        options,
        vec![
            ("allow_once".to_string(), "AllowOnce".to_string()),
            ("reject_once".to_string(), "RejectOnce".to_string()),
        ]
    );
    assert!(
        !text.contains("Prompt rejected"),
        "an allowed prompt must continue: {text:?}"
    );
}

#[tokio::test]
async fn a_generic_client_can_reject_a_hook_gated_prompt_over_stdio() {
    let (reason, text, options) = hook_gated_prompt("reject_once").await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(options.len(), 2);
    assert!(
        text.contains("Prompt rejected"),
        "a rejected prompt must report the refusal: {text:?}"
    );
}

#[tokio::test]
async fn a_client_provided_stdio_mcp_server_is_accepted_over_stdio() {
    use std::os::unix::fs::PermissionsExt;

    let home = mock_home();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let script = workspace.path().join("fixture-mcp.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}]}}'
      ;;
  esac
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = AcpAgentConfig::new(common::binary_path())
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.path().display().to_string())
        .env("HOME", home.path().display().to_string())
        .env(
            "COSH_TELEMETRY_DISABLED_PATH",
            home.path()
                .join(".copilot-shell/telemetry_disabled")
                .display()
                .to_string(),
        );
    let agent = AcpAgent::new(config);

    let cwd = workspace.path().to_path_buf();
    let server = agent_client_protocol::v1::schema::McpServer::Stdio(
        agent_client_protocol::v1::schema::McpServerStdio::new("fixture", script),
    );
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
                .send_request(NewSessionRequest::new(cwd).mcp_servers(vec![server]))
                .block_task()
                .await?;
            let response = connection
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new("hello with mcp"))],
                ))
                .block_task()
                .await?;
            Ok(response.stop_reason)
        })
        .await;

    let reason = scenario.expect("client MCP session over stdio");
    assert_eq!(reason, StopReason::EndTurn);
}
