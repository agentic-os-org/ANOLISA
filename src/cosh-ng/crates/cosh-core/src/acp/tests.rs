//! Deterministic ACP fixtures: an official client paired with the agent in process.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    ContentBlock, ErrorCode, ImageContent, InitializeRequest, LoadSessionRequest,
    NewSessionRequest, PromptRequest, RequestPermissionRequest, ResourceLink, SessionNotification,
    SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::v1::schema::{
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionResponse,
    SelectedPermissionOutcome, ToolCallStatus, ToolKind,
};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::{on_receive_notification, Client, Error};
use async_trait::async_trait;
use cosh_types::audit::AuditOutcomeStatus;
use tokio::sync::Notify;

use super::transport::CloseWatch;
use super::{
    agent, AcpRuntime, CancelClaim, EventSink, HostClaim, Projected, TurnControl, CLOSE_GRACE,
};
use crate::audit::CoreAuditRecorder;
use crate::config::CoreConfig;
use crate::core::CoshCore;
use crate::provider::mock::MockProvider;
use crate::provider::{
    ContentGenerator, GenerateConfig, GenerateEvent, GenerateStream, Message, ToolDeclaration,
};

/// Lets one provider instance serve every session the factory creates.
struct SharedProvider(Arc<dyn ContentGenerator>);

#[async_trait]
impl ContentGenerator for SharedProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.0.generate(messages, tools, config).await
    }

    fn cancel(&self) {
        self.0.cancel();
    }
}

/// Holds the model step open until the test releases or cancels it.
struct GatedProvider {
    started: Arc<Notify>,
    release: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
}

impl GatedProvider {
    fn new() -> (Arc<Self>, Arc<Notify>) {
        let started = Arc::new(Notify::new());
        let provider = Arc::new(Self {
            started: Arc::clone(&started),
            release: Arc::new(Notify::new()),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        (provider, started)
    }
}

#[async_trait]
impl ContentGenerator for GatedProvider {
    async fn generate(
        &self,
        _messages: &[Message],
        _tools: &[ToolDeclaration],
        _config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.started.notify_one();
        let release = Arc::clone(&self.release);
        let cancelled = Arc::clone(&self.cancelled);
        Ok(Box::pin(futures::stream::unfold((), move |_| {
            let release = Arc::clone(&release);
            let cancelled = Arc::clone(&cancelled);
            async move {
                release.notified().await;
                let event = if cancelled.load(Ordering::SeqCst) {
                    GenerateEvent::Cancelled
                } else {
                    GenerateEvent::MessageEnd
                };
                Some((event, ()))
            }
        })))
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.release.notify_one();
    }
}

fn runtime_with(provider: Arc<dyn ContentGenerator>) -> Arc<AcpRuntime> {
    runtime_with_config(CoreConfig::default(), provider)
}

fn runtime_with_config(config: CoreConfig, provider: Arc<dyn ContentGenerator>) -> Arc<AcpRuntime> {
    Arc::new(AcpRuntime::new(
        config,
        Arc::new(move |_| Box::new(SharedProvider(Arc::clone(&provider)))),
    ))
}

fn text_runtime(text: &str) -> Arc<AcpRuntime> {
    runtime_with(Arc::new(MockProvider::text_only(text)))
}

/// Records the user text the kernel actually sent to the model.
struct RecordingProvider {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ContentGenerator for RecordingProvider {
    async fn generate(
        &self,
        messages: &[Message],
        _tools: &[ToolDeclaration],
        _config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        let text = messages
            .iter()
            .filter(|message| message.role == "user")
            .map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join(" | ");
        self.seen.lock().unwrap().push(text.clone());
        Ok(Box::pin(futures::stream::iter(vec![
            GenerateEvent::TextDelta(text),
            GenerateEvent::MessageEnd,
        ])))
    }

    fn cancel(&self) {}
}

/// Collects every session update the agent sends, in arrival order.
type Updates = Arc<Mutex<Vec<SessionUpdate>>>;

fn collector() -> (
    Updates,
    impl Fn(SessionNotification) + Send + Sync + 'static,
) {
    let updates: Updates = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    (updates, move |notification: SessionNotification| {
        sink.lock().unwrap().push(notification.update);
    })
}

fn chunk_text(update: &SessionUpdate) -> Option<&str> {
    let chunk = match update {
        SessionUpdate::AgentMessageChunk(chunk) => chunk,
        _ => return None,
    };
    match &chunk.content {
        ContentBlock::Text(text) => Some(&text.text),
        _ => None,
    }
}

fn joined_text(updates: &Updates) -> String {
    updates
        .lock()
        .unwrap()
        .iter()
        .filter_map(chunk_text)
        .collect::<Vec<_>>()
        .join("")
}

/// Runs one client scenario against the agent and returns the collected updates.
async fn with_client<T>(
    runtime: Arc<AcpRuntime>,
    scenario: impl AsyncFnOnce(ConnectionTo<agent_client_protocol::Agent>) -> Result<T, Error>
        + Send
        + 'static,
) -> (T, Updates) {
    let (updates, record) = collector();
    let result = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                record(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                // No permission surface is advertised; a request here is a
                // contract violation the test must not silently approve.
                let _ = request;
                responder.respond_with_internal_error("unexpected permission request")
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent(runtime, CloseWatch::new(CLOSE_GRACE)), scenario)
        .await
        .expect("client scenario");
    (result, updates)
}

async fn start_session(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
) -> Result<agent_client_protocol::v1::schema::SessionId, Error> {
    connection
        .send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    let cwd = std::env::temp_dir();
    Ok(connection
        .send_request(NewSessionRequest::new(cwd))
        .block_task()
        .await?
        .session_id)
}

fn prompt(session_id: agent_client_protocol::v1::schema::SessionId, text: &str) -> PromptRequest {
    PromptRequest::new(session_id, vec![ContentBlock::Text(TextContent::new(text))])
}

/// One tool call step followed by a closing text step.
fn tool_then_text(tool: &str, id: &str, arguments: &str, text: &str) -> Arc<MockProvider> {
    Arc::new(MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: id.to_string(),
                name: tool.to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: arguments.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta(text.to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]))
}

type Asks = Arc<Mutex<Vec<RequestPermissionRequest>>>;

/// Runs a client scenario with a permission surface that records every ask.
async fn with_permission_client<T>(
    runtime: Arc<AcpRuntime>,
    decide: impl Fn(&RequestPermissionRequest) -> RequestPermissionResponse + Send + Sync + 'static,
    scenario: impl AsyncFnOnce(ConnectionTo<agent_client_protocol::Agent>) -> Result<T, Error>
        + Send
        + 'static,
) -> (T, Updates, Asks) {
    let (updates, record) = collector();
    let asks: Asks = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asks);
    let result = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                record(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                seen.lock().unwrap().push(request.clone());
                responder.respond(decide(&request))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent(runtime, CloseWatch::new(CLOSE_GRACE)), scenario)
        .await
        .expect("client scenario");
    (result, updates, asks)
}

/// Execute requests the agent sent, as the host-shell client received them.
type HostExecutes = Arc<Mutex<Vec<serde_json::Value>>>;

/// Answers one foreground execute, or refuses it with a JSON-RPC error.
type HostReply = Arc<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, Error> + Send + Sync>;

/// A client that advertises host-shell v1 and answers the execute extension.
///
/// It registers the catch-all agent request handler because the SDK routes
/// extension methods through the same enum. A standard permission request for
/// a negotiated shell action is a contract violation, not something to approve.
async fn with_host_shell_client<T>(
    runtime: Arc<AcpRuntime>,
    reply: HostReply,
    scenario: impl AsyncFnOnce(ConnectionTo<agent_client_protocol::Agent>) -> Result<T, Error>
        + Send
        + 'static,
) -> (T, Updates, HostExecutes) {
    with_host_shell_client_owned(
        runtime,
        reply,
        OwnedPolicy::Matching,
        std::time::Duration::ZERO,
        scenario,
    )
    .await
}

/// How the fixture client registers ownership before answering an execute.
#[derive(Clone, Copy)]
enum OwnedPolicy {
    Matching,
    Mismatched,
    Absent,
    /// Only the first execute is owned; a later action is refused before owned.
    FirstOnly,
}

/// Same client, with ownership emission and reply latency under test control.
async fn with_host_shell_client_owned<T>(
    runtime: Arc<AcpRuntime>,
    reply: HostReply,
    owned: OwnedPolicy,
    reply_delay: std::time::Duration,
    scenario: impl AsyncFnOnce(ConnectionTo<agent_client_protocol::Agent>) -> Result<T, Error>
        + Send
        + 'static,
) -> (T, Updates, HostExecutes) {
    with_host_shell_client_gated(runtime, reply, owned, reply_delay, None, scenario).await
}

/// Same client, with the execute reply held until the scenario releases it.
///
/// `gate` is `(owned_seen, release)`: the handler signals once ownership is on
/// the wire and then answers from a spawned task, so the scenario can order a
/// cancel strictly between ownership and the host terminal.
async fn with_host_shell_client_gated<T>(
    runtime: Arc<AcpRuntime>,
    reply: HostReply,
    owned: OwnedPolicy,
    reply_delay: std::time::Duration,
    gate: Option<(Arc<Notify>, Arc<Notify>)>,
    scenario: impl AsyncFnOnce(ConnectionTo<agent_client_protocol::Agent>) -> Result<T, Error>
        + Send
        + 'static,
) -> (T, Updates, HostExecutes) {
    let (updates, record) = collector();
    let executes: HostExecutes = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&executes);
    let result = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                record(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: agent_client_protocol::v1::schema::AgentRequest,
                        responder,
                        _connection| {
                match request {
                    agent_client_protocol::v1::schema::AgentRequest::ExtMethodRequest(ext) => {
                        // SDK 5.x keeps the full wire method name (leading underscore);
                        // 2.0.0 stripped it. Match the wire name the agent sends.
                        if ext.method.as_ref() != "_cosh/shell/v1/execute" {
                            return responder.respond_with_error(Error::method_not_found());
                        }
                        let params: serde_json::Value =
                            serde_json::from_str(ext.params.get()).expect("execute params");
                        seen.lock().unwrap().push(params.clone());
                        // The frozen ordering: ownership is registered before the
                        // local decision, so the peer sees owned first.
                        let tool_call_id = match owned {
                            OwnedPolicy::Matching => params["toolCallId"].clone(),
                            OwnedPolicy::FirstOnly => {
                                if seen.lock().unwrap().len() == 1 {
                                    params["toolCallId"].clone()
                                } else {
                                    serde_json::Value::Null
                                }
                            }
                            OwnedPolicy::Mismatched => {
                                serde_json::json!("a-different-action")
                            }
                            OwnedPolicy::Absent => serde_json::Value::Null,
                        };
                        if !tool_call_id.is_null() {
                            let body = serde_json::json!({
                                "sessionId": params["sessionId"],
                                "toolCallId": tool_call_id,
                            });
                            let raw = serde_json::value::RawValue::from_string(body.to_string())
                                .expect("owned params");
                            _connection.send_notification(
                                agent_client_protocol::v1::schema::ClientNotification::ExtNotification(
                                    agent_client_protocol::v1::schema::ExtNotification::new(
                                        "_cosh/shell/v1/owned",
                                        raw.into(),
                                    ),
                                ),
                            )?;
                        }
                        if let Some((owned_seen, release)) = &gate {
                            owned_seen.notify_one();
                            let release = Arc::clone(release);
                            let answer = Arc::clone(&reply);
                            let held = params.clone();
                            let _ = _connection.spawn(async move {
                                release.notified().await;
                                match answer(&held) {
                                    Ok(outcome) => responder.respond(outcome),
                                    Err(error) => responder.respond_with_error(error),
                                }
                            });
                            return Ok(());
                        }
                        if !reply_delay.is_zero() {
                            tokio::time::sleep(reply_delay).await;
                        }
                        match reply(&params) {
                            Ok(outcome) => responder.respond(outcome),
                            Err(error) => responder.respond_with_error(error),
                        }
                    }
                    agent_client_protocol::v1::schema::AgentRequest::RequestPermissionRequest(
                        _,
                    ) => responder.respond_with_internal_error(
                        "a negotiated shell action must not use standard permission",
                    ),
                    _ => responder.respond_with_error(Error::method_not_found()),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent(runtime, CloseWatch::new(CLOSE_GRACE)), scenario)
        .await
        .expect("host-shell client scenario");
    (result, updates, executes)
}

/// Initializes with the host-shell capability and opens one session.
async fn start_host_shell_session(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    capability: serde_json::Value,
    cwd: std::path::PathBuf,
) -> Result<agent_client_protocol::v1::schema::SessionId, Error> {
    let mut initialize = InitializeRequest::new(ProtocolVersion::V1);
    let mut meta = agent_client_protocol::v1::schema::Meta::new();
    meta.insert("_cosh/shell".to_string(), capability);
    initialize.client_capabilities.meta = Some(meta);
    connection.send_request(initialize).block_task().await?;
    Ok(connection
        .send_request(NewSessionRequest::new(cwd))
        .block_task()
        .await?
        .session_id)
}

/// The kernel behind the negotiated session, so a fixture can read the audit
/// records the turn actually wrote instead of only the wire projection.
fn session_core(runtime: &AcpRuntime) -> Arc<tokio::sync::Mutex<CoshCore>> {
    let guard = runtime.lock_session();
    Arc::clone(
        &guard
            .as_ref()
            .expect("a session exists after session/new")
            .core,
    )
}

/// The terminal facts a cooperative Shell returns for one owned action.
fn host_outcome() -> serde_json::Value {
    serde_json::json!({
        "command": "echo hi",
        "status": "completed",
        "exit_code": 0,
        "signal": serde_json::Value::Null,
        "cwd": "/tmp",
        "end_cwd": "/tmp",
        "duration_ms": 7,
        "output_ref": serde_json::Value::Null,
        "redaction_status": "none",
        "approval_id": serde_json::Value::Null,
        "tool_use_id": serde_json::Value::Null,
        "llmContent": "hi\n",
    })
}

fn allow_executes() -> HostReply {
    Arc::new(|_| Ok(host_outcome()))
}

fn selected(option_id: &'static str) -> RequestPermissionResponse {
    RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
        SelectedPermissionOutcome::new(option_id),
    ))
}

async fn start_session_in(
    connection: &ConnectionTo<agent_client_protocol::Agent>,
    cwd: std::path::PathBuf,
) -> Result<agent_client_protocol::v1::schema::SessionId, Error> {
    connection
        .send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    Ok(connection
        .send_request(NewSessionRequest::new(cwd))
        .block_task()
        .await?
        .session_id)
}

/// Finds the tool call updates of one kind in arrival order.
fn tool_updates(updates: &Updates) -> Vec<(String, Option<ToolCallStatus>)> {
    updates
        .lock()
        .unwrap()
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::ToolCall(call) => {
                Some((call.tool_call_id.0.to_string(), Some(call.status)))
            }
            SessionUpdate::ToolCallUpdate(update) => {
                Some((update.tool_call_id.0.to_string(), update.fields.status))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn initialize_declares_only_implemented_capabilities() {
    let (response, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await
    })
    .await;
    assert_eq!(response.protocol_version, ProtocolVersion::V1);
    let capabilities = response.agent_capabilities;
    assert!(
        capabilities.load_session,
        "session/load is implemented and advertised"
    );
    assert!(!capabilities.prompt_capabilities.image);
    assert!(!capabilities.prompt_capabilities.audio);
    assert!(!capabilities.prompt_capabilities.embedded_context);
    assert!(capabilities.session_capabilities.list.is_none());
    assert!(capabilities.session_capabilities.delete.is_none());
    assert!(response.auth_methods.is_empty());
    assert_eq!(response.agent_info.unwrap().name, "cosh-core");
}

#[tokio::test]
async fn text_prompt_streams_once_and_ends_the_turn() {
    let (reason, updates) = with_client(
        text_runtime("hello from the kernel"),
        async move |connection| {
            let session = start_session(&connection).await?;
            connection
                .send_request(prompt(session, "hi"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(joined_text(&updates), "hello from the kernel");
}

#[tokio::test]
async fn thinking_deltas_become_thought_chunks() {
    let provider = Arc::new(MockProvider::new(vec![vec![
        GenerateEvent::ThinkingDelta("considering".to_string()),
        GenerateEvent::TextDelta("answer".to_string()),
        GenerateEvent::MessageEnd,
    ]]));
    let (_reason, updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        connection
            .send_request(prompt(session, "hi"))
            .block_task()
            .await
    })
    .await;
    let thoughts = updates
        .lock()
        .unwrap()
        .iter()
        .filter(|update| matches!(update, SessionUpdate::AgentThoughtChunk(_)))
        .count();
    assert_eq!(thoughts, 1);
    assert_eq!(joined_text(&updates), "answer");
}

#[tokio::test]
async fn resource_link_reaches_the_model_as_a_reference() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        seen: Arc::clone(&seen),
    });
    let (reason, updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        connection
            .send_request(PromptRequest::new(
                session,
                vec![
                    ContentBlock::Text(TextContent::new("read this")),
                    ContentBlock::ResourceLink(ResourceLink::new(
                        "notes.txt",
                        "file:///tmp/notes.txt",
                    )),
                ],
            ))
            .block_task()
            .await
            .map(|response| response.stop_reason)
    })
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    let reference = "notes.txt (file:///tmp/notes.txt)";
    assert!(seen.lock().unwrap()[0].contains(reference));
    assert!(joined_text(&updates).contains(reference));
}

#[tokio::test]
async fn unsupported_prompt_block_is_rejected() {
    let (error, _updates) = with_client(text_runtime("unused"), async move |connection| {
        let session = start_session(&connection).await?;
        Ok::<_, Error>(
            connection
                .send_request(PromptRequest::new(
                    session,
                    vec![ContentBlock::Image(ImageContent::new(
                        "base64",
                        "image/png",
                    ))],
                ))
                .block_task()
                .await
                .expect_err("image is not advertised"),
        )
    })
    .await;
    assert_eq!(error.code, ErrorCode::InvalidParams);
}

#[tokio::test]
async fn session_new_requires_an_absolute_cwd() {
    let (error, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        Ok::<_, Error>(
            connection
                .send_request(NewSessionRequest::new(std::path::PathBuf::from(
                    "relative/path",
                )))
                .block_task()
                .await
                .expect_err("relative cwd"),
        )
    })
    .await;
    assert!(error.message.contains("absolute"));
}

#[tokio::test]
async fn a_relative_mcp_command_is_rejected_instead_of_ignored() {
    let (error, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        let server = agent_client_protocol::v1::schema::McpServer::Stdio(
            agent_client_protocol::v1::schema::McpServerStdio::new("fixture", "echo"),
        );
        Ok::<_, Error>(
            connection
                .send_request(
                    NewSessionRequest::new(std::env::temp_dir()).mcp_servers(vec![server]),
                )
                .block_task()
                .await
                .expect_err("a relative command must be refused"),
        )
    })
    .await;
    assert!(error.message.contains("absolute path"));
}

#[tokio::test]
async fn a_second_session_does_not_replace_the_first() {
    let ((error, reason), updates) =
        with_client(text_runtime("still usable"), async move |connection| {
            let first = start_session(&connection).await?;
            let error = connection
                .send_request(NewSessionRequest::new(std::env::temp_dir()))
                .block_task()
                .await
                .expect_err("capacity");
            let reason = connection
                .send_request(prompt(first, "hi"))
                .block_task()
                .await
                .map(|response| response.stop_reason)?;
            Ok::<_, Error>((error, reason))
        })
        .await;
    assert_eq!(error.code, ErrorCode::Other(super::CAPACITY_ERROR_CODE));
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(joined_text(&updates), "still usable");
}

#[tokio::test]
async fn prompt_for_an_unknown_session_is_rejected() {
    let (error, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        Ok::<_, Error>(
            connection
                .send_request(prompt(
                    agent_client_protocol::v1::schema::SessionId::new("missing"),
                    "hi",
                ))
                .block_task()
                .await
                .expect_err("unknown session"),
        )
    })
    .await;
    assert!(error.message.contains("unknown session"));
}

#[tokio::test]
async fn cancel_stops_the_turn_and_reports_cancelled() {
    let (provider, started) = GatedProvider::new();
    let (reason, _updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        let pending = connection.send_request(prompt(session.clone(), "hi"));
        started.notified().await;
        connection.send_notification(
            agent_client_protocol::v1::schema::CancelNotification::new(session),
        )?;
        pending
            .block_task()
            .await
            .map(|response| response.stop_reason)
    })
    .await;
    assert_eq!(reason, StopReason::Cancelled);
}

#[tokio::test]
async fn a_second_prompt_while_one_is_in_flight_is_rejected() {
    let (provider, started) = GatedProvider::new();
    let ((busy, reason), _updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        let pending = connection.send_request(prompt(session.clone(), "first"));
        started.notified().await;
        let busy = connection
            .send_request(prompt(session.clone(), "second"))
            .block_task()
            .await
            .expect_err("in-flight capacity");
        connection.send_notification(
            agent_client_protocol::v1::schema::CancelNotification::new(session),
        )?;
        let reason = pending
            .block_task()
            .await
            .map(|response| response.stop_reason)?;
        Ok::<_, Error>((busy, reason))
    })
    .await;
    assert_eq!(busy.code, ErrorCode::Other(super::CAPACITY_ERROR_CODE));
    assert_eq!(reason, StopReason::Cancelled);
}

#[tokio::test]
async fn a_sequential_prompt_reuses_the_released_turn_slot() {
    let provider = Arc::new(MockProvider::new(vec![
        vec![
            GenerateEvent::TextDelta("first".to_string()),
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("second".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]));
    let (reasons, updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        let first = connection
            .send_request(prompt(session.clone(), "one"))
            .block_task()
            .await?
            .stop_reason;
        let second = connection
            .send_request(prompt(session, "two"))
            .block_task()
            .await?
            .stop_reason;
        Ok::<_, Error>((first, second))
    })
    .await;
    assert_eq!(reasons, (StopReason::EndTurn, StopReason::EndTurn));
    assert_eq!(joined_text(&updates), "firstsecond");
}

#[tokio::test]
async fn a_flood_of_deltas_is_rejected_instead_of_silently_dropped() {
    let deltas = (0..super::MAX_QUEUED_EVENTS * 4)
        .map(|index| GenerateEvent::TextDelta(format!("chunk {index};")))
        .collect::<Vec<_>>();
    let mut events = deltas;
    events.push(GenerateEvent::MessageEnd);
    let provider = Arc::new(MockProvider::new(vec![events]));
    let (updates, record) = collector();
    let outcome = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                record(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(
            agent(runtime_with(provider), CloseWatch::new(CLOSE_GRACE)),
            async move |connection| {
                let session = start_session(&connection).await?;
                connection
                    .send_request(prompt(session, "hi"))
                    .block_task()
                    .await?;
                Ok::<_, Error>(())
            },
        )
        .await;
    // Overload is an explicit exit: the peer sees the typed error and the
    // connection closes instead of the agent silently dropping output.
    let connection_error = outcome.expect_err("the connection must not survive an overflow");
    assert_eq!(
        connection_error.code,
        ErrorCode::Other(super::CAPACITY_ERROR_CODE)
    );
    assert!(connection_error.message.contains("event budget"));
    // The peer still receives the updates that fit, so the loss is bounded and
    // reported rather than silent.
    assert!(!updates.lock().unwrap().is_empty());
}

#[test]
fn an_assistant_snapshot_repeating_streamed_text_stays_internal() {
    let snapshot = serde_json::json!({
        "type": "assistant",
        "session_id": "s",
        "message": { "content": [{ "type": "text", "text": "hello" }] },
    });
    let mut projector = super::projection::Projector::new();
    let delta = serde_json::json!({
        "type": "stream_event",
        "event": { "type": "content_block_delta", "index": 0,
                   "delta": { "type": "text_delta", "text": "hello" } },
    });
    assert_eq!(projector.project(&delta).len(), 1);
    assert!(projector.project(&snapshot).is_empty());
    let result = serde_json::json!({ "type": "result", "is_error": false });
    assert!(projector.project(&result).is_empty());
}

#[test]
fn an_assistant_snapshot_that_was_never_streamed_is_forwarded() {
    let snapshot = serde_json::json!({
        "type": "assistant",
        "session_id": "s",
        "message": { "content": [{ "type": "text", "text": "Prompt rejected: no" }] },
    });
    let projected = super::projection::Projector::new().project(&snapshot);
    let [Projected::Update(update)] = projected.as_slice() else {
        panic!("an unstreamed refusal must reach the peer");
    };
    let SessionUpdate::AgentMessageChunk(chunk) = update.as_ref() else {
        panic!("the refusal must be a message chunk");
    };
    let ContentBlock::Text(text) = &chunk.content else {
        panic!("the refusal must be text");
    };
    assert_eq!(text.text, "Prompt rejected: no");
}

#[test]
fn empty_deltas_are_not_forwarded() {
    let empty = serde_json::json!({
        "type": "stream_event",
        "event": { "type": "content_block_delta", "index": 0,
                   "delta": { "type": "text_delta", "text": "" } },
    });
    assert!(matches!(
        super::projection::Projector::new()
            .project(&empty)
            .as_slice(),
        [Projected::Ignored]
    ));
}

#[test]
fn an_approval_request_is_projected_as_a_permission_ask() {
    let request = serde_json::json!({
        "type": "control_request",
        "request_id": "r",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "write_file",
            "tool_use_id": "t1",
            "input": { "path": "/tmp/a", "content": "x" },
        },
    });
    let projected = super::projection::Projector::new().project(&request);
    let [Projected::Permission(ask)] = projected.as_slice() else {
        panic!("an approval request must become a permission ask");
    };
    assert_eq!(ask.request_id, "r");
    assert_eq!(ask.tool_use_id, "t1");
    assert_eq!(ask.tool_name, "write_file");
    assert_eq!(ask.input["path"], "/tmp/a");
}

#[test]
fn a_host_control_request_without_an_approval_subtype_is_unsupported() {
    let request = serde_json::json!({
        "type": "control_request",
        "request_id": "r",
        "request": { "subtype": "shell_evidence" },
    });
    assert!(matches!(
        super::projection::Projector::new()
            .project(&request)
            .as_slice(),
        [Projected::UnsupportedInteraction]
    ));
}

#[test]
fn sink_frames_lines_split_across_writes() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let overflowed = Arc::new(AtomicBool::new(false));
    let mut sink = EventSink::new(tx, 1024, Arc::clone(&overflowed));
    std::io::Write::write_all(&mut sink, b"{\"type\":\"res").unwrap();
    std::io::Write::write_all(&mut sink, b"ult\"}\n{\"type\":\"sys").unwrap();
    std::io::Write::write_all(&mut sink, b"tem\"}\n").unwrap();
    assert!(!overflowed.load(Ordering::SeqCst));
    assert_eq!(rx.try_recv().unwrap()["type"], "result");
    assert_eq!(rx.try_recv().unwrap()["type"], "system");
}

#[test]
fn sink_bounds_a_line_that_never_terminates() {
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let overflowed = Arc::new(AtomicBool::new(false));
    let mut sink = EventSink::new(tx, 8, Arc::clone(&overflowed));
    std::io::Write::write_all(&mut sink, b"0123456789").unwrap();
    assert!(overflowed.load(Ordering::SeqCst));
}

#[test]
fn sink_records_a_full_queue_instead_of_losing_output_quietly() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let overflowed = Arc::new(AtomicBool::new(false));
    let mut sink = EventSink::new(tx, 1024, Arc::clone(&overflowed));
    std::io::Write::write_all(&mut sink, b"{\"type\":\"a\"}\n").unwrap();
    assert!(!overflowed.load(Ordering::SeqCst));
    std::io::Write::write_all(&mut sink, b"{\"type\":\"b\"}\n").unwrap();
    assert!(overflowed.load(Ordering::SeqCst));
}

async fn read_bounded(input: &[u8], cap: usize) -> Vec<std::io::Result<String>> {
    use futures::StreamExt;
    let reader = tokio::io::BufReader::new(std::io::Cursor::new(input.to_vec()));
    super::transport::bounded_lines(reader, cap)
        .collect::<Vec<_>>()
        .await
}

#[tokio::test]
async fn bounded_reader_yields_whole_lines() {
    let items = read_bounded(b"{\"a\":1}\n{\"b\":2}\n", 1024).await;
    let lines = items
        .into_iter()
        .map(|item| item.expect("valid frame"))
        .collect::<Vec<_>>();
    assert_eq!(lines, vec!["{\"a\":1}", "{\"b\":2}"]);
}

#[tokio::test]
async fn bounded_reader_keeps_a_final_line_without_a_newline() {
    let items = read_bounded(b"tail", 1024).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].as_ref().expect("valid frame"), "tail");
}

#[tokio::test]
async fn bounded_reader_rejects_an_oversized_frame_and_stops() {
    let items = read_bounded(b"0123456789\nnext\n", 8).await;
    assert_eq!(items.len(), 1);
    let error = items[0].as_ref().expect_err("over the cap");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("inbound limit"));
}

#[tokio::test]
async fn bounded_reader_rejects_a_frame_that_never_terminates() {
    let items = read_bounded(&[b'x'; 64], 8).await;
    assert_eq!(items.len(), 1);
    assert!(items[0].is_err());
}

#[tokio::test]
async fn bounded_reader_rejects_invalid_utf8() {
    let items = read_bounded(&[0xff, b'\n'], 1024).await;
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].as_ref().expect_err("invalid utf-8").kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[tokio::test]
async fn a_read_only_tool_call_is_projected_with_its_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), "kernel evidence").unwrap();
    let provider = tool_then_text("read_file", "call-read", r#"{"path":"note.txt"}"#, "done");
    let cwd = dir.path().to_path_buf();
    let (reason, updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::ALLOW_ONCE),
        async move |connection| {
            let session = start_session_in(&connection, cwd).await?;
            connection
                .send_request(prompt(session, "read it"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert!(
        asks.lock().unwrap().is_empty(),
        "read-only needs no approval"
    );
    assert_eq!(
        tool_updates(&updates),
        vec![
            ("call-read".to_string(), Some(ToolCallStatus::Pending)),
            ("call-read".to_string(), None),
            ("call-read".to_string(), Some(ToolCallStatus::Completed)),
        ]
    );
    let recorded = updates.lock().unwrap();
    let Some(SessionUpdate::ToolCall(call)) = recorded.first() else {
        panic!("the first tool update must create the call");
    };
    assert_eq!(call.kind, ToolKind::Read);
    let Some(SessionUpdate::ToolCallUpdate(input)) = recorded.get(1) else {
        panic!("the second tool update must carry the streamed arguments");
    };
    assert!(input.fields.title.as_deref().unwrap().contains("note.txt"));
    assert_eq!(input.fields.raw_input.as_ref().unwrap()["path"], "note.txt");
    let Some(SessionUpdate::ToolCallUpdate(update)) = recorded.get(2) else {
        panic!("the third tool update must carry the result");
    };
    assert!(update.fields.raw_output.is_some());
}

#[tokio::test]
async fn a_guarded_write_asks_the_peer_and_an_allow_executes_it() {
    let dir = tempfile::tempdir().unwrap();
    let provider = tool_then_text(
        "write_file",
        "call-write",
        r#"{"path":"out.txt","content":"approved"}"#,
        "done",
    );
    let cwd = dir.path().to_path_buf();
    let target = dir.path().join("out.txt");
    let (reason, _updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::ALLOW_ONCE),
        async move |connection| {
            let session = start_session_in(&connection, cwd).await?;
            connection
                .send_request(prompt(session, "write it"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "approved");
    let recorded = asks.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let ask = &recorded[0];
    assert_eq!(ask.tool_call.tool_call_id.0.as_ref(), "call-write");
    assert_eq!(ask.tool_call.fields.kind, Some(ToolKind::Edit));
    let options: Vec<_> = ask
        .options
        .iter()
        .map(|option| (option.option_id.0.to_string(), option.kind))
        .collect();
    assert_eq!(
        options,
        vec![
            (
                super::ALLOW_ONCE.to_string(),
                PermissionOptionKind::AllowOnce
            ),
            (
                super::REJECT_ONCE.to_string(),
                PermissionOptionKind::RejectOnce
            ),
        ]
    );
}

#[tokio::test]
async fn a_guarded_write_denied_by_the_peer_is_not_executed() {
    let dir = tempfile::tempdir().unwrap();
    let provider = tool_then_text(
        "write_file",
        "call-deny",
        r#"{"path":"out.txt","content":"denied"}"#,
        "done",
    );
    let cwd = dir.path().to_path_buf();
    let target = dir.path().join("out.txt");
    let (reason, _updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::REJECT_ONCE),
        async move |connection| {
            let session = start_session_in(&connection, cwd).await?;
            connection
                .send_request(prompt(session, "write it"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(asks.lock().unwrap().len(), 1);
    assert!(!target.exists(), "a denied call must not write the file");
}

#[tokio::test]
async fn a_cancelled_permission_outcome_denies_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let provider = tool_then_text(
        "write_file",
        "call-cancel",
        r#"{"path":"out.txt","content":"cancelled"}"#,
        "done",
    );
    let cwd = dir.path().to_path_buf();
    let target = dir.path().join("out.txt");
    let (reason, _updates, _asks) = with_permission_client(
        runtime_with(provider),
        |_| RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
        async move |connection| {
            let session = start_session_in(&connection, cwd).await?;
            connection
                .send_request(prompt(session, "write it"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert!(!target.exists(), "a cancelled decision must not authorize");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_turn_does_not_wait_for_a_pending_permission_answer() {
    let dir = tempfile::tempdir().unwrap();
    let provider = tool_then_text(
        "write_file",
        "call-pending",
        r#"{"path":"out.txt","content":"pending"}"#,
        "done",
    );
    let asked = Arc::new(Notify::new());
    let never = Arc::new(Notify::new());
    let (updates, record) = collector();
    let signal = Arc::clone(&asked);
    let hold = Arc::clone(&never);
    let cwd = dir.path().to_path_buf();
    let target = dir.path().join("out.txt");
    let outcome = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                record(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |_request: RequestPermissionRequest, responder, connection| {
                signal.notify_one();
                // The peer never answers: the turn must still end on cancel.
                // Waiting happens in a spawned task so the client keeps
                // dispatching the prompt response.
                let hold = Arc::clone(&hold);
                connection.spawn(async move {
                    hold.notified().await;
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            agent(runtime_with(provider), CloseWatch::new(CLOSE_GRACE)),
            async move |connection| {
                let session = start_session_in(&connection, cwd).await?;
                let pending = connection.send_request(prompt(session.clone(), "write it"));
                asked.notified().await;
                connection.send_notification(
                    agent_client_protocol::v1::schema::CancelNotification::new(session),
                )?;
                let reason = pending
                    .block_task()
                    .await
                    .map(|response| response.stop_reason);
                // Released only after the agent already answered, so the turn
                // provably did not wait for this decision.
                never.notify_one();
                reason
            },
        )
        .await
        .expect("client scenario");
    assert_eq!(outcome, StopReason::Cancelled);
    assert!(
        !target.exists(),
        "an unanswered permission must not execute"
    );
    drop(updates);
}

/// Cancel and the host terminal race for one decision right on the production
/// multi-thread runtime. Exactly one branch may claim it, and the winner must
/// leave UNKNOWN recorded so the loser can neither answer again nor interrupt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_one_branch_claims_the_owned_execute_decision() {
    let mut cancel_wins = 0usize;
    let mut host_wins = 0usize;
    for round in 0..400 {
        let (input, peer) = tokio::io::duplex(64 * 1024);
        let turn = Arc::new(TurnControl::new(input));
        turn.mark_host_execute_pending("req-1");
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let cancel_turn = Arc::clone(&turn);
        let cancel_barrier = Arc::clone(&barrier);
        let cancel_future = async move {
            cancel_barrier.wait().await;
            cancel_turn.claim_cancel()
        };
        let host_turn = Arc::clone(&turn);
        let host_barrier = Arc::clone(&barrier);
        let host_future = async move {
            host_barrier.wait().await;
            host_turn.claim_host_execute("req-1", true)
        };
        // The last task to reach the barrier proceeds without a wake, so a
        // fixed spawn order would only ever exercise one interleaving.
        let (cancel, host) = if round % 2 == 0 {
            let host = tokio::spawn(host_future);
            let cancel = tokio::spawn(cancel_future);
            (cancel, host)
        } else {
            let cancel = tokio::spawn(cancel_future);
            let host = tokio::spawn(host_future);
            (cancel, host)
        };
        let claimed = (
            cancel.await.expect("cancel task"),
            host.await.expect("host task"),
        );
        match claimed {
            (CancelClaim::ResolveUnknown(id), HostClaim::Rejected) => {
                assert_eq!(id, "req-1");
                cancel_wins += 1;
            }
            (CancelClaim::Leave, HostClaim::Claimed(id)) => {
                assert_eq!(id, "req-1");
                host_wins += 1;
            }
            other => {
                panic!("round {round}: both or neither branch claimed the decision: {other:?}")
            }
        }
        assert!(
            turn.host_execute_is_unknown(),
            "round {round}: the winner must leave UNKNOWN recorded"
        );
        // A repeated cancel or a late host reply must not reopen a decision
        // that is already UNKNOWN, nor claim the right to answer again.
        assert!(
            matches!(turn.claim_cancel(), CancelClaim::Leave),
            "round {round}: a repeated cancel must not interrupt an UNKNOWN turn"
        );
        assert!(
            matches!(turn.claim_host_execute("req-1", true), HostClaim::Rejected),
            "round {round}: a late unknown reply must not reclaim the decision"
        );
        assert!(
            matches!(turn.claim_host_execute("req-1", false), HostClaim::Rejected),
            "round {round}: a late definite reply must not reclaim the decision"
        );
        assert!(
            turn.host_execute_is_unknown(),
            "round {round}: repeated claims must keep UNKNOWN recorded"
        );
        drop(peer);
    }
    assert!(
        cancel_wins > 0 && host_wins > 0,
        "both interleavings must be exercised: cancel={cancel_wins} host={host_wins}"
    );
}

/// A decided slot keeps its state across repeated and late claims for the same
/// action, while the next action still establishes its own decision.
#[test]
fn a_decided_slot_is_never_reopened() {
    let (input, peer) = tokio::io::duplex(1024);
    let turn = TurnControl::new(input);
    turn.mark_host_execute_pending("req-1");
    assert!(matches!(
        turn.claim_host_execute("req-1", true),
        HostClaim::Claimed(id) if id == "req-1"
    ));
    assert!(matches!(turn.claim_cancel(), CancelClaim::Leave));
    assert!(matches!(
        turn.claim_host_execute("req-1", false),
        HostClaim::Rejected
    ));
    assert!(turn.host_execute_is_unknown());
    // UNKNOWN is sticky for the turn: a later owned action and its terminal
    // must neither clear it nor claim a decision of their own.
    turn.mark_host_execute_pending("req-2");
    assert!(turn.host_execute_is_unknown());
    assert!(matches!(
        turn.claim_host_execute("req-2", false),
        HostClaim::Rejected
    ));
    assert!(matches!(turn.claim_cancel(), CancelClaim::Leave));
    assert!(turn.host_execute_is_unknown());

    let (input, terminal_peer) = tokio::io::duplex(1024);
    let turn = TurnControl::new(input);
    turn.mark_host_execute_pending("req-1");
    assert!(matches!(
        turn.claim_host_execute("req-1", false),
        HostClaim::Claimed(id) if id == "req-1"
    ));
    assert!(matches!(turn.claim_cancel(), CancelClaim::Interrupt));
    assert!(matches!(
        turn.claim_host_execute("req-1", true),
        HostClaim::Rejected
    ));
    assert!(matches!(
        turn.claim_host_execute("req-1", false),
        HostClaim::Rejected
    ));
    assert!(!turn.host_execute_is_unknown());
    // A definite terminal belongs to that action alone: the next action claims
    // its own decision, and a late fact for the earlier one claims nothing.
    assert!(matches!(
        turn.claim_host_execute("req-2", false),
        HostClaim::Unowned
    ));
    turn.mark_host_execute_pending("req-2");
    assert!(matches!(
        turn.claim_host_execute("req-1", false),
        HostClaim::Rejected
    ));
    assert!(matches!(
        turn.claim_host_execute("req-2", false),
        HostClaim::Claimed(id) if id == "req-2"
    ));
    assert!(!turn.host_execute_is_unknown());
    drop(peer);
    drop(terminal_peer);
}

/// Two sequential foreground actions in one turn: the first ends with a definite
/// host terminal, the second is refused before it is ever owned.
///
/// The decision right belongs to one action. The second action must reach its
/// own explicit refusal instead of inheriting the first action's decided slot
/// and waiting out the residual deadline.
#[tokio::test]
async fn a_refused_next_action_is_not_blocked_by_the_previous_terminal() {
    let _guard = crate::core::tests::APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let runtime = runtime_with(Arc::new(MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo hi"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-2".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo again"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("done".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ])));
    // A standard invalid-params refusal, not the UNKNOWN code: the host is
    // stating that this action will not run.
    let reply: HostReply = Arc::new(|params| {
        if params["toolCallId"] == "call-1" {
            Ok(host_outcome())
        } else {
            Err(Error::invalid_params())
        }
    });
    let probe = Arc::clone(&runtime);
    let reader = Arc::clone(&runtime);
    let (reason, updates, executes) = with_host_shell_client_owned(
        runtime,
        reply,
        OwnedPolicy::FirstOnly,
        std::time::Duration::ZERO,
        async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                std::env::temp_dir(),
            )
            .await?;
            let handle = session_core(&probe);
            let mut core = handle.lock().await;
            core.audit = CoreAuditRecorder::test_capture(&core.session_id);
            drop(core);
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await
                .map(|response| response.stop_reason))
        },
    )
    .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");

    assert_eq!(
        executes.lock().unwrap().len(),
        2,
        "each action is dispatched exactly once, with no replay"
    );
    assert_eq!(
        reason.expect("the refused second action must not wait out the residual deadline"),
        StopReason::EndTurn
    );
    assert!(
        joined_text(&updates).contains("done"),
        "an explicit refusal lets the model advance: {:?}",
        joined_text(&updates)
    );
    let handle = session_core(&reader);
    let core = handle.lock().await;
    let events = core.audit.captured_events();
    // The host-executed allow path deliberately records no approval.resolved,
    // so the only resolved record here belongs to the refused second action.
    let resolved: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.as_str() == "approval.resolved")
        .collect();
    assert_eq!(
        resolved.len(),
        1,
        "the refused action must reach its own recorded decision: {resolved:?}"
    );
    assert_eq!(
        resolved[0].outcome.status,
        AuditOutcomeStatus::Denied,
        "the second action keeps its own explicit refusal: {:?}",
        resolved[0].outcome
    );
    assert_eq!(
        resolved[0].data().get("decision").and_then(|v| v.as_str()),
        Some("deny"),
        "the refusal must be the host's decision, not the residual deadline"
    );
    assert_eq!(core.metrics.approval_deny, 1);
    assert_eq!(core.metrics.approval_unknown, 0);
}

/// Writes a controlled stdio MCP server that answers initialize, tools/list and
/// tools/call, mirroring the kernel's own MCP fixture pattern.
/// G4 session/load: a persisted session is resumed on a fresh connection, its
/// user/assistant transcript is replayed as session/update chunks, and a
/// continuation prompt still runs against the restored context.
#[tokio::test]
async fn session_load_replays_the_persisted_transcript_and_continues() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path().to_path_buf();
    let mut config = CoreConfig::default();
    config.session.auto_persist = true;

    let cwd1 = cwd.clone();
    let runtime1 = runtime_with_config(
        config.clone(),
        Arc::new(MockProvider::text_only("world response")),
    );
    let (sid, _updates1) = with_client(runtime1, async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        let session = connection
            .send_request(NewSessionRequest::new(cwd1.clone()))
            .block_task()
            .await?
            .session_id;
        let response = connection
            .send_request(prompt(session.clone(), "hello"))
            .block_task()
            .await?;
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        Ok(session)
    })
    .await;

    let cwd2 = cwd.clone();
    let sid2 = sid.clone();
    let runtime2 = runtime_with_config(
        config.clone(),
        Arc::new(MockProvider::text_only("continued")),
    );
    let (_load, updates2) = with_client(runtime2, async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        let load = connection
            .send_request(LoadSessionRequest::new(sid2.clone(), cwd2.clone()))
            .block_task()
            .await?;
        let response = connection
            .send_request(prompt(sid2.clone(), "continue"))
            .block_task()
            .await?;
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        Ok(load)
    })
    .await;

    let recorded = updates2.lock().unwrap();
    let replay_text: Vec<&str> = recorded
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::UserMessageChunk(chunk) | SessionUpdate::AgentMessageChunk(chunk) => {
                match &chunk.content {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    assert!(
        replay_text.iter().any(|text| text.contains("hello")),
        "the persisted user turn must be replayed: {replay_text:?}"
    );
    assert!(
        replay_text
            .iter()
            .any(|text| text.contains("world response")),
        "the persisted assistant turn must be replayed: {replay_text:?}"
    );
    assert!(
        replay_text.iter().any(|text| text.contains("continued")),
        "the continuation prompt must still run: {replay_text:?}"
    );
}

fn mcp_fixture() -> (
    tempfile::TempDir,
    agent_client_protocol::v1::schema::McpServer,
) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fixture-mcp.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"mcp evidence"}]}}'
      ;;
  esac
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = agent_client_protocol::v1::schema::McpServer::Stdio(
        agent_client_protocol::v1::schema::McpServerStdio::new("fixture", script)
            .args(vec![])
            .env(vec![]),
    );
    (dir, server)
}

#[tokio::test]
async fn a_client_stdio_mcp_tool_is_discovered_and_invoked() {
    let (_dir, server) = mcp_fixture();
    let provider = tool_then_text(
        "mcp__fixture__echo",
        "call-mcp",
        r#"{"message":"hello"}"#,
        "done",
    );
    let cwd = std::env::temp_dir();
    let (reason, updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::ALLOW_ONCE),
        async move |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = connection
                .send_request(NewSessionRequest::new(cwd).mcp_servers(vec![server]))
                .block_task()
                .await?
                .session_id;
            connection
                .send_request(prompt(session, "use the tool"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(asks.lock().unwrap().len(), 1, "an MCP call needs approval");
    let recorded = updates.lock().unwrap();
    let result = recorded
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::ToolCallUpdate(update) => update.fields.raw_output.as_ref(),
            _ => None,
        })
        .find(|output| output.to_string().contains("mcp evidence"));
    assert!(
        result.is_some(),
        "the MCP result must reach the peer: {recorded:?}"
    );
}

/// H2: a single MCP server that fails to start only warns; the session still
/// runs and a healthy server's tool is still discovered and invoked.
#[tokio::test]
async fn a_failing_mcp_server_only_warns_and_the_session_still_runs() {
    use std::os::unix::fs::PermissionsExt;
    let broken_dir = tempfile::tempdir().unwrap();
    let broken_script = broken_dir.path().join("broken-mcp.sh");
    std::fs::write(&broken_script, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&broken_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let broken = agent_client_protocol::v1::schema::McpServer::Stdio(
        agent_client_protocol::v1::schema::McpServerStdio::new("broken", broken_script)
            .args(vec![])
            .env(vec![]),
    );
    let (_dir, healthy) = mcp_fixture();
    let provider = tool_then_text(
        "mcp__fixture__echo",
        "call-mcp",
        r#"{"message":"hello"}"#,
        "done",
    );
    let cwd = std::env::temp_dir();
    let (reason, updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::ALLOW_ONCE),
        async move |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = connection
                .send_request(NewSessionRequest::new(cwd).mcp_servers(vec![broken, healthy]))
                .block_task()
                .await?
                .session_id;
            connection
                .send_request(prompt(session, "use the tool"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(
        reason,
        StopReason::EndTurn,
        "the broken server must not fail the session"
    );
    assert_eq!(
        asks.lock().unwrap().len(),
        1,
        "the healthy MCP call still needs approval"
    );
    let recorded = updates.lock().unwrap();
    let result = recorded
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::ToolCallUpdate(update) => update.fields.raw_output.as_ref(),
            _ => None,
        })
        .find(|output| output.to_string().contains("mcp evidence"));
    assert!(
        result.is_some(),
        "the healthy server's result must still reach the peer: {recorded:?}"
    );
}

#[tokio::test]
async fn a_non_stdio_mcp_server_is_rejected() {
    let (error, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        let server = agent_client_protocol::v1::schema::McpServer::Http(
            agent_client_protocol::v1::schema::McpServerHttp::new(
                "remote",
                "https://example.invalid/mcp",
            ),
        );
        Ok::<_, Error>(
            connection
                .send_request(
                    NewSessionRequest::new(std::env::temp_dir()).mcp_servers(vec![server]),
                )
                .block_task()
                .await
                .expect_err("HTTP MCP is not advertised"),
        )
    })
    .await;
    assert!(error.message.contains("stdio"));
}

#[tokio::test]
async fn too_many_mcp_servers_are_rejected() {
    let servers: Vec<_> = (0..=super::MAX_MCP_SERVERS)
        .map(|index| {
            agent_client_protocol::v1::schema::McpServer::Stdio(
                agent_client_protocol::v1::schema::McpServerStdio::new(
                    format!("fixture{index}"),
                    "/bin/echo",
                ),
            )
        })
        .collect();
    let (error, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        Ok::<_, Error>(
            connection
                .send_request(NewSessionRequest::new(std::env::temp_dir()).mcp_servers(servers))
                .block_task()
                .await
                .expect_err("the server count is bounded"),
        )
    })
    .await;
    assert_eq!(error.code, ErrorCode::Other(super::CAPACITY_ERROR_CODE));
}

#[test]
fn host_shell_negotiation_requires_version_one_and_a_legal_pty_instance() {
    let meta = |value: serde_json::Value| {
        let mut meta = agent_client_protocol::v1::schema::Meta::new();
        meta.insert("_cosh/shell".to_string(), value);
        meta
    };
    let legal = serde_json::json!({"version": 1, "ptyInstance": "pty-7"});
    assert_eq!(
        super::negotiated_host_shell(Some(&meta(legal))).as_deref(),
        Some("pty-7")
    );
    assert_eq!(super::negotiated_host_shell(None), None);
    assert_eq!(
        super::negotiated_host_shell(Some(&agent_client_protocol::v1::schema::Meta::new())),
        None
    );
    for illegal in [
        serde_json::json!({"version": 2, "ptyInstance": "pty-7"}),
        serde_json::json!({"version": 1}),
        serde_json::json!({"version": 1, "ptyInstance": ""}),
        serde_json::json!({"version": 1, "ptyInstance": "x".repeat(257)}),
        serde_json::json!({"version": 1, "ptyInstance": 7}),
        serde_json::json!({"ptyInstance": "pty-7"}),
    ] {
        assert_eq!(
            super::negotiated_host_shell(Some(&meta(illegal.clone()))),
            None,
            "{illegal:?}"
        );
    }
}

#[test]
fn the_trusted_mandatory_ask_flag_is_projected_and_fails_closed() {
    let ask = |flag: serde_json::Value| {
        let mut request = serde_json::json!({
            "subtype": "can_use_tool",
            "tool_name": "shell",
            "input": {"command": "echo hi"},
            "tool_use_id": "call-1",
        });
        if !flag.is_null() {
            request["hook_requires_approval"] = flag;
        }
        serde_json::json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": request,
        })
    };
    let flag_of = |value: serde_json::Value| {
        let mut projector = super::projection::Projector::new();
        let projected = projector.project(&value);
        match projected.into_iter().next() {
            Some(Projected::Permission(ask)) => Some(ask.hook_requires_approval),
            _ => None,
        }
    };
    // The kernel is the identified trusted source and legally omits false.
    assert_eq!(flag_of(ask(serde_json::Value::Null)), Some(false));
    assert_eq!(flag_of(ask(serde_json::json!(true))), Some(true));
    assert_eq!(flag_of(ask(serde_json::json!(false))), Some(false));
    // A malformed value is not a legal omission, so it fails closed.
    assert_eq!(flag_of(ask(serde_json::json!("true"))), Some(true));
    assert_eq!(flag_of(ask(serde_json::json!(1))), Some(true));
}

#[tokio::test]
async fn a_negotiated_shell_action_runs_once_on_the_host_and_never_natively() {
    let workspace = tempfile::tempdir().expect("temp workspace");
    let marker = workspace.path().join("native-marker");
    let command = format!("touch {}", marker.display());
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        &serde_json::json!({"command": command}).to_string(),
        "done",
    ));
    let cwd = workspace.path().to_path_buf();
    let session_cwd = cwd.clone();
    let (reason, _updates, executes) =
        with_host_shell_client(runtime, allow_executes(), async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                session_cwd,
            )
            .await?;
            let response = connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await?;
            Ok(response.stop_reason)
        })
        .await;

    assert_eq!(reason, StopReason::EndTurn);
    let seen = executes.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "exactly one foreground execute: {seen:?}");
    assert_eq!(seen[0]["toolCallId"], "call-1");
    assert_eq!(seen[0]["command"], command);
    assert_eq!(seen[0]["ptyInstance"], "pty-7");
    assert_eq!(seen[0]["hookRequiresApproval"], false);
    assert_eq!(seen[0]["cwd"], cwd.to_string_lossy().to_string());
    assert!(seen[0]["sessionId"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert!(
        !marker.exists(),
        "the kernel must not also run the action natively"
    );
}

#[tokio::test]
async fn a_refused_foreground_execute_denies_the_call_without_native_fallback() {
    let workspace = tempfile::tempdir().expect("temp workspace");
    let marker = workspace.path().join("native-marker");
    let command = format!("touch {}", marker.display());
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        &serde_json::json!({"command": command}).to_string(),
        "done",
    ));
    let refuse: HostReply = Arc::new(|_| Err(Error::new(-32011, "the user refused this action")));
    let (reason, _updates, executes) =
        with_host_shell_client(runtime, refuse, async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                std::env::temp_dir(),
            )
            .await?;
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await?
                .stop_reason)
        })
        .await;

    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(executes.lock().unwrap().len(), 1);
    assert!(
        !marker.exists(),
        "a refusal must never fall back to native execution"
    );
}

#[tokio::test]
async fn an_unknown_foreground_execute_fails_the_turn_closed_without_replay() {
    let workspace = tempfile::tempdir().expect("temp workspace");
    let marker = workspace.path().join("native-marker");
    let command = format!("touch {}", marker.display());
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        &serde_json::json!({"command": command}).to_string(),
        "done",
    ));
    // -32012 is the host's UNKNOWN: ownership was lost after a possible
    // dispatch, so the action may still have run. The owned notification is
    // emitted first, so this is not the "never owned" refusal shape.
    let unknown: HostReply = Arc::new(|_| {
        Err(Error::new(
            -32012,
            "the foreground lost ownership after a possible dispatch",
        ))
    });
    let probe = Arc::clone(&runtime);
    let reader = Arc::clone(&runtime);
    let (response, updates, executes) = with_host_shell_client_owned(
        runtime,
        unknown,
        OwnedPolicy::Matching,
        std::time::Duration::ZERO,
        async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                std::env::temp_dir(),
            )
            .await?;
            let handle = session_core(&probe);
            let mut core = handle.lock().await;
            core.audit = CoreAuditRecorder::test_capture(&core.session_id);
            drop(core);
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await)
        },
    )
    .await;

    assert_eq!(
        executes.lock().unwrap().len(),
        1,
        "an unknown outcome must not be replayed"
    );
    assert!(
        !marker.exists(),
        "an unknown outcome must never fall back to native execution"
    );
    assert!(
        !joined_text(&updates).contains("done"),
        "the model must not advance after an unknown foreground outcome: {:?}",
        joined_text(&updates)
    );
    let terminal = tool_updates(&updates)
        .into_iter()
        .rfind(|(id, _)| id == "call-1")
        .and_then(|(_, status)| status);
    assert!(
        matches!(
            terminal,
            Some(ToolCallStatus::Completed) | Some(ToolCallStatus::Failed)
        ),
        "the declared call must stay paired with a terminal status: {terminal:?}"
    );
    let error = response
        .expect_err("an unknown foreground outcome must not end the turn as end_turn or cancelled");
    assert!(
        error.message.contains("unknown"),
        "the terminal must keep the unknown fact: {}",
        error.message
    );
    assert!(
        !error.message.contains("did not reach a decision"),
        "an unknown outcome must not be reported as a definite non-execution: {}",
        error.message
    );

    // The wire text is only half the contract: the durable record must not
    // relabel "may still have run" as a policy or user denial either.
    let handle = session_core(&reader);
    let core = handle.lock().await;
    let events = core.audit.captured_events();
    let resolved = events
        .iter()
        .find(|event| event.event_type.as_str() == "approval.resolved")
        .expect("the unknown approval must still be audited as resolved");
    assert_eq!(
        resolved.outcome.status,
        AuditOutcomeStatus::Failed,
        "an unknown outcome is a failure, never a denial: {:?}",
        resolved.outcome
    );
    assert_eq!(
        resolved.data().get("decision").and_then(|v| v.as_str()),
        Some("host_execute_unknown"),
        "the audit decision must keep the unknown fact"
    );
    assert!(
        resolved.identity.request_id.is_some(),
        "the audit record must carry the request id"
    );
    assert_eq!(
        core.metrics.approval_deny, 0,
        "an unknown outcome must not be counted as a denial"
    );
    assert_eq!(core.metrics.approval_unknown, 1);
    assert_eq!(
        core.audit.captured_tool_event_types("call-1").last(),
        Some(&"tool.failed"),
        "the call must not be audited as a definite non-execution: {:?}",
        core.audit.captured_tool_event_types("call-1")
    );
    let turn_failed = events
        .iter()
        .find(|event| event.event_type.as_str() == "turn.failed")
        .expect("the turn must be audited as failed");
    assert_eq!(
        turn_failed
            .data()
            .get("reason_code")
            .and_then(|v| v.as_str()),
        Some("host_execute_unknown")
    );
}

#[tokio::test]
async fn a_cancel_before_the_host_terminal_keeps_the_unknown_execute_fatal() {
    let workspace = tempfile::tempdir().expect("temp workspace");
    let marker = workspace.path().join("native-marker");
    let command = format!("touch {}", marker.display());
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        &serde_json::json!({"command": command}).to_string(),
        "done",
    ));
    let probe = Arc::clone(&runtime);
    let reader = Arc::clone(&runtime);
    let owned_seen = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let gate_seen = Arc::clone(&owned_seen);
    let gate_release = Arc::clone(&release);
    let cwd = workspace.path().to_path_buf();
    let unknown: HostReply = Arc::new(|_| {
        Err(Error::new(
            -32012,
            "the foreground lost ownership after a possible dispatch",
        ))
    });
    let (response, updates, executes) = with_host_shell_client_gated(
        runtime,
        unknown,
        OwnedPolicy::Matching,
        std::time::Duration::ZERO,
        Some((gate_seen, gate_release)),
        async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                cwd,
            )
            .await?;
            let handle = session_core(&probe);
            let mut core = handle.lock().await;
            core.audit = CoreAuditRecorder::test_capture(&core.session_id);
            drop(core);
            let pending = connection.send_request(prompt(session.clone(), "run it"));
            // Ownership is on the wire and the execute is still in flight. The
            // host has produced no fact proving the action terminated, so the
            // cancel below cannot be treated as a clean cancellation.
            owned_seen.notified().await;
            connection.send_notification(
                agent_client_protocol::v1::schema::CancelNotification::new(session),
            )?;
            // The cancel is processed before the host terminal is released.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            release.notify_one();
            Ok(pending.block_task().await)
        },
    )
    .await;

    assert_eq!(
        executes.lock().unwrap().len(),
        1,
        "a cancelled unknown execute must not be replayed"
    );
    assert!(
        !marker.exists(),
        "a cancelled unknown execute must never fall back to native execution"
    );
    assert!(
        !joined_text(&updates).contains("done"),
        "the model must not advance after a cancelled unknown execute: {:?}",
        joined_text(&updates)
    );
    let error = response.expect_err(
        "a cancel with no host termination proof must not report a normal cancelled turn",
    );
    assert!(
        error.message.contains("unknown"),
        "the terminal must keep the unknown fact: {}",
        error.message
    );

    // The wire terminal and the durable record must agree: neither may call
    // this a clean cancellation while the action may still have run.
    let handle = session_core(&reader);
    let core = handle.lock().await;
    let events = core.audit.captured_events();
    let resolved = events
        .iter()
        .find(|event| event.event_type.as_str() == "approval.resolved")
        .expect("the unresolved approval must still be audited as resolved");
    assert_eq!(
        resolved.outcome.status,
        AuditOutcomeStatus::Failed,
        "an owned execute with no termination proof is not a clean cancel: {:?}",
        resolved.outcome
    );
    assert_eq!(
        resolved.data().get("decision").and_then(|v| v.as_str()),
        Some("host_execute_unknown"),
        "the audit decision must keep the unknown fact"
    );
    assert_eq!(
        core.metrics.approval_deny, 0,
        "an unknown outcome must not be counted as a denial"
    );
    assert_eq!(core.metrics.approval_unknown, 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type.as_str() == "approval.resolved")
            .count(),
        1,
        "the racing branches must resolve the approval exactly once"
    );
    let turn_failed = events
        .iter()
        .find(|event| event.event_type.as_str() == "turn.failed")
        .expect("the turn must be audited as failed");
    assert_eq!(
        turn_failed.outcome.status,
        AuditOutcomeStatus::Failed,
        "the audited turn terminal must match the wire terminal: {:?}",
        turn_failed.outcome
    );
    assert_eq!(
        turn_failed
            .data()
            .get("reason_code")
            .and_then(|v| v.as_str()),
        Some("host_execute_unknown")
    );
}

#[tokio::test]
async fn without_negotiation_the_shell_tool_is_not_registered() {
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        r#"{"command":"echo hi"}"#,
        "done",
    ));
    let (reason, _updates, executes) =
        with_host_shell_client(runtime, allow_executes(), async move |connection| {
            // No capability advertisement: the extension is not negotiated.
            let session = start_session(&connection).await?;
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await?
                .stop_reason)
        })
        .await;

    assert_eq!(reason, StopReason::EndTurn);
    assert!(
        executes.lock().unwrap().is_empty(),
        "an unnegotiated connection must not receive a foreground execute"
    );
}

#[tokio::test]
async fn a_hook_ask_reaches_the_wire_as_a_trusted_mandatory_confirmation() {
    let mut config = CoreConfig::default();
    // Trust mode: only the hook asks, so a pass here also pins that the hook's
    // ask is not auto-approved away before it reaches the foreground request.
    config.agent.approval_mode = crate::config::ApprovalMode::Trust;
    config.hooks.enabled = true;
    config.hooks.pre_tool_use = vec![crate::config::HookDefinition {
        command: r#"python3 -c 'print("""{"decision":"ask","reason":"needs review"}""")'"#
            .to_string(),
        name: Some("ask-hook".to_string()),
        matcher: None,
        timeout: Some(10_000),
        sequential: None,
        fail_open: false,
        require_output_when_env: None,
        env: Default::default(),
    }];
    let runtime = runtime_with_config(
        config,
        tool_then_text("shell", "call-1", r#"{"command":"echo hi"}"#, "done"),
    );
    let (reason, _updates, executes) =
        with_host_shell_client(runtime, allow_executes(), async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                std::env::temp_dir(),
            )
            .await?;
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await?
                .stop_reason)
        })
        .await;

    assert_eq!(reason, StopReason::EndTurn);
    let seen = executes.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "exactly one foreground execute: {seen:?}");
    assert_eq!(
        seen[0]["hookRequiresApproval"], true,
        "the hook's ask must survive projection onto the final action"
    );
}

#[tokio::test]
async fn a_local_auto_allow_cannot_take_a_negotiated_shell_action_native() {
    let workspace = tempfile::tempdir().expect("temp workspace");
    let marker = workspace.path().join("native-marker");
    let command = format!("touch {}", marker.display());
    let mut config = CoreConfig::default();
    config.agent.approval_mode = crate::config::ApprovalMode::Auto;
    config.agent.allowed_tools.insert("shell".to_string());
    let runtime = runtime_with_config(
        config,
        tool_then_text(
            "shell",
            "call-1",
            &serde_json::json!({"command": command}).to_string(),
            "done",
        ),
    );
    let session_cwd = workspace.path().to_path_buf();
    let (reason, _updates, executes) =
        with_host_shell_client(runtime, allow_executes(), async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                session_cwd,
            )
            .await?;
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await?
                .stop_reason)
        })
        .await;

    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(executes.lock().unwrap().len(), 1);
    assert!(
        !marker.exists(),
        "a configured auto-allow must not bypass the negotiated foreground handoff"
    );
}

/// Runs one foreground action with a 1s residual deadline and a slow peer.
///
/// The peer answers after the deadline, so the only thing that can keep the
/// kernel waiting is the ownership receipt. Without it the kernel ends the turn
/// with a timeout error instead of accepting a late decision.
async fn foreground_outcome_with_owned(owned: OwnedPolicy) -> (Result<StopReason, Error>, usize) {
    let _guard = crate::core::tests::APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        r#"{"command":"echo hi"}"#,
        "done",
    ));
    let (reason, _updates, executes) = with_host_shell_client_owned(
        runtime,
        allow_executes(),
        owned,
        std::time::Duration::from_millis(1500),
        async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                std::env::temp_dir(),
            )
            .await?;
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await
                .map(|response| response.stop_reason))
        },
    )
    .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");
    let seen = executes.lock().unwrap().len();
    (reason, seen)
}

#[tokio::test]
async fn the_ownership_notification_disarms_the_residual_deadline() {
    let (reason, seen) = foreground_outcome_with_owned(OwnedPolicy::Matching).await;
    assert_eq!(seen, 1, "exactly one foreground execute");
    assert_eq!(
        reason.expect("owned lets the slow foreground result land"),
        StopReason::EndTurn
    );
}

#[tokio::test]
async fn without_the_ownership_notification_the_residual_deadline_still_fires() {
    let (reason, seen) = foreground_outcome_with_owned(OwnedPolicy::Absent).await;
    assert_eq!(seen, 1, "exactly one foreground execute");
    let error = reason.expect_err("sending the request alone must not disarm the guard");
    assert!(
        error.message.contains("approval timed out"),
        "expected the residual deadline, got {error:?}"
    );
}

#[tokio::test]
async fn an_owned_notification_for_another_action_does_not_disarm_the_deadline() {
    let (reason, seen) = foreground_outcome_with_owned(OwnedPolicy::Mismatched).await;
    assert_eq!(seen, 1, "exactly one foreground execute");
    let error = reason.expect_err("an unrelated owned must not disarm this action");
    assert!(
        error.message.contains("approval timed out"),
        "expected the residual deadline, got {error:?}"
    );
}

#[test]
fn ownership_is_consumed_only_for_the_action_it_names() {
    let ownership = super::PendingOwnership::default();
    let (input, _server) = tokio::io::duplex(64);
    let turn = Arc::new(super::TurnControl::new(input));
    let session = agent_client_protocol::v1::schema::SessionId::new("session-1".to_string());
    ownership.register(super::PendingOwned {
        session_id: session.clone(),
        tool_call_id: "call-1".to_string(),
        request_id: "req-1".to_string(),
        turn: Arc::clone(&turn),
    });
    assert!(ownership.take(&session, "call-other").is_none());
    assert!(ownership
        .take(
            &agent_client_protocol::v1::schema::SessionId::new("session-2".to_string()),
            "call-1",
        )
        .is_none());
    let taken = ownership.take(&session, "call-1").expect("matching action");
    assert_eq!(taken.request_id, "req-1");
    assert!(ownership.take(&session, "call-1").is_none(), "one shot");
}

#[test]
fn clearing_ownership_leaves_another_action_registered() {
    let ownership = super::PendingOwnership::default();
    let (input, _server) = tokio::io::duplex(64);
    let turn = Arc::new(super::TurnControl::new(input));
    let session = agent_client_protocol::v1::schema::SessionId::new("session-1".to_string());
    ownership.register(super::PendingOwned {
        session_id: session.clone(),
        tool_call_id: "call-1".to_string(),
        request_id: "req-1".to_string(),
        turn: Arc::clone(&turn),
    });
    ownership.clear("call-other");
    assert!(ownership.take(&session, "call-1").is_some());
    ownership.register(super::PendingOwned {
        session_id: session.clone(),
        tool_call_id: "call-2".to_string(),
        request_id: "req-2".to_string(),
        turn,
    });
    ownership.clear("call-2");
    assert!(ownership.take(&session, "call-2").is_none());
}

/// Runs one foreground action against a controlled host reply and returns the
/// wire result together with the kernel that recorded the decision.
async fn host_reply_round(
    reply: HostReply,
) -> (
    Result<agent_client_protocol::v1::schema::PromptResponse, Error>,
    Updates,
    HostExecutes,
    Arc<AcpRuntime>,
) {
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-1",
        r#"{"command":"echo hi"}"#,
        "done",
    ));
    let probe = Arc::clone(&runtime);
    let reader = Arc::clone(&runtime);
    let (response, updates, executes) = with_host_shell_client_owned(
        runtime,
        reply,
        OwnedPolicy::Matching,
        std::time::Duration::ZERO,
        async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-7"}),
                std::env::temp_dir(),
            )
            .await?;
            let handle = session_core(&probe);
            let mut core = handle.lock().await;
            core.audit = CoreAuditRecorder::test_capture(&core.session_id);
            drop(core);
            Ok(connection
                .send_request(prompt(session, "run it"))
                .block_task()
                .await)
        },
    )
    .await;
    (response, updates, executes, reader)
}

/// Asserts the shared UNKNOWN contract: no replay, no model advance, and a
/// durable record that keeps "may still have run" instead of a denial.
async fn assert_unknown_contract(
    response: Result<agent_client_protocol::v1::schema::PromptResponse, Error>,
    updates: &Updates,
    executes: &HostExecutes,
    reader: &Arc<AcpRuntime>,
) {
    assert_eq!(
        executes.lock().unwrap().len(),
        1,
        "an uncertain outcome must not be replayed"
    );
    assert!(
        !joined_text(updates).contains("done"),
        "the model must not advance on an uncertain foreground outcome: {:?}",
        joined_text(updates)
    );
    let error = response.expect_err("an uncertain outcome must not end the turn as end_turn");
    assert!(
        error.message.contains("unknown"),
        "the terminal must keep the unknown fact: {}",
        error.message
    );
    let handle = session_core(reader);
    let core = handle.lock().await;
    assert_eq!(
        core.metrics.approval_deny, 0,
        "an uncertain outcome must not be counted as a definite non-execution"
    );
    assert_eq!(core.metrics.approval_unknown, 1);
    let events = core.audit.captured_events();
    let resolved = events
        .iter()
        .find(|event| event.event_type.as_str() == "approval.resolved")
        .expect("the approval must still be audited as resolved");
    assert_eq!(
        resolved.data().get("decision").and_then(|v| v.as_str()),
        Some("host_execute_unknown"),
        "the audit decision must keep the unknown fact: {:?}",
        resolved.outcome
    );
}

#[tokio::test]
async fn a_transport_failure_after_ownership_is_unknown_not_a_denial() {
    // The SDK reports a lost connection as an internal error, never as the
    // host's own -32012, so the code alone cannot decide this branch.
    let reply: HostReply = Arc::new(|_| Err(Error::internal_error()));
    let (response, updates, executes, reader) = host_reply_round(reply).await;
    assert_unknown_contract(response, &updates, &executes, &reader).await;
}

#[tokio::test]
async fn a_malformed_success_payload_is_unknown_not_an_executed_result() {
    let reply: HostReply = Arc::new(|_| Ok(serde_json::json!("ok")));
    let (response, updates, executes, reader) = host_reply_round(reply).await;
    assert_unknown_contract(response, &updates, &executes, &reader).await;
}

#[tokio::test]
async fn a_non_terminal_status_in_a_success_payload_is_unknown() {
    let reply: HostReply = Arc::new(|_| {
        let mut outcome = host_outcome();
        outcome["status"] = serde_json::json!("unknown");
        Ok(outcome)
    });
    let (response, updates, executes, reader) = host_reply_round(reply).await;
    assert_unknown_contract(response, &updates, &executes, &reader).await;
}

#[tokio::test]
async fn a_completed_result_without_an_exit_code_is_neither_success_nor_unknown() {
    let reply: HostReply = Arc::new(|_| {
        let mut outcome = host_outcome();
        outcome
            .as_object_mut()
            .expect("the fixture outcome is an object")
            .remove("exit_code");
        Ok(outcome)
    });
    let (response, updates, executes, reader) = host_reply_round(reply).await;
    assert_eq!(executes.lock().unwrap().len(), 1, "no replay");
    assert_eq!(
        response
            .expect("a proven-finished action still settles the turn")
            .stop_reason,
        StopReason::EndTurn
    );
    assert!(
        joined_text(&updates).contains("done"),
        "a proven-finished action lets the model advance: {:?}",
        joined_text(&updates)
    );
    let handle = session_core(&reader);
    let core = handle.lock().await;
    assert_eq!(
        core.metrics.approval_unknown, 0,
        "a proven-finished action must not be conflated with a possible run"
    );
    assert_eq!(
        core.audit.captured_tool_event_types("call-1").last(),
        Some(&"tool.failed"),
        "a missing exit code must not be reported as success: {:?}",
        core.audit.captured_tool_event_types("call-1")
    );
}

#[tokio::test]
async fn every_frozen_terminal_status_still_settles_the_turn() {
    // The terminal whitelist must cover the Shell's whole vocabulary, including
    // the untracked handoff, or a legitimate result would fail the turn closed.
    for status in [
        "completed",
        "completed_untracked",
        "failed",
        "interrupted",
        "timed_out",
        "not_executed",
    ] {
        let reply: HostReply = Arc::new(move |_| {
            let mut outcome = host_outcome();
            outcome["status"] = serde_json::json!(status);
            Ok(outcome)
        });
        let (response, updates, executes, reader) = host_reply_round(reply).await;
        assert_eq!(executes.lock().unwrap().len(), 1, "{status}: no replay");
        assert_eq!(
            response
                .unwrap_or_else(|error| panic!("{status} must settle the turn: {error}"))
                .stop_reason,
            StopReason::EndTurn,
            "{status} must stay a terminal fact"
        );
        assert!(
            joined_text(&updates).contains("done"),
            "{status} must let the model advance: {:?}",
            joined_text(&updates)
        );
        let handle = session_core(&reader);
        let core = handle.lock().await;
        assert_eq!(
            core.metrics.approval_unknown, 0,
            "{status} is not uncertain"
        );
        let expected = if matches!(status, "completed" | "completed_untracked") {
            "tool.completed"
        } else {
            "tool.failed"
        };
        assert_eq!(
            core.audit.captured_tool_event_types("call-1").last(),
            Some(&expected),
            "{status} must keep its own success or failure fact: {:?}",
            core.audit.captured_tool_event_types("call-1")
        );
    }
}

#[tokio::test]
async fn a_proven_failure_status_is_not_success_even_with_a_reported_zero_exit() {
    // A timed-out, interrupted or unexecuted action did not succeed. A recorded
    // zero exit contradicts that status and must not turn it into a success,
    // and the action is still proven finished, so it must not become UNKNOWN.
    for status in ["failed", "interrupted", "timed_out", "not_executed"] {
        let reply: HostReply = Arc::new(move |_| {
            let mut outcome = host_outcome();
            outcome["status"] = serde_json::json!(status);
            outcome["exit_code"] = serde_json::json!(0);
            Ok(outcome)
        });
        let (response, updates, executes, reader) = host_reply_round(reply).await;
        assert_eq!(executes.lock().unwrap().len(), 1, "{status}: no replay");
        assert_eq!(
            response
                .unwrap_or_else(|error| panic!("{status} must settle the turn: {error}"))
                .stop_reason,
            StopReason::EndTurn,
            "{status} is proven finished and must still settle"
        );
        assert!(
            joined_text(&updates).contains("done"),
            "{status} must let the model advance: {:?}",
            joined_text(&updates)
        );
        let handle = session_core(&reader);
        let core = handle.lock().await;
        assert_eq!(
            core.metrics.approval_unknown, 0,
            "{status} must not be conflated with a possible run"
        );
        assert_eq!(
            core.audit.captured_tool_event_types("call-1").last(),
            Some(&"tool.failed"),
            "{status} must not be reported as success: {:?}",
            core.audit.captured_tool_event_types("call-1")
        );
    }
}

/// Records the tool inventory declared on each model step.
struct DeclarationSpy {
    seen: Arc<Mutex<Vec<Vec<String>>>>,
    inner: Arc<dyn ContentGenerator>,
}

#[async_trait]
impl ContentGenerator for DeclarationSpy {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        let mut names: Vec<_> = tools.iter().map(|tool| tool.name.clone()).collect();
        names.sort();
        self.seen.lock().unwrap().push(names);
        self.inner.generate(messages, tools, config).await
    }

    fn cancel(&self) {
        self.inner.cancel();
    }
}

fn declaration_spy(
    inner: Arc<dyn ContentGenerator>,
) -> (Arc<DeclarationSpy>, Arc<Mutex<Vec<Vec<String>>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(DeclarationSpy {
        seen: Arc::clone(&seen),
        inner,
    });
    (provider, seen)
}

/// A prompt carrying the per-turn read-only restriction under `_meta`.
fn restricted_prompt(
    session_id: agent_client_protocol::v1::schema::SessionId,
    text: &str,
) -> PromptRequest {
    let mut request = prompt(session_id, text);
    let mut meta = agent_client_protocol::v1::schema::Meta::new();
    meta.insert(
        "_cosh/turn".to_string(),
        serde_json::json!({"version": 1, "readOnly": true}),
    );
    request.meta = Some(meta);
    request
}

#[tokio::test]
async fn initialize_advertises_the_read_only_turn_capability() {
    let (response, _updates) = with_client(text_runtime("unused"), async move |connection| {
        connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await
    })
    .await;
    let meta = response
        .agent_capabilities
        .meta
        .expect("the agent must advertise the restriction it can enforce");
    assert_eq!(
        meta.get("_cosh/turn"),
        Some(&serde_json::json!({"version": 1, "readOnly": true})),
        "a client may only send a restricted turn to an agent that promised it: {meta:?}"
    );
}

#[tokio::test]
async fn a_read_only_turn_declares_only_read_only_tools() {
    let (provider, seen) = declaration_spy(Arc::new(MockProvider::text_only("done")));
    let (reason, _updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        connection
            .send_request(restricted_prompt(session, "analyse the evidence"))
            .block_task()
            .await
            .map(|response| response.stop_reason)
    })
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![vec!["read_file".to_string()]],
        "a restricted turn must not declare a write, a shell or a question tool"
    );
}

#[tokio::test]
async fn a_read_only_turn_refuses_a_write_even_when_the_peer_would_allow_it() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("out.txt");
    let provider = tool_then_text(
        "write_file",
        "call-write",
        r#"{"path":"out.txt","content":"must not appear"}"#,
        "done",
    );
    let cwd = dir.path().to_path_buf();
    let (reason, updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::ALLOW_ONCE),
        async move |connection| {
            let session = start_session_in(&connection, cwd).await?;
            connection
                .send_request(restricted_prompt(session, "analyse the evidence"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert!(
        !target.exists(),
        "a restricted turn must not write the file"
    );
    assert!(
        asks.lock().unwrap().is_empty(),
        "a restricted turn must refuse the call, not ask the peer to allow it"
    );
    assert_eq!(
        tool_updates(&updates).last(),
        Some(&("call-write".to_string(), Some(ToolCallStatus::Failed))),
        "the refusal must be visible on the wire: {:?}",
        tool_updates(&updates)
    );
}

#[tokio::test]
async fn a_read_only_turn_refuses_an_mcp_tool() {
    let (_dir, server) = mcp_fixture();
    let provider = tool_then_text(
        "mcp__fixture__echo",
        "call-mcp",
        r#"{"message":"hello"}"#,
        "done",
    );
    let cwd = std::env::temp_dir();
    let (reason, updates, asks) = with_permission_client(
        runtime_with(provider),
        |_| selected(super::ALLOW_ONCE),
        async move |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = connection
                .send_request(NewSessionRequest::new(cwd).mcp_servers(vec![server]))
                .block_task()
                .await?
                .session_id;
            connection
                .send_request(restricted_prompt(session, "analyse the evidence"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        },
    )
    .await;
    assert_eq!(reason, StopReason::EndTurn);
    assert!(
        asks.lock().unwrap().is_empty(),
        "an MCP side effect must be refused, not offered for approval"
    );
    let statuses = tool_updates(&updates);
    let recorded = updates.lock().unwrap();
    assert!(
        !recorded.iter().any(|update| match update {
            SessionUpdate::ToolCallUpdate(update) => update
                .fields
                .raw_output
                .as_ref()
                .is_some_and(|output| output.to_string().contains("mcp evidence")),
            _ => false,
        }),
        "the MCP tool must not run: {recorded:?}"
    );
    assert_eq!(
        statuses.last(),
        Some(&("call-mcp".to_string(), Some(ToolCallStatus::Failed)))
    );
}

#[tokio::test]
async fn a_read_only_turn_refuses_a_foreground_shell_action() {
    let workspace = tempfile::tempdir().expect("temp workspace");
    let marker = workspace.path().join("native-marker");
    let command = format!("touch {}", marker.display());
    let runtime = runtime_with(tool_then_text(
        "shell",
        "call-shell",
        &serde_json::json!({"command": command}).to_string(),
        "done",
    ));
    let cwd = workspace.path().to_path_buf();
    let (reason, updates, executes) =
        with_host_shell_client(runtime, allow_executes(), async move |connection| {
            let session = start_host_shell_session(
                &connection,
                serde_json::json!({"version": 1, "ptyInstance": "pty-9"}),
                cwd,
            )
            .await?;
            connection
                .send_request(restricted_prompt(session, "analyse the evidence"))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        })
        .await;
    assert_eq!(reason, StopReason::EndTurn);
    let handed_off = executes.lock().unwrap().clone();
    assert!(
        handed_off.is_empty(),
        "a restricted turn must not hand an action to the foreground: {handed_off:?}"
    );
    assert!(!marker.exists(), "the action must not run natively either");
    assert_eq!(
        tool_updates(&updates).last(),
        Some(&("call-shell".to_string(), Some(ToolCallStatus::Failed)))
    );
}

#[tokio::test]
async fn the_next_ordinary_turn_restores_the_full_tool_inventory() {
    let (provider, seen) = declaration_spy(Arc::new(MockProvider::new(vec![
        vec![
            GenerateEvent::TextDelta("analysed".to_string()),
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("resumed".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ])));
    let (reasons, _updates) = with_client(runtime_with(provider), async move |connection| {
        let session = start_session(&connection).await?;
        let restricted = connection
            .send_request(restricted_prompt(session.clone(), "analyse the evidence"))
            .block_task()
            .await?
            .stop_reason;
        let ordinary = connection
            .send_request(prompt(session, "now continue"))
            .block_task()
            .await?
            .stop_reason;
        Ok((restricted, ordinary))
    })
    .await;
    assert_eq!(reasons, (StopReason::EndTurn, StopReason::EndTurn));
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![
            vec!["read_file".to_string()],
            vec![
                "ask_user_question".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
            ],
        ],
        "the restriction must last one turn only"
    );
}

#[tokio::test]
async fn an_unverifiable_read_only_meta_is_rejected_instead_of_ignored() {
    for (name, payload) in [
        (
            "version",
            serde_json::json!({"version": 2, "readOnly": true}),
        ),
        ("flag", serde_json::json!({"version": 1, "readOnly": false})),
        ("shape", serde_json::json!("yes")),
        ("missing", serde_json::json!({"version": 1})),
    ] {
        let (provider, seen) = declaration_spy(Arc::new(MockProvider::text_only("done")));
        let (error, _updates) = with_client(runtime_with(provider), async move |connection| {
            let session = start_session(&connection).await?;
            let mut request = prompt(session, "analyse the evidence");
            let mut meta = agent_client_protocol::v1::schema::Meta::new();
            meta.insert("_cosh/turn".to_string(), payload);
            request.meta = Some(meta);
            Ok::<_, Error>(
                connection
                    .send_request(request)
                    .block_task()
                    .await
                    .expect_err("an unverifiable restriction must not start a turn"),
            )
        })
        .await;
        assert_eq!(error.code, ErrorCode::InvalidParams, "{name}: {error:?}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "{name}: no model step may run under an unverifiable restriction"
        );
    }
}

/// One durable envelope the settlement seam committed for `session_id`.
fn durable_envelope(store: &std::path::Path, session_id: &str) -> Option<std::path::PathBuf> {
    let mut found = Vec::new();
    for scope in std::fs::read_dir(store).expect("session store root") {
        let path = scope
            .expect("scope entry")
            .path()
            .join(format!("{session_id}.json"));
        if path.is_file() {
            found.push(path);
        }
    }
    assert!(
        found.len() <= 1,
        "more than one durable envelope for {session_id}"
    );
    found.pop()
}

fn committed_envelope(store: &std::path::Path, session_id: &str) -> serde_json::Value {
    let path = durable_envelope(store, session_id).expect("a committed durable envelope");
    serde_json::from_slice(&std::fs::read(&path).expect("envelope bytes")).expect("envelope JSON")
}

fn message_text(message: &serde_json::Value) -> String {
    match &message["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
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

fn message_roles(envelope: &serde_json::Value) -> Vec<String> {
    envelope["messages"]
        .as_array()
        .expect("transcript array")
        .iter()
        .map(|message| message["role"].as_str().expect("role").to_string())
        .collect()
}

/// A workspace and session store the ACP entry has to commit into.
struct SettlementFixture {
    _temp: tempfile::TempDir,
    workspace: std::path::PathBuf,
    store: std::path::PathBuf,
    config: CoreConfig,
}

fn settlement_fixture() -> SettlementFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut config = CoreConfig::default();
    config.session.auto_persist = true;
    config.session.persist_dir = store.display().to_string();
    SettlementFixture {
        _temp: temp,
        workspace,
        store,
        config,
    }
}

/// Occupies the scoped storage path with a regular file after the store was
/// opened, so the next commit fails even when the test runs as root.
fn block_scope_directory(store: &std::path::Path, workspace: &std::path::Path) {
    use sha2::Digest as _;
    let canonical = std::fs::canonicalize(workspace).expect("canonical workspace");
    let scope = hex::encode(sha2::Sha256::digest(
        canonical.to_str().expect("utf-8 workspace").as_bytes(),
    ));
    std::fs::create_dir_all(store).expect("store root");
    std::fs::write(store.join(&scope[..24]), b"not a directory").expect("block scope directory");
}

#[tokio::test]
async fn an_acp_turn_commits_the_transcript_through_the_shared_finalizer() {
    let fixture = settlement_fixture();
    let runtime = runtime_with_config(
        fixture.config.clone(),
        Arc::new(MockProvider::history_echo()),
    );
    let workspace = fixture.workspace.clone();
    let (session, _updates) = with_client(runtime, async move |connection| {
        let session = start_session_in(&connection, workspace).await?;
        let response = connection
            .send_request(prompt(session.clone(), "parity seed"))
            .block_task()
            .await?;
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        Ok::<_, Error>(session)
    })
    .await;
    let id = session.0.to_string();
    let envelope = committed_envelope(&fixture.store, &id);
    assert_eq!(envelope["session_id"], id);
    assert_eq!(
        envelope["workspace_scope"],
        std::fs::canonicalize(&fixture.workspace)
            .expect("canonical workspace")
            .display()
            .to_string()
    );
    assert_eq!(envelope["generation"], 1);
    assert_eq!(message_roles(&envelope), ["user", "assistant"]);
    assert_eq!(message_text(&envelope["messages"][0]), "parity seed");
    assert_eq!(
        message_text(&envelope["messages"][1]),
        "mock history: parity seed"
    );
}

#[tokio::test]
async fn tool_call_pairing_survives_the_acp_commit() {
    let mut fixture = settlement_fixture();
    let note = fixture.workspace.join("note.txt");
    std::fs::write(&note, "parity tool payload").expect("fixture file");
    fixture
        .config
        .agent
        .allowed_tools
        .insert("read_file".to_string());
    let provider = tool_then_text(
        "read_file",
        "call-parity",
        &serde_json::json!({ "path": note }).to_string(),
        "done",
    );
    let runtime = runtime_with_config(fixture.config.clone(), provider);
    let workspace = fixture.workspace.clone();
    let (session, _updates) = with_client(runtime, async move |connection| {
        let session = start_session_in(&connection, workspace).await?;
        let response = connection
            .send_request(prompt(session.clone(), "read the note"))
            .block_task()
            .await?;
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        Ok::<_, Error>(session)
    })
    .await;
    let envelope = committed_envelope(&fixture.store, session.0.as_ref());
    assert_eq!(
        message_roles(&envelope),
        ["user", "assistant", "tool", "assistant"]
    );
    let call_id = envelope["messages"][1]["tool_calls"][0]["id"]
        .as_str()
        .expect("tool call id");
    assert_eq!(call_id, "call-parity");
    assert_eq!(envelope["messages"][2]["tool_call_id"], call_id);
    assert!(
        message_text(&envelope["messages"][2]).contains("parity tool payload"),
        "tool result must stay paired with its call: {envelope}"
    );
    assert_eq!(message_text(&envelope["messages"][3]), "done");
}

#[tokio::test]
async fn a_cancelled_acp_turn_commits_the_partial_transcript() {
    let fixture = settlement_fixture();
    let (provider, started) = GatedProvider::new();
    let runtime = runtime_with_config(fixture.config.clone(), provider);
    let workspace = fixture.workspace.clone();
    let ((session, reason), _updates) = with_client(runtime, async move |connection| {
        let session = start_session_in(&connection, workspace).await?;
        let pending = connection.send_request(prompt(session.clone(), "cancelled seed"));
        started.notified().await;
        connection.send_notification(
            agent_client_protocol::v1::schema::CancelNotification::new(session.clone()),
        )?;
        let reason = pending.block_task().await?.stop_reason;
        Ok::<_, Error>((session, reason))
    })
    .await;
    assert_eq!(reason, StopReason::Cancelled);
    let envelope = committed_envelope(&fixture.store, session.0.as_ref());
    assert_eq!(message_text(&envelope["messages"][0]), "cancelled seed");
}

#[tokio::test]
async fn a_refused_commit_is_reported_as_an_error_not_a_stop_reason() {
    let fixture = settlement_fixture();
    let runtime = runtime_with_config(
        fixture.config.clone(),
        Arc::new(MockProvider::history_echo()),
    );
    let workspace = fixture.workspace.clone();
    let store = fixture.store.clone();
    let ((session, second), _updates) = with_client(runtime, async move |connection| {
        let session = start_session_in(&connection, workspace).await?;
        connection
            .send_request(prompt(session.clone(), "first"))
            .block_task()
            .await?;
        let path = durable_envelope(&store, session.0.as_ref()).expect("first commit");
        // Another actor reclaimed the durable envelope, so this process still
        // holds generation 1 while the store no longer has it.
        std::fs::remove_file(&path).expect("reclaim the durable envelope");
        let second = connection
            .send_request(prompt(session.clone(), "second"))
            .block_task()
            .await;
        Ok::<_, Error>((session, second))
    })
    .await;
    let error = second.expect_err("a refused commit must not report a stop reason");
    assert!(
        error.message.contains("session persistence failed"),
        "{}",
        error.message
    );
    assert!(
        durable_envelope(&fixture.store, session.0.as_ref()).is_none(),
        "a refused commit must not leave a durable envelope behind"
    );
}

#[tokio::test]
async fn cancellation_does_not_mask_a_persistence_failure() {
    let fixture = settlement_fixture();
    let (provider, started) = GatedProvider::new();
    let runtime = runtime_with_config(fixture.config.clone(), provider);
    let workspace = fixture.workspace.clone();
    let store = fixture.store.clone();
    let ((session, result), _updates) = with_client(runtime, async move |connection| {
        let session = start_session_in(&connection, workspace.clone()).await?;
        block_scope_directory(&store, &workspace);
        let pending = connection.send_request(prompt(session.clone(), "cancelled seed"));
        started.notified().await;
        connection.send_notification(
            agent_client_protocol::v1::schema::CancelNotification::new(session.clone()),
        )?;
        let result = pending.block_task().await;
        Ok::<_, Error>((session, result))
    })
    .await;
    let error = result.expect_err("a lost transcript must not be reported as cancelled");
    assert!(
        error.message.contains("session persistence failed"),
        "{}",
        error.message
    );
    assert!(
        durable_envelope(&fixture.store, session.0.as_ref()).is_none(),
        "the blocked store must not gain an envelope"
    );
}

#[tokio::test]
async fn max_turns_keeps_the_standard_stop_reason_after_a_successful_commit() {
    let mut fixture = settlement_fixture();
    let note = fixture.workspace.join("note.txt");
    std::fs::write(&note, "parity tool payload").expect("fixture file");
    fixture
        .config
        .agent
        .allowed_tools
        .insert("read_file".to_string());
    fixture.config.agent.max_turns = 1;
    let provider = tool_then_text(
        "read_file",
        "call-limit",
        &serde_json::json!({ "path": note }).to_string(),
        "done",
    );
    let runtime = runtime_with_config(fixture.config.clone(), provider);
    let workspace = fixture.workspace.clone();
    let ((session, reason), _updates) = with_client(runtime, async move |connection| {
        let session = start_session_in(&connection, workspace).await?;
        let reason = connection
            .send_request(prompt(session.clone(), "read the note"))
            .block_task()
            .await?
            .stop_reason;
        Ok::<_, Error>((session, reason))
    })
    .await;
    assert_eq!(reason, StopReason::MaxTurnRequests);
    let envelope = committed_envelope(&fixture.store, session.0.as_ref());
    assert_eq!(message_roles(&envelope), ["user", "assistant", "tool"]);
}
