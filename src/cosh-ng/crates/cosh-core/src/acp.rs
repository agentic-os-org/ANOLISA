//! Official ACP v1 agent entry point over the existing turn kernel.
//!
//! This module owns wire types, request correlation and projection only. Turn
//! progression, budgets, tool pairing and terminal state stay in `core`, so the
//! legacy JSONL transport and ACP drive one loop.

#[cfg(test)]
mod backlog_tests;
mod projection;
mod sink;
#[cfg(test)]
mod tests;
mod transport;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ClientNotification, ContentBlock, ContentChunk,
    ErrorCode, Implementation, InitializeRequest, InitializeResponse, LoadSessionRequest,
    LoadSessionResponse, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionKind, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{
    on_receive_notification, on_receive_request, Agent, Client, ConnectTo, Error,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::mpsc;

use crate::cli::ExecutionProfile;
use crate::config::CoreConfig;
use crate::core::CoshCore;
use crate::extension::{RuntimeGeneration, RuntimeSnapshot};
use crate::provider::ContentGenerator;
use crate::session::ProviderSessionId;
use crate::settlement::{combine_turn_and_persist, SessionRuntime};
use crate::tool::{SessionWorkspace, ToolRegistry};

use projection::Projected;
use sink::EventSink;

/// One active session and one in-flight prompt per session.
const MAX_QUEUED_EVENTS: usize = 256;
const MAX_EVENT_LINE_BYTES: usize = 1024 * 1024;
const MAX_INBOUND_FRAME_BYTES: usize = 1024 * 1024;
/// Notifications the official unbounded outgoing queue may hold for one
/// connection while it is still alive. Mirrors the inbound frame bound; the
/// post-EOF close budget only covers the tail after a clean EOF, so without this
/// a peer that stops reading without EOF grows the accepted-not-handed queue.
/// Each notification is already bounded by MAX_EVENT_LINE_BYTES, so the derived
/// worst-case wire volume is the product of the two, not a measured byte gauge.
const MAX_INFLIGHT_FRAMES: usize = 4096;
const MAX_PROMPT_BYTES: usize = 256 * 1024;
const INPUT_BUFFER_BYTES: usize = 64 * 1024;
/// Post-EOF outgoing drain budget. Mirrors the Shell host's unified reclaim
/// grace so both ends of one connection use the same number; the exit oracle
/// asserts the bound, so a change here fails there instead of widening silently.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// Client-provided MCP servers each own a child process, so one session accepts
/// only a bounded number of them.
const MAX_MCP_SERVERS: usize = 8;

/// Permission options offered to the peer. Remembered choices are not offered
/// because this entry point does not persist permission rules.
const ALLOW_ONCE: &str = "allow_once";
const REJECT_ONCE: &str = "reject_once";

/// Reserved JSON-RPC server range, used for a capacity rejection the standard
/// codes do not name. Provisional until the specification assigns one.
const CAPACITY_ERROR_CODE: i32 = -32001;

/// host-shell v1 extension, frozen in design section 6.3. The envelope stays
/// standard ACP; only the payload is Cosh's. cosh-shell is standalone, so both
/// sides spell the same frozen constants rather than sharing a crate.
const HOST_SHELL_CAPABILITY: &str = "_cosh/shell";
const HOST_SHELL_EXECUTE_METHOD: &str = "_cosh/shell/v1/execute";
/// Wire spelling of the ownership notification. The SDK strips the leading
/// underscore when routing extension messages, so the agent matches the routed
/// form below rather than this one.
const HOST_SHELL_OWNED_METHOD: &str = "_cosh/shell/v1/owned";

const HOST_SHELL_VERSION: u64 = 1;

/// Per-turn read-only restriction, frozen in design section 6.4. The envelope
/// stays standard ACP `_meta`; only the payload is Cosh's.
const TURN_CAPABILITY: &str = "_cosh/turn";
const TURN_VERSION: u64 = 1;
const MAX_PTY_INSTANCE_BYTES: usize = 256;
/// The kernel's shell tool name; the only tool routed to the foreground handoff.
const SHELL_TOOL_NAME: &str = "shell";

type ProviderFactory = Arc<dyn Fn(&CoreConfig) -> Box<dyn ContentGenerator> + Send + Sync>;

/// The decision right for a foreground action the host has owned.
///
/// Cancel and the host terminal race for one slot. The winner's resolution is
/// recorded in the same critical section that hands out the right, so the loser
/// can neither submit a second conflicting result nor interrupt a turn that
/// must fail closed. Ownership alone is never proof the action ended.
#[derive(Debug, Default)]
enum HostExecuteSlot {
    /// No owned action is awaiting a terminal fact.
    #[default]
    Idle,
    /// Owned; the approval request whose decision is still unclaimed.
    Pending(String),
    /// Claimed and resolved as UNKNOWN: the action may still have run. Sticky
    /// for the whole turn, because the turn fails closed on it.
    Unknown(String),
    /// Claimed by a definite host terminal fact for this approval request.
    Terminal(String),
}

/// What the cancel branch may do after claiming.
#[derive(Debug)]
enum CancelClaim {
    /// Resolve this approval as UNKNOWN instead of interrupting.
    ResolveUnknown(String),
    /// No owned decision to claim; interrupt as usual.
    Interrupt,
    /// Already UNKNOWN; the turn fails closed on its own.
    Leave,
}

/// What the host terminal branch may do after claiming.
#[derive(Debug)]
enum HostClaim {
    /// This branch owns the decision for this approval request.
    Claimed(String),
    /// Ownership was never registered; answer on the existing never-owned path.
    Unowned,
    /// Another branch already resolved the decision; submit nothing.
    Rejected,
}

/// Cancellation handle for the prompt currently running in a session.
struct TurnControl {
    input: tokio::sync::Mutex<DuplexStream>,
    cancel_requested: AtomicBool,
    host_execute: std::sync::Mutex<HostExecuteSlot>,
}

impl TurnControl {
    fn new(input: DuplexStream) -> Self {
        Self {
            input: tokio::sync::Mutex::new(input),
            cancel_requested: AtomicBool::new(false),
            host_execute: std::sync::Mutex::new(HostExecuteSlot::Idle),
        }
    }

    fn lock_host_execute(&self) -> std::sync::MutexGuard<'_, HostExecuteSlot> {
        self.host_execute
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn mark_host_execute_pending(&self, request_id: &str) {
        let mut slot = self.lock_host_execute();
        // A later action may not clear an UNKNOWN the turn is already failing
        // closed on; every other state hands the slot to the new action.
        if !matches!(*slot, HostExecuteSlot::Unknown(_)) {
            *slot = HostExecuteSlot::Pending(request_id.to_string());
        }
    }

    /// A decided slot is never cleared: a repeated cancel or a late host reply
    /// must not reopen the decision, so every losing arm restores the state it
    /// found instead of leaving the `Idle` default behind.
    fn claim_cancel(&self) -> CancelClaim {
        let mut slot = self.lock_host_execute();
        match std::mem::take(&mut *slot) {
            HostExecuteSlot::Pending(request_id) => {
                *slot = HostExecuteSlot::Unknown(request_id.clone());
                CancelClaim::ResolveUnknown(request_id)
            }
            HostExecuteSlot::Unknown(request_id) => {
                *slot = HostExecuteSlot::Unknown(request_id);
                CancelClaim::Leave
            }
            HostExecuteSlot::Terminal(request_id) => {
                *slot = HostExecuteSlot::Terminal(request_id);
                CancelClaim::Interrupt
            }
            HostExecuteSlot::Idle => CancelClaim::Interrupt,
        }
    }

    /// The decision right belongs to one approval request. A terminal fact for
    /// an earlier action must not swallow the next action's own refusal, while
    /// a late or repeated fact for the same action still claims nothing.
    fn claim_host_execute(&self, request_id: &str, unknown: bool) -> HostClaim {
        let mut slot = self.lock_host_execute();
        match std::mem::take(&mut *slot) {
            HostExecuteSlot::Pending(pending) if pending == request_id => {
                *slot = if unknown {
                    HostExecuteSlot::Unknown(pending.clone())
                } else {
                    HostExecuteSlot::Terminal(pending.clone())
                };
                HostClaim::Claimed(pending)
            }
            HostExecuteSlot::Pending(pending) => {
                *slot = HostExecuteSlot::Pending(pending);
                HostClaim::Rejected
            }
            HostExecuteSlot::Unknown(decided) => {
                *slot = HostExecuteSlot::Unknown(decided);
                HostClaim::Rejected
            }
            HostExecuteSlot::Terminal(decided) if decided == request_id => {
                *slot = HostExecuteSlot::Terminal(decided);
                HostClaim::Rejected
            }
            HostExecuteSlot::Terminal(_) | HostExecuteSlot::Idle => {
                if unknown {
                    *slot = HostExecuteSlot::Unknown(request_id.to_string());
                }
                HostClaim::Unowned
            }
        }
    }

    fn host_execute_is_unknown(&self) -> bool {
        matches!(*self.lock_host_execute(), HostExecuteSlot::Unknown(_))
    }

    /// Reuses the kernel's existing interrupt dispatch instead of adding a
    /// second cancellation mechanism.
    async fn interrupt(&self) -> std::io::Result<()> {
        let line = serde_json::json!({
            "type": "control_request",
            "request_id": uuid::Uuid::new_v4().to_string(),
            "request": { "subtype": "interrupt" },
        });
        self.write_line(&line).await?;
        self.cancel_requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Answers a kernel approval request on the same control channel.
    async fn respond(&self, request_id: &str, response: serde_json::Value) -> std::io::Result<()> {
        let line = serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": response,
            },
        });
        self.write_line(&line).await
    }

    /// Reports that the request reached a decision surface, disarming the
    /// kernel's residual guard while the peer considers the permission.
    async fn receipt(&self, request_id: &str) -> std::io::Result<()> {
        let line = serde_json::json!({
            "type": "approval_receipt",
            "request_id": request_id,
        });
        self.write_line(&line).await
    }

    async fn write_line(&self, line: &serde_json::Value) -> std::io::Result<()> {
        let mut framed = line.to_string();
        framed.push('\n');
        let mut input = self.input.lock().await;
        input.write_all(framed.as_bytes()).await
    }
}

struct Session {
    id: SessionId,
    core: Arc<tokio::sync::Mutex<CoshCore>>,
    /// The same finalizer the legacy entry uses, so both entries commit one
    /// transcript under one set of durability rules.
    persistence: Arc<Mutex<SessionRuntime>>,
    turn: Option<Arc<TurnControl>>,
    host_shell: Option<HostShell>,
}

/// Negotiated foreground handoff target for one session. The PTY instance comes
/// from the client's capability advertisement, never from the model.
#[derive(Clone)]
struct HostShell {
    pty_instance: String,
    cwd: std::path::PathBuf,
}

struct AcpRuntime {
    config: CoreConfig,
    providers: ProviderFactory,
    session: Mutex<Option<Session>>,
    /// Set while a session is being built, so a second `session/new` cannot
    /// start MCP children that would then be discarded.
    creating: AtomicBool,
    /// PTY instance the client advertised at initialize, if the extension was
    /// negotiated. Read once when the session is created.
    host_shell_pty: Mutex<Option<String>>,
    ownership: Arc<PendingOwnership>,
    /// Connection-level admission shared with the outgoing sink.
    inflight: transport::InFlight,
}

impl AcpRuntime {
    fn new(config: CoreConfig, providers: ProviderFactory) -> Self {
        Self {
            config,
            providers,
            session: Mutex::new(None),
            creating: AtomicBool::new(false),
            host_shell_pty: Mutex::new(None),
            ownership: Arc::new(PendingOwnership::default()),
            inflight: transport::InFlight::new(MAX_INFLIGHT_FRAMES),
        }
    }

    fn lock_host_shell_pty(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.host_shell_pty
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_session(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        self.session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn clear_turn(&self, id: &SessionId) {
        let mut guard = self.lock_session();
        if let Some(session) = guard.as_mut() {
            if &session.id == id {
                session.turn = None;
            }
        }
    }
}

/// Releases the creation reservation even if session setup fails or unwinds.
struct CreationSlot<'a>(&'a AtomicBool);

impl Drop for CreationSlot<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The foreground action whose ownership notification has not arrived yet.
///
/// The kernel awaits each approval inline in its sequential tool loop, so at
/// most one foreground action per session can be awaiting ownership. A single
/// slot is therefore enough and cannot grow without bound.
#[derive(Default)]
struct PendingOwnership(Mutex<Option<PendingOwned>>);

struct PendingOwned {
    session_id: SessionId,
    tool_call_id: String,
    request_id: String,
    turn: Arc<TurnControl>,
}

impl PendingOwnership {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<PendingOwned>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn register(&self, entry: PendingOwned) {
        *self.lock() = Some(entry);
    }

    /// Consumes the slot only for the action it names, so a late or unrelated
    /// notification cannot disarm another action's guard.
    fn take(&self, session_id: &SessionId, tool_call_id: &str) -> Option<PendingOwned> {
        let mut slot = self.lock();
        match slot.as_ref() {
            Some(entry)
                if &entry.session_id == session_id && entry.tool_call_id == tool_call_id =>
            {
                slot.take()
            }
            _ => None,
        }
    }

    fn clear(&self, tool_call_id: &str) {
        let mut slot = self.lock();
        if slot
            .as_ref()
            .is_some_and(|entry| entry.tool_call_id == tool_call_id)
        {
            slot.take();
        }
    }
}

fn typed_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new(i32::from(code), message)
}

fn capacity_error(message: impl Into<String>) -> Error {
    Error::new(CAPACITY_ERROR_CODE, message)
}

fn unknown_session(id: &SessionId) -> Error {
    typed_error(
        ErrorCode::InvalidParams,
        format!("unknown session {id}; call session/new first"),
    )
}

/// Builds the agent handler set. Separated from the transport so fixtures can
/// pair it with an official client in process.
fn agent(
    runtime: Arc<AcpRuntime>,
    close: transport::CloseWatch,
) -> impl agent_client_protocol::ConnectTo<Client> + 'static {
    let initialize_runtime = Arc::clone(&runtime);
    let new_session_runtime = Arc::clone(&runtime);
    let load_session_runtime = Arc::clone(&runtime);
    let prompt_runtime = Arc::clone(&runtime);
    let cancel_runtime = Arc::clone(&runtime);
    let owned_runtime = Arc::clone(&runtime);
    Agent
        .v1()
        .name("cosh-core")
        .on_receive_request(
            async move |request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _connection: ConnectionTo<Client>| {
                *initialize_runtime.lock_host_shell_pty() =
                    negotiated_host_shell(request.client_capabilities.meta.as_ref());
                // Only wire v1 is implemented. The client must disconnect when
                // it cannot use the returned version.
                let mut capabilities = AgentCapabilities::new();
                capabilities.load_session = true;
                let mut meta = agent_client_protocol::v1::schema::Meta::new();
                meta.insert(TURN_CAPABILITY.to_string(), turn_restriction());
                capabilities.meta = Some(meta);
                responder.respond(
                    InitializeResponse::new(agent_client_protocol::schema::ProtocolVersion::V1)
                        .agent_capabilities(capabilities)
                        .agent_info(Implementation::new("cosh-core", env!("CARGO_PKG_VERSION"))),
                )
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        connection: ConnectionTo<Client>| {
                // MCP startup spawns children and waits for discovery, so it
                // runs in a host task instead of blocking the event loop.
                let runtime = Arc::clone(&new_session_runtime);
                connection.spawn(async move {
                    match create_session(&runtime, &request).await {
                        Ok(id) => responder.respond(NewSessionResponse::new(id)),
                        Err(error) => responder.respond_with_error(error),
                    }
                })
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: LoadSessionRequest,
                        responder: Responder<LoadSessionResponse>,
                        connection: ConnectionTo<Client>| {
                let runtime = Arc::clone(&load_session_runtime);
                let conn = connection.clone();
                connection.spawn(async move {
                    match load_session(&runtime, &request, conn).await {
                        Ok(_id) => responder.respond(LoadSessionResponse::new()),
                        Err(error) => responder.respond_with_error(error),
                    }
                })
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        connection: ConnectionTo<Client>| {
                let job = match accept_prompt(&prompt_runtime, &request) {
                    Ok(job) => job,
                    Err(error) => return responder.respond_with_error(error),
                };
                // Handlers run on the event loop: register the turn here, then
                // move the model work and the responder into a host task so
                // cancellation and reverse messages keep being processed.
                connection.spawn(drive_prompt(
                    Arc::clone(&prompt_runtime),
                    job,
                    connection.clone(),
                    responder,
                ))
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: CancelNotification, _connection: ConnectionTo<Client>| {
                let turn = {
                    let guard = cancel_runtime.lock_session();
                    guard
                        .as_ref()
                        .filter(|session| session.id == notification.session_id)
                        .and_then(|session| session.turn.clone())
                };
                if let Some(turn) = turn {
                    // Best effort: the turn may have ended between the client
                    // sending cancel and this handler running.
                    match turn.claim_cancel() {
                        CancelClaim::ResolveUnknown(request_id) => {
                            // The host owns an action that has produced no
                            // terminal fact, so a cancel cannot prove it
                            // stopped. Resolve the approval as UNKNOWN and let
                            // the kernel fail the turn closed.
                            let _ = turn
                                .respond(
                                    &request_id,
                                    serde_json::json!({
                                        "behavior": "host_execute_unknown",
                                        "message": "a cancel arrived while the foreground action was owned and no host terminal proved it ended; the action may still have run",
                                    }),
                                )
                                .await;
                        }
                        CancelClaim::Interrupt => {
                            let _ = turn.interrupt().await;
                        }
                        CancelClaim::Leave => {}
                    }
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notification: ClientNotification, _connection: ConnectionTo<Client>| {
                // The SDK only routes whole notification enums, so the extension
                // variant is matched here. Cancel is taken by the handler above.
                if let ClientNotification::ExtNotification(ext) = notification {
                    // SDK 5.x keeps the full wire method name (leading underscore).
                    if ext.method.as_ref() == HOST_SHELL_OWNED_METHOD {
                        acknowledge_owned(&owned_runtime.ownership, &ext.params).await;
                    }
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_close(move |_connection| {
            // The SDK's public clean-EOF hook. Returning Ok keeps the official
            // drain intact; the shared watch only bounds the outgoing sink.
            close.arm();
            async { Ok::<(), Error>(()) }
        })
}

async fn create_session(
    runtime: &AcpRuntime,
    request: &NewSessionRequest,
) -> Result<SessionId, Error> {
    if !request.cwd.is_absolute() {
        return Err(typed_error(
            ErrorCode::InvalidParams,
            "session cwd must be an absolute path",
        ));
    }
    if !request.additional_directories.is_empty() {
        return Err(typed_error(
            ErrorCode::InvalidParams,
            "additionalDirectories is not supported by this entry point",
        ));
    }
    let servers = client_mcp_servers(&request.mcp_servers)?;
    if runtime.creating.swap(true, Ordering::SeqCst) {
        return Err(capacity_error(
            "a session is already being created; the request was not started",
        ));
    }
    let _slot = CreationSlot(&runtime.creating);
    if runtime.lock_session().is_some() {
        return Err(capacity_error(
            "one active session is supported; the existing session was not replaced",
        ));
    }
    let provider_id = ProviderSessionId::new();
    let id = provider_id.as_str().to_string();
    let provider = (runtime.providers)(&runtime.config);
    let host_shell = runtime
        .lock_host_shell_pty()
        .clone()
        .map(|pty_instance| HostShell {
            pty_instance,
            cwd: request.cwd.clone(),
        });
    let mut registry = native_tools();
    let mut config = runtime.config.clone();
    if host_shell.is_some() {
        // Registered only after negotiation, so an unnegotiated connection has
        // no shell tool at all and cannot reach a native shell execution.
        registry.register(Box::new(crate::tool::shell::ShellTool));
        // A negotiated foreground action is the Shell's decision. A local
        // auto-allow would route the same action to native execution and
        // silently drop the foreground guarantee.
        config.agent.allowed_tools.remove(SHELL_TOOL_NAME);
    }
    // Reuses the kernel's existing MCP registry and child-process ownership.
    crate::tool::mcp::register_configured_tools(&mut registry, &servers, &request.cwd).await;
    let snapshot =
        RuntimeSnapshot::bootstrap(RuntimeGeneration::healthy(1, "acp"), Arc::new(registry));
    let persistence =
        SessionRuntime::for_entry(&config, &request.cwd, provider_id).map_err(|error| {
            typed_error(
                ErrorCode::InternalError,
                format!("session persistence unavailable: {error}"),
            )
        })?;
    let mut core = CoshCore::new_with_snapshot_and_session_id(
        config,
        provider,
        snapshot,
        id.clone(),
        request.cwd.clone(),
        SessionWorkspace::new(&request.cwd),
        ExecutionProfile::Legacy,
    );
    // This adapter answers `can_use_tool`. With the extension negotiated it
    // also owns the foreground handoff, which reroutes trust-mode shell onto
    // the approval channel instead of executing locally.
    core.client_capabilities.can_handle_can_use_tool = true;
    core.client_capabilities.can_handle_host_executed_shell = host_shell.is_some();
    let session_id = SessionId::new(id);
    *runtime.lock_session() = Some(Session {
        id: session_id.clone(),
        core: Arc::new(tokio::sync::Mutex::new(core)),
        persistence: Arc::new(Mutex::new(persistence)),
        turn: None,
        host_shell,
    });
    Ok(session_id)
}

/// Resumes a persisted session for `session/load`, restores its transcript into
/// the kernel for context continuity, and replays the user/assistant text as
/// `session/update` chunks so the client reconstructs the conversation. Tool
/// calls are not replayed as actionable cards; the restored transcript still
/// carries them for the model.
async fn load_session(
    runtime: &AcpRuntime,
    request: &LoadSessionRequest,
    connection: ConnectionTo<Client>,
) -> Result<SessionId, Error> {
    if !request.cwd.is_absolute() {
        return Err(typed_error(
            ErrorCode::InvalidParams,
            "session cwd must be an absolute path",
        ));
    }
    if !request.additional_directories.is_empty() {
        return Err(typed_error(
            ErrorCode::InvalidParams,
            "additionalDirectories is not supported by this entry point",
        ));
    }
    let id = request.session_id.0.to_string();
    ProviderSessionId::parse(&id).map_err(|error| {
        typed_error(
            ErrorCode::InvalidParams,
            format!("invalid session id: {error}"),
        )
    })?;
    let servers = client_mcp_servers(&request.mcp_servers)?;
    if runtime.creating.swap(true, Ordering::SeqCst) {
        return Err(capacity_error(
            "a session is already being created; the request was not started",
        ));
    }
    let _slot = CreationSlot(&runtime.creating);
    if runtime.lock_session().is_some() {
        return Err(capacity_error(
            "one active session is supported; the existing session was not replaced",
        ));
    }
    let host_shell = runtime
        .lock_host_shell_pty()
        .clone()
        .map(|pty_instance| HostShell {
            pty_instance,
            cwd: request.cwd.clone(),
        });
    let mut registry = native_tools();
    let mut config = runtime.config.clone();
    if host_shell.is_some() {
        registry.register(Box::new(crate::tool::shell::ShellTool));
        config.agent.allowed_tools.remove(SHELL_TOOL_NAME);
    }
    crate::tool::mcp::register_configured_tools(&mut registry, &servers, &request.cwd).await;
    let snapshot =
        RuntimeSnapshot::bootstrap(RuntimeGeneration::healthy(1, "acp"), Arc::new(registry));
    let persistence = SessionRuntime::for_resume(&config, &request.cwd, &id).map_err(|error| {
        typed_error(
            ErrorCode::InvalidParams,
            format!("session load unavailable: {error}"),
        )
    })?;
    let messages = persistence.record.messages.clone();
    let provider = (runtime.providers)(&runtime.config);
    let mut core = CoshCore::new_with_snapshot_and_session_id(
        config,
        provider,
        snapshot,
        id.clone(),
        request.cwd.clone(),
        SessionWorkspace::new(&request.cwd),
        ExecutionProfile::Legacy,
    );
    core.messages = messages.clone();
    core.client_capabilities.can_handle_can_use_tool = true;
    core.client_capabilities.can_handle_host_executed_shell = host_shell.is_some();
    let session_id = SessionId::new(id);
    *runtime.lock_session() = Some(Session {
        id: session_id.clone(),
        core: Arc::new(tokio::sync::Mutex::new(core)),
        persistence: Arc::new(Mutex::new(persistence)),
        turn: None,
        host_shell,
    });
    for message in &messages {
        let text = message.content.as_text();
        if text.is_empty() {
            continue;
        }
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
        let update = match message.role.as_str() {
            "user" => SessionUpdate::UserMessageChunk(chunk),
            "assistant" => SessionUpdate::AgentMessageChunk(chunk),
            _ => continue,
        };
        connection.send_notification(SessionNotification::new(session_id.clone(), update))?;
    }
    Ok(session_id)
}

/// Converts the client's MCP configuration into the kernel's server config.
///
/// Only stdio is accepted: it is the transport every v1 agent must support,
/// and the HTTP/SSE capabilities are not advertised.
fn client_mcp_servers(
    servers: &[agent_client_protocol::v1::schema::McpServer],
) -> Result<std::collections::HashMap<String, crate::config::McpServerConfig>, Error> {
    use agent_client_protocol::v1::schema::McpServer;

    if servers.len() > MAX_MCP_SERVERS {
        return Err(capacity_error(
            "too many client-provided MCP servers for one session",
        ));
    }
    let mut converted = std::collections::HashMap::new();
    for server in servers {
        let McpServer::Stdio(stdio) = server else {
            return Err(typed_error(
                ErrorCode::InvalidParams,
                "only stdio MCP servers are supported by this entry point",
            ));
        };
        if !stdio.command.is_absolute() {
            return Err(typed_error(
                ErrorCode::InvalidParams,
                "an MCP server command must be an absolute path",
            ));
        }
        if converted.contains_key(&stdio.name) {
            return Err(typed_error(
                ErrorCode::InvalidParams,
                "client-provided MCP server names must be unique",
            ));
        }
        let config = crate::config::McpServerConfig {
            command: stdio.command.to_string_lossy().into_owned(),
            url: None,
            args: stdio.args.clone(),
            env: stdio
                .env
                .iter()
                .map(|variable| (variable.name.clone(), variable.value.clone()))
                .collect(),
            bearer_token: None,
            oauth: Default::default(),
            timeout_ms: crate::config::default_mcp_timeout_ms(),
            startup_timeout_ms: crate::config::default_mcp_startup_timeout_ms(),
            allowed_tools: None,
        };
        converted.insert(stdio.name.clone(), config);
    }
    Ok(converted)
}

/// The native tool inventory this entry point exposes without any extension.
///
/// Shell execution stays out here: it is added only after host-shell
/// negotiation, and even then the kernel routes it to the foreground handoff
/// rather than to a local executor.
fn native_tools() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tool::read_file::ReadFileTool::new()));
    registry.register(Box::new(crate::tool::write_file::WriteFileTool));
    registry
}

struct PromptJob {
    session_id: SessionId,
    text: String,
    read_only: bool,
    core: Arc<tokio::sync::Mutex<CoshCore>>,
    persistence: Arc<Mutex<SessionRuntime>>,
    turn: Arc<TurnControl>,
    input: DuplexStream,
    host_shell: Option<HostShell>,
}

/// Releases the in-flight slot even if the host task unwinds, so one failed
/// turn cannot wedge the session into a permanent busy state.
struct TurnSlot {
    runtime: Arc<AcpRuntime>,
    session_id: SessionId,
}

impl Drop for TurnSlot {
    fn drop(&mut self) {
        self.runtime.clear_turn(&self.session_id);
    }
}

/// Renders the prompt blocks this entry point accepts.
///
/// `resource_link` is passed to the model as a bounded reference; nothing is
/// fetched. Image, audio and embedded resources are rejected because the
/// matching prompt capabilities are not advertised.
fn prompt_text(request: &PromptRequest) -> Result<String, Error> {
    let mut text = String::new();
    for block in &request.prompt {
        match block {
            ContentBlock::Text(content) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&content.text);
            }
            ContentBlock::ResourceLink(link) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("{} ({})", link.name, link.uri));
            }
            other => {
                return Err(typed_error(
                    ErrorCode::InvalidParams,
                    format!("unsupported prompt content block: {}", block_name(other)),
                ));
            }
        }
        if text.len() > MAX_PROMPT_BYTES {
            return Err(capacity_error("prompt exceeds the supported byte budget"));
        }
    }
    if text.is_empty() {
        return Err(typed_error(
            ErrorCode::InvalidParams,
            "prompt must contain at least one non-empty text or resource_link block",
        ));
    }
    Ok(text)
}

fn block_name(block: &ContentBlock) -> &'static str {
    match block {
        ContentBlock::Text(_) => "text",
        ContentBlock::Image(_) => "image",
        ContentBlock::Audio(_) => "audio",
        ContentBlock::ResourceLink(_) => "resource_link",
        ContentBlock::Resource(_) => "resource",
        _ => "unknown",
    }
}

/// The one restriction payload this entry point enforces.
fn turn_restriction() -> serde_json::Value {
    serde_json::json!({ "version": TURN_VERSION, "readOnly": true })
}

/// Reads the per-turn restriction from the prompt's `_meta`.
///
/// An unrecognised payload is rejected rather than ignored: a client that asked
/// for a guarantee it cannot verify must not get an unrestricted turn instead.
fn read_only_turn(meta: Option<&agent_client_protocol::v1::schema::Meta>) -> Result<bool, Error> {
    let Some(value) = meta.and_then(|meta| meta.get(TURN_CAPABILITY)) else {
        return Ok(false);
    };
    if value != &turn_restriction() {
        return Err(typed_error(
            ErrorCode::InvalidParams,
            "unsupported _cosh/turn payload; this entry point enforces version 1 readOnly only",
        ));
    }
    Ok(true)
}

fn accept_prompt(runtime: &AcpRuntime, request: &PromptRequest) -> Result<PromptJob, Error> {
    let text = prompt_text(request)?;
    let read_only = read_only_turn(request.meta.as_ref())?;
    let mut guard = runtime.lock_session();
    let Some(session) = guard.as_mut() else {
        return Err(unknown_session(&request.session_id));
    };
    if session.id != request.session_id {
        return Err(unknown_session(&request.session_id));
    }
    if session.turn.is_some() {
        return Err(capacity_error(
            "a prompt is already in flight for this session",
        ));
    }
    let (input_tx, input) = tokio::io::duplex(INPUT_BUFFER_BYTES);
    let turn = Arc::new(TurnControl::new(input_tx));
    session.turn = Some(Arc::clone(&turn));
    Ok(PromptJob {
        session_id: session.id.clone(),
        text,
        read_only,
        core: Arc::clone(&session.core),
        persistence: Arc::clone(&session.persistence),
        turn,
        input,
        host_shell: session.host_shell.clone(),
    })
}

async fn forward_events(
    mut events: mpsc::Receiver<serde_json::Value>,
    connection: ConnectionTo<Client>,
    session_id: SessionId,
    turn: Arc<TurnControl>,
    ownership: Arc<PendingOwnership>,
    unsupported: Arc<AtomicBool>,
    host_shell: Option<HostShell>,
    inflight: transport::InFlight,
) -> Result<(), Error> {
    let mut permissions: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut failure = None;
    let mut projector = projection::Projector::new();
    'drain: while let Some(value) = events.recv().await {
        for item in projector.project(&value) {
            match item {
                Projected::Update(update) => {
                    // Admission is refused before the official queue accepts the
                    // frame, so the unbounded queue cannot grow past the bound.
                    // The refusal reuses the turn's existing fatal capacity path.
                    if !inflight.admit() {
                        failure = Some(capacity_error(
                            "kernel output exceeded the ACP event budget; closing the connection",
                        ));
                        break 'drain;
                    }
                    if let Err(error) = connection
                        .send_notification(SessionNotification::new(session_id.clone(), *update))
                    {
                        inflight.release();
                        failure = Some(error);
                        break 'drain;
                    }
                }
                Projected::Permission(ask) => {
                    // Answering blocks on the peer, so it runs beside the drain
                    // loop: the kernel is waiting, but cancellation and the rest
                    // of the connection must keep being served.
                    permissions.push(tokio::spawn(answer_permission(
                        connection.clone(),
                        session_id.clone(),
                        Arc::clone(&turn),
                        Arc::clone(&ownership),
                        ask,
                        host_shell.clone(),
                    )));
                }
                Projected::UnsupportedInteraction => {
                    if !unsupported.swap(true, Ordering::SeqCst) {
                        // End the wait instead of leaving the kernel blocked on a
                        // decision this entry point cannot answer.
                        let _ = turn.interrupt().await;
                    }
                }
                Projected::Ignored => {}
            }
        }
    }
    // The turn is over, so an unanswered permission can no longer change
    // anything. Aborting drops the handle, which asks the peer to cancel it.
    for handle in permissions {
        handle.abort();
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Reads the negotiated host-shell PTY instance from the client's `_meta`.
///
/// A missing key, a version other than 1, or an illegal instance means the
/// extension is not negotiated, so no foreground execute is ever sent. The
/// advertisement is the only source of the target PTY identity.
fn negotiated_host_shell(meta: Option<&agent_client_protocol::v1::schema::Meta>) -> Option<String> {
    let value = meta?.get(HOST_SHELL_CAPABILITY)?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(HOST_SHELL_VERSION) {
        return None;
    }
    let instance = value
        .get("ptyInstance")
        .and_then(serde_json::Value::as_str)?;
    if instance.is_empty() || instance.len() > MAX_PTY_INSTANCE_BYTES {
        return None;
    }
    Some(instance.to_string())
}

/// Asks the peer to authorize one tool call and relays the decision back to
/// the kernel's existing approval channel.
async fn answer_permission(
    connection: ConnectionTo<Client>,
    session_id: SessionId,
    turn: Arc<TurnControl>,
    ownership: Arc<PendingOwnership>,
    ask: projection::PermissionAsk,
    host_shell: Option<HostShell>,
) {
    // A negotiated shell action belongs to the Shell's foreground PTY. It never
    // takes the standard permission path, because a generic allow there would
    // let the kernel execute the same action natively.
    if let Some(host) = host_shell.filter(|_| ask.tool_name == SHELL_TOOL_NAME) {
        answer_host_shell(connection, session_id, turn, ownership, ask, host).await;
        return;
    }
    let tool_call = ToolCallUpdate::new(
        ToolCallId::new(ask.tool_use_id.clone()),
        ToolCallUpdateFields::new()
            .title(projection::title(&ask.tool_name, &ask.input))
            .kind(projection::tool_kind(&ask.tool_name))
            .status(ToolCallStatus::Pending)
            .raw_input(ask.input.clone()),
    );
    let request = RequestPermissionRequest::new(
        session_id,
        tool_call,
        vec![
            PermissionOption::new(ALLOW_ONCE, "Allow once", PermissionOptionKind::AllowOnce),
            PermissionOption::new(REJECT_ONCE, "Reject once", PermissionOptionKind::RejectOnce),
        ],
    );
    let sent = connection.send_request(request);
    let _ = turn.receipt(&ask.request_id).await;
    let response = match sent.block_task().await {
        Ok(response) => match response.outcome {
            RequestPermissionOutcome::Selected(selected)
                if selected.option_id.0.as_ref() == ALLOW_ONCE =>
            {
                serde_json::json!({ "behavior": "allow" })
            }
            // Rejection and cancellation both deny: a cancelled turn must not
            // leave the kernel believing the call was authorized.
            _ => serde_json::json!({
                "behavior": "deny",
                "message": "the peer did not authorize this tool call",
            }),
        },
        Err(_) => serde_json::json!({
            "behavior": "deny",
            "message": "the permission request did not reach a decision",
        }),
    };
    let _ = turn.respond(&ask.request_id, response).await;
}

/// Sends one foreground execute and relays the Shell's terminal facts back to
/// the kernel's existing host-executed shell channel.
///
/// The kernel already classified the final post-Hook action and supplied the
/// trusted mandatory-ask flag; this function neither rewrites the command nor
/// relaxes that flag. A refusal, an undecodable reply or a transport failure
/// denies the call: the Shell owns the only dispatch, so there is no native
/// fallback and no replay.
/// Disarms the kernel's residual guard only when the Shell registers ownership.
///
/// Malformed or unrelated notifications are ignored: without a matching action
/// there is nothing to acknowledge, and guessing would disarm the wrong guard.
async fn acknowledge_owned(ownership: &PendingOwnership, params: &serde_json::value::RawValue) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(params.get()) else {
        return;
    };
    let (Some(session), Some(tool)) = (
        value.get("sessionId").and_then(serde_json::Value::as_str),
        value.get("toolCallId").and_then(serde_json::Value::as_str),
    ) else {
        return;
    };
    let Some(pending) = ownership.take(&SessionId::new(session.to_string()), tool) else {
        return;
    };
    pending.turn.mark_host_execute_pending(&pending.request_id);
    let _ = pending.turn.receipt(&pending.request_id).await;
}

/// Whether an execute error proves the action never dispatched.
///
/// The frozen host-shell v1 refusals and the standard JSON-RPC pre-dispatch
/// rejections state that nothing ran. Every other code leaves the outcome
/// uncertain, including the extension's own -32012 UNKNOWN and the SDK's
/// internal error for a lost connection, which cannot distinguish "never sent"
/// from "sent and still running".
fn proves_not_executed(code: i32) -> bool {
    matches!(code, -32700 | -32600 | -32601 | -32602 | -32010 | -32011)
}

/// Whether a host result status is one of the frozen terminal facts.
///
/// Mirrors the Shell's command outcome vocabulary plus its untracked-handoff
/// status. Anything else, including "unknown", leaves the effect uncertain.
fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "completed"
            | "completed_untracked"
            | "failed"
            | "interrupted"
            | "timed_out"
            | "not_executed"
    )
}

async fn answer_host_shell(
    connection: ConnectionTo<Client>,
    session_id: SessionId,
    turn: Arc<TurnControl>,
    ownership: Arc<PendingOwnership>,
    ask: projection::PermissionAsk,
    host: HostShell,
) {
    let deny = |message: &str| serde_json::json!({ "behavior": "deny", "message": message });
    let command = ask
        .input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if command.is_empty() {
        let _ = turn
            .respond(
                &ask.request_id,
                deny("the foreground action has no command"),
            )
            .await;
        return;
    }
    let params = serde_json::json!({
        "sessionId": session_id.0.to_string(),
        "toolCallId": ask.tool_use_id,
        "command": command,
        "cwd": host.cwd.to_string_lossy(),
        "ptyInstance": host.pty_instance,
        "hookRequiresApproval": ask.hook_requires_approval,
    });
    let raw = match serde_json::value::RawValue::from_string(params.to_string()) {
        Ok(raw) => raw,
        Err(_) => {
            let _ = turn
                .respond(
                    &ask.request_id,
                    deny("the foreground execute request could not be encoded"),
                )
                .await;
            return;
        }
    };
    // Registered before the request leaves, so an owned notification that
    // arrives immediately cannot find an empty slot.
    ownership.register(PendingOwned {
        session_id: session_id.clone(),
        tool_call_id: ask.tool_use_id.clone(),
        request_id: ask.request_id.clone(),
        turn: Arc::clone(&turn),
    });
    let sent = connection.send_request(
        agent_client_protocol::v1::schema::AgentRequest::ExtMethodRequest(
            agent_client_protocol::v1::schema::ExtRequest::new(
                HOST_SHELL_EXECUTE_METHOD,
                raw.into(),
            ),
        ),
    );
    let mut outcome: serde_json::Value = match sent.block_task().await {
        Ok(outcome) => outcome,
        Err(error) => {
            ownership.clear(&ask.tool_use_id);
            // A deny records "not executed" and lets the model advance. Only a
            // refusal that proves the action never dispatched may claim that;
            // every other error must fail the turn closed.
            let unknown = !proves_not_executed(i32::from(error.code));
            if matches!(
                turn.claim_host_execute(&ask.request_id, unknown),
                HostClaim::Rejected
            ) {
                return;
            }
            let response = if unknown {
                serde_json::json!({
                    "behavior": "host_execute_unknown",
                    "message": error.message,
                })
            } else {
                deny("the foreground execute request did not reach a decision")
            };
            let _ = turn.respond(&ask.request_id, response).await;
            return;
        }
    };
    ownership.clear(&ask.tool_use_id);
    // The result must prove a terminal fact before this branch may claim the
    // decision. A payload without a frozen terminal status leaves the action
    // possibly running, which is UNKNOWN rather than an executed result.
    let terminal = outcome
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(is_terminal_status);
    if !terminal {
        if matches!(
            turn.claim_host_execute(&ask.request_id, true),
            HostClaim::Rejected
        ) {
            return;
        }
        let _ = turn
            .respond(
                &ask.request_id,
                serde_json::json!({
                    "behavior": "host_execute_unknown",
                    "message": "the foreground execute result carried no terminal status",
                }),
            )
            .await;
        return;
    }
    if matches!(
        turn.claim_host_execute(&ask.request_id, false),
        HostClaim::Rejected
    ) {
        return;
    }
    let llm_content = outcome
        .get("llmContent")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(fields) = outcome.as_object_mut() {
        fields.remove("llmContent");
    }
    let _ = turn
        .respond(
            &ask.request_id,
            serde_json::json!({
                "behavior": "host_executed_shell",
                "result": {
                    "llmContent": llm_content,
                    "returnDisplay": serde_json::Value::Null,
                    "metadata": outcome,
                },
            }),
        )
        .await;
}

async fn drive_prompt(
    runtime: Arc<AcpRuntime>,
    job: PromptJob,
    connection: ConnectionTo<Client>,
    responder: Responder<PromptResponse>,
) -> Result<(), Error> {
    let PromptJob {
        session_id,
        text,
        read_only,
        core,
        persistence,
        turn,
        input,
        host_shell,
    } = job;
    let _slot = TurnSlot {
        runtime: Arc::clone(&runtime),
        session_id: session_id.clone(),
    };
    let mut lines = BufReader::new(input).lines();
    let (events_tx, events_rx) = mpsc::channel(MAX_QUEUED_EVENTS);
    let overflowed = Arc::new(AtomicBool::new(false));
    let unsupported = Arc::new(AtomicBool::new(false));
    let mut sink = EventSink::new(events_tx, MAX_EVENT_LINE_BYTES, Arc::clone(&overflowed));
    let forward = tokio::spawn(forward_events(
        events_rx,
        connection.clone(),
        session_id.clone(),
        Arc::clone(&turn),
        Arc::clone(&runtime.ownership),
        Arc::clone(&unsupported),
        host_shell,
        runtime.inflight.clone(),
    ));
    let settled = {
        let mut kernel = core.lock().await;
        // Held across the whole turn so tool declarations, the system prompt and
        // the prefix budget all see the same restricted inventory. The guard is
        // taken on a cloned handle so the kernel stays mutably borrowable.
        let registry = Arc::clone(&kernel.tools);
        let _read_only = read_only.then(|| registry.restrict_read_only());
        let outcome = kernel
            .handle_user_message(&text, &mut lines, &mut sink)
            .await;
        // Committed before the terminal is projected, so a peer can never be
        // told a turn succeeded while its transcript was refused.
        let persisted = persistence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .persist(&kernel);
        combine_turn_and_persist(outcome, persisted)
    };
    drop(sink);
    let forwarded = forward.await;
    runtime.clear_turn(&session_id);

    let overflow = overflowed.load(Ordering::SeqCst);
    let unsupported_interaction = unsupported.load(Ordering::SeqCst);
    let forward_failed = !matches!(forwarded, Ok(Ok(())));
    let terminal = if overflow || forward_failed {
        Err(capacity_error(
            "kernel output exceeded the ACP event budget; closing the connection",
        ))
    } else if unsupported_interaction {
        Err(typed_error(
            ErrorCode::InternalError,
            "the kernel requested a host interaction this entry point does not implement",
        ))
    } else {
        // The loop has returned, so the run is provably over. A cancellation we
        // delivered is reported as the standard cancelled stop reason even when
        // the provider surfaced it as an error, unless the transcript was lost.
        let cancelled =
            turn.cancel_requested.load(Ordering::SeqCst) && !turn.host_execute_is_unknown();
        projection::prompt_terminal(settled, cancelled)
            .map_err(|message| typed_error(ErrorCode::InternalError, message))
    };

    let fatal = overflow || forward_failed || unsupported_interaction;
    match terminal {
        Ok(reason) => responder.respond(PromptResponse::new(reason)),
        Err(error) => {
            let _ = responder.respond_with_error(error.clone());
            if fatal {
                return Err(error);
            }
            Ok(())
        }
    }
}

/// Serves ACP over stdio until the peer closes the connection.
pub(crate) async fn run(config: CoreConfig) -> i32 {
    let runtime = Arc::new(AcpRuntime::new(
        config,
        Arc::new(|config: &CoreConfig| crate::create_provider(config)),
    ));
    let close = transport::CloseWatch::new(CLOSE_GRACE);
    let inflight = runtime.inflight.clone();
    match agent(runtime, close.clone())
        .connect_to(transport::bounded_stdio(
            MAX_INBOUND_FRAME_BYTES,
            close,
            inflight,
        ))
        .await
    {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("[cosh-core] ACP connection ended: {error}");
            1
        }
    }
}
