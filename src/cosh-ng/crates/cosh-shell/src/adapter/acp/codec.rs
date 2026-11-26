//! Synchronous ACP v1 client codec built from official SDK wire types.
//!
//! The wire definition stays in `agent-client-protocol`; this module only owns
//! client-side ordering, correlation and bounds. Any violation makes the codec
//! terminal so the caller fails closed instead of guessing protocol state.

use std::collections::BTreeMap;
use std::path::Path;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ClientCapabilities, ContentBlock, Implementation,
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, RequestId,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, Response,
    SessionId, SessionNotification, StopReason, CLIENT_METHOD_NAMES,
};
use agent_client_protocol::{RawJsonRpcMessage, RawJsonRpcResponse, TransportFrame};

use crate::adapter::AdapterError;

use super::host_shell::{
    self, AcpHostExecuteOutcome, AcpHostExecuteRequest, AcpHostRefusal, CAPABILITY_KEY,
    EXECUTE_METHOD, OWNED_METHOD, VERSION,
};
use super::turn;

mod facts;

use facts::MAX_PENDING_REQUESTS;
pub(super) use facts::{DecodedFrame, Inbound, Phase, RequestKind, MAX_FRAME_BYTES};

/// Stateful ACP v1 client codec for one supervised Agent connection.
#[derive(Debug)]
pub(super) struct AcpClientCodec {
    phase: Phase,
    next_request_id: i64,
    pending: BTreeMap<i64, RequestKind>,
    session_id: Option<SessionId>,
    agent_capabilities: Option<AgentCapabilities>,
    agent_info: Option<Implementation>,
    max_frame_bytes: usize,
    host_shell: Option<String>,
    read_only_turn: bool,
}

impl AcpClientCodec {
    pub(super) fn new(max_frame_bytes: usize) -> Self {
        Self {
            phase: Phase::Created,
            next_request_id: 1,
            pending: BTreeMap::new(),
            session_id: None,
            agent_capabilities: None,
            agent_info: None,
            max_frame_bytes: max_frame_bytes.clamp(1, MAX_FRAME_BYTES),
            host_shell: None,
            read_only_turn: false,
        }
    }

    /// Advertises the host-shell extension; only called when a foreground
    /// authority is wired, so the client never promises a takeover it cannot
    /// perform.
    pub(super) fn enable_host_shell(&mut self, pty_instance: String) {
        self.host_shell = Some(pty_instance);
    }

    /// Sets the read-only profile for the next prompt on this connection.
    ///
    /// The restriction belongs to one turn, so a kept connection must clear it
    /// again when the next turn is not a restricted continuation.
    pub(super) fn set_read_only_turn(&mut self, restricted: bool) {
        self.read_only_turn = restricted;
    }

    pub(super) fn read_only_turn(&self) -> bool {
        self.read_only_turn
    }

    /// Whether the peer promised to enforce the restriction before any prompt
    /// is written. A peer that executes inside its own process is otherwise
    /// unreachable by the Shell's approval policy.
    pub(super) fn agent_supports_read_only_turn(&self) -> bool {
        turn::advertised(
            self.agent_capabilities
                .as_ref()
                .and_then(|c| c.meta.as_ref()),
        )
    }

    pub(super) fn phase(&self) -> Phase {
        self.phase
    }

    pub(super) fn session_id(&self) -> Option<&str> {
        self.session_id.as_ref().map(|id| id.0.as_ref())
    }

    pub(super) fn agent_capabilities(&self) -> Option<&AgentCapabilities> {
        self.agent_capabilities.as_ref()
    }

    pub(super) fn agent_info(&self) -> Option<&Implementation> {
        self.agent_info.as_ref()
    }

    /// Builds the mandatory `initialize` request advertising only implemented
    /// client capabilities: no fs, no terminal, and the host-shell extension
    /// only when a foreground executor is wired.
    pub(super) fn initialize(&mut self) -> Result<String, AdapterError> {
        self.require_phase(Phase::Created, "initialize")?;
        let mut request = InitializeRequest::new(ProtocolVersion::V1);
        request.client_capabilities = ClientCapabilities::default();
        if let Some(instance) = &self.host_shell {
            let mut meta = agent_client_protocol::v1::schema::Meta::new();
            meta.insert(
                CAPABILITY_KEY.to_string(),
                serde_json::json!({ "version": VERSION, "ptyInstance": instance }),
            );
            request.client_capabilities.meta = Some(meta);
        }
        request.client_info = Some(Implementation::new("cosh-shell", env!("CARGO_PKG_VERSION")));
        let frame = self.request_frame(RequestKind::Initialize, "initialize", &request)?;
        self.phase = Phase::Initializing;
        Ok(frame)
    }

    /// Builds `session/new` for one absolute working directory.
    pub(super) fn new_session(&mut self, cwd: &Path) -> Result<String, AdapterError> {
        self.require_phase(Phase::Ready, "session/new")?;
        if self.session_id.is_some() {
            return Err(error(
                "session/new rejected: one active session per connection",
            ));
        }
        if !cwd.is_absolute() {
            return Err(error(format!(
                "session/new rejected: cwd must be absolute, got {}",
                cwd.display()
            )));
        }
        let request = NewSessionRequest::new(cwd.to_path_buf());
        self.request_frame(RequestKind::NewSession, "session/new", &request)
    }

    /// Builds `session/load` to resume a persisted session on this connection.
    /// The id binds immediately so replayed `session/update` notifications pass
    /// the session check before the load response arrives; a failed load clears
    /// it again so the connection can start fresh.
    pub(super) fn load_session(
        &mut self,
        cwd: &Path,
        session_id: &str,
    ) -> Result<String, AdapterError> {
        self.require_phase(Phase::Ready, "session/load")?;
        if self.session_id.is_some() {
            return Err(error(
                "session/load rejected: one active session per connection",
            ));
        }
        if session_id.is_empty() {
            return Err(error("session/load rejected: empty session id"));
        }
        if !cwd.is_absolute() {
            return Err(error(format!(
                "session/load rejected: cwd must be absolute, got {}",
                cwd.display()
            )));
        }
        let id = SessionId::new(session_id.to_string());
        let request = LoadSessionRequest::new(id.clone(), cwd.to_path_buf());
        let frame = self.request_frame(RequestKind::LoadSession, "session/load", &request)?;
        self.session_id = Some(id);
        Ok(frame)
    }

    /// Builds `session/prompt`; only one prompt may be in flight per session.
    pub(super) fn prompt(&mut self, blocks: Vec<ContentBlock>) -> Result<String, AdapterError> {
        self.require_phase(Phase::Ready, "session/prompt")?;
        let session_id = self
            .session_id
            .clone()
            .ok_or_else(|| error("session/prompt rejected: no session"))?;
        if blocks.is_empty() {
            return Err(error("session/prompt rejected: empty prompt"));
        }
        let mut request = PromptRequest::new(session_id, blocks);
        if self.read_only_turn {
            let mut meta = agent_client_protocol::v1::schema::Meta::new();
            meta.insert(turn::CAPABILITY_KEY.to_string(), turn::restriction());
            request.meta = Some(meta);
        }
        let frame = self.request_frame(RequestKind::Prompt, "session/prompt", &request)?;
        self.phase = Phase::Prompting;
        Ok(frame)
    }

    /// Builds the `session/cancel` notification for the active session.
    pub(super) fn cancel(&mut self) -> Result<String, AdapterError> {
        if self.phase == Phase::Terminal {
            return Err(error("session/cancel rejected: codec is terminal"));
        }
        let session_id = self
            .session_id
            .clone()
            .ok_or_else(|| error("session/cancel rejected: no session"))?;
        let notification = CancelNotification::new(session_id);
        serialize_notification("session/cancel", &notification)
    }

    /// Builds the response to one `session/request_permission` reverse call.
    pub(super) fn permission_reply(
        &self,
        id: RequestId,
        outcome: RequestPermissionOutcome,
    ) -> Result<String, AdapterError> {
        let response = RequestPermissionResponse::new(outcome);
        serialize_response(id, &response)
    }

    /// Builds an explicit JSON-RPC method-not-found response.
    pub(super) fn unsupported_reply(
        &self,
        id: RequestId,
        method: &str,
    ) -> Result<String, AdapterError> {
        let failure = agent_client_protocol::Error::method_not_found()
            .data(format!("cosh-shell ACP client does not implement {method}"));
        serialize_failure(id, failure)
    }

    /// Decodes one bounded inbound line into ordered facts and required replies.
    pub(super) fn decode_frame(&mut self, line: &str) -> Result<DecodedFrame, AdapterError> {
        if self.phase == Phase::Terminal {
            return Err(error("decode rejected: codec is terminal"));
        }
        if self.phase == Phase::Created {
            return Err(error("decode rejected: initialize was never sent"));
        }
        if line.len() > self.max_frame_bytes {
            self.phase = Phase::Terminal;
            return Err(error(format!(
                "inbound frame exceeds {} byte limit ({} bytes)",
                self.max_frame_bytes,
                line.len()
            )));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            self.phase = Phase::Terminal;
            return Err(error("inbound frame is empty"));
        }
        let mut decoded = DecodedFrame::default();
        let result = match TransportFrame::parse_json(trimmed) {
            TransportFrame::Single(message) => self.decode_message(message, &mut decoded),
            TransportFrame::Malformed { error: failure, .. } => Err(error(format!(
                "malformed inbound JSON-RPC frame: {}",
                failure.message
            ))),
            // SDK 5.x TransportFrame is non_exhaustive and classifies JSON-RPC
            // batches as malformed; any future variant fails closed.
            _ => Err(error("unsupported inbound JSON-RPC frame variant")),
        };
        if result.is_err() {
            self.phase = Phase::Terminal;
        }
        result.map(|()| decoded)
    }

    /// Produces the single terminal fact observed when Agent stdout closes.
    pub(super) fn finish_stdout(&mut self) -> Option<Inbound> {
        if self.phase == Phase::Terminal {
            return None;
        }
        self.phase = Phase::Terminal;
        Some(Inbound::TransportClosed)
    }

    fn decode_message(
        &mut self,
        message: RawJsonRpcMessage,
        decoded: &mut DecodedFrame,
    ) -> Result<(), AdapterError> {
        match message {
            RawJsonRpcMessage::Response(response) => self.decode_response(response, decoded),
            RawJsonRpcMessage::Notification(notification) => {
                let method = notification.method.to_string();
                let params = params_value(notification.params);
                if method != CLIENT_METHOD_NAMES.session_update {
                    return Err(error(format!(
                        "unknown inbound notification method {method}"
                    )));
                }
                let update: SessionNotification =
                    serde_json::from_value(params).map_err(|failure| {
                        error(format!("invalid session/update params: {failure}"))
                    })?;
                self.require_session(&update.session_id)?;
                decoded.events.push(Inbound::Update(update.update));
                Ok(())
            }
            RawJsonRpcMessage::Request(request) => {
                let method = request.method.to_string();
                let request_id: RequestId = request.id.into();
                let params = params_value(request.params);
                if method == EXECUTE_METHOD {
                    return self.decode_host_execute(request_id, params, decoded);
                }
                if method != CLIENT_METHOD_NAMES.session_request_permission {
                    decoded.events.push(Inbound::UnsupportedRequest {
                        id: request_id,
                        method,
                    });
                    return Ok(());
                }
                let permission: RequestPermissionRequest =
                    serde_json::from_value(params).map_err(|failure| {
                        error(format!(
                            "invalid session/request_permission params: {failure}"
                        ))
                    })?;
                self.require_session(&permission.session_id)?;
                decoded.events.push(Inbound::Permission {
                    id: request_id,
                    request: permission,
                });
                Ok(())
            }
        }
    }

    /// Handles one host-shell execute reverse request.
    ///
    /// Negotiation and validation failures are answered with an explicit error
    /// reply and keep the connection usable; they are peer errors, not transport
    /// failures.
    fn decode_host_execute(
        &mut self,
        id: RequestId,
        params: serde_json::Value,
        decoded: &mut DecodedFrame,
    ) -> Result<(), AdapterError> {
        if self.host_shell.is_none() {
            return self.reject_host_execute(
                id,
                AcpHostRefusal::new(
                    host_shell::CODE_METHOD_NOT_FOUND,
                    "host-shell capability was not negotiated on this connection",
                ),
                decoded,
            );
        }
        let request: AcpHostExecuteRequest = match serde_json::from_value(params) {
            Ok(request) => request,
            Err(failure) => {
                return self.reject_host_execute(
                    id,
                    AcpHostRefusal::new(
                        host_shell::CODE_INVALID_PARAMS,
                        format!("invalid host-shell execute params: {failure}"),
                    ),
                    decoded,
                );
            }
        };
        if let Err(rejection) = host_shell::validate(&request) {
            return self.reject_host_execute(id, rejection, decoded);
        }
        let bound = self
            .session_id
            .as_ref()
            .map(|session| session.0.to_string())
            .unwrap_or_default();
        if request.session_id != bound {
            return self.reject_host_execute(
                id,
                AcpHostRefusal::new(
                    host_shell::CODE_INVALID_PARAMS,
                    "host-shell execute targets a session this connection does not own",
                ),
                decoded,
            );
        }
        // The advertisement names the PTY instance this connection owns. A
        // request for any other instance is stale, not a hint to retarget.
        if self.host_shell.as_deref() != Some(request.pty_instance.as_str()) {
            return self.reject_host_execute(
                id,
                AcpHostRefusal::new(
                    host_shell::CODE_CONSTRAINT,
                    "host-shell execute targets a PTY instance this connection does not own",
                ),
                decoded,
            );
        }
        decoded.events.push(Inbound::HostExecute { id, request });
        Ok(())
    }

    fn reject_host_execute(
        &self,
        id: RequestId,
        rejection: AcpHostRefusal,
        decoded: &mut DecodedFrame,
    ) -> Result<(), AdapterError> {
        let code = rejection.code;
        decoded.replies.push(serialize_failure(
            id.clone(),
            agent_client_protocol::Error::new(code, rejection.message),
        )?);
        decoded
            .events
            .push(Inbound::HostExecuteRejected { id, code });
        Ok(())
    }

    /// Builds the owned notification for one accepted action.
    pub(super) fn host_owned_frame(
        &self,
        request: &AcpHostExecuteRequest,
    ) -> Result<String, AdapterError> {
        serialize_notification(
            OWNED_METHOD,
            &serde_json::json!({
                "sessionId": request.session_id,
                "toolCallId": request.tool_call_id,
            }),
        )
    }

    /// Builds the terminal result reply for one owned action.
    pub(super) fn host_execute_reply(
        &self,
        id: RequestId,
        outcome: &AcpHostExecuteOutcome,
    ) -> Result<String, AdapterError> {
        serialize_response(id, outcome)
    }

    /// Builds an explicit error reply for a refused or incompatible action.
    pub(super) fn host_execute_rejection(
        &self,
        id: RequestId,
        rejection: AcpHostRefusal,
    ) -> Result<String, AdapterError> {
        serialize_failure(
            id,
            agent_client_protocol::Error::new(rejection.code, rejection.message),
        )
    }

    fn decode_response(
        &mut self,
        response: RawJsonRpcResponse,
        decoded: &mut DecodedFrame,
    ) -> Result<(), AdapterError> {
        let (id, outcome) = match response {
            RawJsonRpcResponse::Result { id, result } => (id, Ok(result)),
            RawJsonRpcResponse::Error { id, error } => (id, Err(error)),
        };
        let id: RequestId = id.into();
        let RequestId::Number(number) = id else {
            return Err(error("inbound response id is not a client-issued number"));
        };
        let kind = self
            .pending
            .remove(&number)
            .ok_or_else(|| error(format!("inbound response has unknown id {number}")))?;
        let failure = match outcome {
            Ok(result) => return self.decode_success(kind, result, decoded),
            Err(failure) => failure,
        };
        if kind == RequestKind::Prompt {
            self.phase = Phase::Ready;
        }
        if kind == RequestKind::LoadSession {
            self.session_id = None;
        }
        if kind == RequestKind::Initialize {
            self.phase = Phase::Terminal;
        }
        decoded.events.push(Inbound::Failed {
            kind,
            code: failure.code,
            message: failure.message,
        });
        Ok(())
    }

    fn decode_success(
        &mut self,
        kind: RequestKind,
        result: serde_json::Value,
        decoded: &mut DecodedFrame,
    ) -> Result<(), AdapterError> {
        match kind {
            RequestKind::Initialize => {
                let response: InitializeResponse = serde_json::from_value(result)
                    .map_err(|failure| error(format!("invalid initialize result: {failure}")))?;
                if response.protocol_version != ProtocolVersion::V1 {
                    return Err(error(format!(
                        "agent negotiated unsupported ACP wire version {}",
                        response.protocol_version.as_u16()
                    )));
                }
                self.agent_capabilities = Some(response.agent_capabilities);
                self.agent_info = response.agent_info;
                self.phase = Phase::Ready;
                decoded.events.push(Inbound::Initialized);
            }
            RequestKind::NewSession => {
                let response: NewSessionResponse = serde_json::from_value(result)
                    .map_err(|failure| error(format!("invalid session/new result: {failure}")))?;
                if response.session_id.0.is_empty() {
                    return Err(error("session/new returned an empty session id"));
                }
                self.session_id = Some(response.session_id);
                decoded.events.push(Inbound::SessionCreated {
                    session_id: self
                        .session_id
                        .as_ref()
                        .map(|id| id.0.to_string())
                        .unwrap_or_default(),
                });
            }
            RequestKind::LoadSession => {
                let _response: LoadSessionResponse = serde_json::from_value(result)
                    .map_err(|failure| error(format!("invalid session/load result: {failure}")))?;
                decoded.events.push(Inbound::SessionCreated {
                    session_id: self
                        .session_id
                        .as_ref()
                        .map(|id| id.0.to_string())
                        .unwrap_or_default(),
                });
            }
            RequestKind::Prompt => {
                let response: PromptResponse =
                    serde_json::from_value(result).map_err(|failure| {
                        error(format!("invalid session/prompt result: {failure}"))
                    })?;
                self.phase = Phase::Ready;
                decoded.events.push(Inbound::Stopped {
                    reason: response.stop_reason,
                });
            }
        }
        Ok(())
    }

    fn require_session(&self, session_id: &SessionId) -> Result<(), AdapterError> {
        let bound = self
            .session_id
            .as_ref()
            .ok_or_else(|| error("inbound message arrived before session/new completed"))?;
        if bound.0 != session_id.0 {
            return Err(error(format!(
                "inbound message targets session {} but this connection is bound to {}",
                session_id.0, bound.0
            )));
        }
        Ok(())
    }

    fn require_phase(&self, expected: Phase, method: &str) -> Result<(), AdapterError> {
        if self.phase != expected {
            return Err(error(format!(
                "{method} rejected in phase {:?}, expected {expected:?}",
                self.phase
            )));
        }
        Ok(())
    }

    fn request_frame(
        &mut self,
        kind: RequestKind,
        method: &str,
        params: &impl serde::Serialize,
    ) -> Result<String, AdapterError> {
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(error(format!(
                "{method} rejected: {MAX_PENDING_REQUESTS} outbound requests already pending"
            )));
        }
        let id = self.next_request_id;
        self.next_request_id = id
            .checked_add(1)
            .ok_or_else(|| error(format!("{method} rejected: request id space exhausted")))?;
        let value = serde_json::to_value(params)
            .map_err(|failure| error(format!("cannot serialize {method} params: {failure}")))?;
        let message = RawJsonRpcMessage::request(method.to_string(), value, RequestId::Number(id))
            .map_err(|failure| {
                error(format!(
                    "cannot build {method} request: {}",
                    failure.message
                ))
            })?;
        let frame = TransportFrame::Single(message)
            .to_json()
            .map_err(|failure| {
                error(format!("cannot encode {method} frame: {}", failure.message))
            })?;
        if frame.len() > self.max_frame_bytes {
            return Err(error(format!(
                "{method} frame exceeds {} byte limit ({} bytes)",
                self.max_frame_bytes,
                frame.len()
            )));
        }
        self.pending.insert(id, kind);
        Ok(frame)
    }
}

fn params_value(params: Option<agent_client_protocol::RawJsonRpcParams>) -> serde_json::Value {
    params.map_or(serde_json::Value::Null, |params| params.into_value())
}

fn serialize_notification(
    method: &str,
    params: &impl serde::Serialize,
) -> Result<String, AdapterError> {
    let value = serde_json::to_value(params)
        .map_err(|failure| error(format!("cannot serialize {method} params: {failure}")))?;
    let message = RawJsonRpcMessage::notification(method.to_string(), value)
        .map_err(|failure| error(format!("cannot build {method}: {}", failure.message)))?;
    TransportFrame::Single(message)
        .to_json()
        .map_err(|failure| error(format!("cannot encode {method}: {}", failure.message)))
}

fn serialize_response(
    id: RequestId,
    params: &impl serde::Serialize,
) -> Result<String, AdapterError> {
    let value = serde_json::to_value(params)
        .map_err(|failure| error(format!("cannot serialize response params: {failure}")))?;
    let message = RawJsonRpcMessage::response(id, Ok(value));
    TransportFrame::Single(message)
        .to_json()
        .map_err(|failure| error(format!("cannot encode response: {}", failure.message)))
}

fn serialize_failure(
    id: RequestId,
    failure: agent_client_protocol::Error,
) -> Result<String, AdapterError> {
    let message = RawJsonRpcMessage::response(id, Err(failure));
    TransportFrame::Single(message)
        .to_json()
        .map_err(|failure| error(format!("cannot encode error response: {}", failure.message)))
}

fn error(message: impl Into<String>) -> AdapterError {
    AdapterError {
        message: message.into(),
    }
}
