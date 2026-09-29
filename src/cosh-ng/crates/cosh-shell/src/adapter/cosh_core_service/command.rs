//! Command payloads carried over the persistent cosh-core service channel.
//!
//! Split from `cosh_core_service.rs` to keep the service module under the
//! layout threshold; these are plain data carriers with no behavior.

use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc, Mutex};

use serde_json::Value;

use crate::types::{AgentEvent, CoshApprovalMode};

use super::super::cosh_core::{SessionResumeAttempt, SessionRuntimeState};
use super::super::cosh_core_registry::RegistryQueryError;
use super::super::PreparedInvocation;
use super::super::{
    control_protocol, AdapterError, ApprovalChannelMessage, AuthResponse,
    ProviderCancellationArtifactStore,
};

pub(super) struct RunCommand {
    pub(super) run_id: String,
    pub(super) prepared: PreparedInvocation,
    pub(super) raw_user_input: Option<String>,
    pub(super) mode: CoshApprovalMode,
    pub(super) session_state: Arc<Mutex<SessionRuntimeState>>,
    pub(super) session_scope: String,
    pub(super) resume_attempt: SessionResumeAttempt,
    pub(super) event_tx: mpsc::Sender<Result<AgentEvent, AdapterError>>,
    pub(super) internal_response_tx: mpsc::Sender<ApprovalChannelMessage>,
    pub(super) approval_rx: Option<mpsc::Receiver<ApprovalChannelMessage>>,
    pub(super) auth_rx: Option<mpsc::Receiver<AuthResponse>>,
    pub(super) answer_confirmation_tx: mpsc::Sender<Result<String, AdapterError>>,
    pub(super) pending_session: Arc<Mutex<Option<String>>>,
    pub(super) cancellation_artifacts: ProviderCancellationArtifactStore,
    pub(super) control_capabilities: Arc<Mutex<control_protocol::ControlProtocolCapabilities>>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) run_done: Arc<AtomicBool>,
}

pub(super) struct RegistryCommand {
    pub(super) request_id: String,
    pub(super) domain: String,
    pub(super) action: String,
    pub(super) params: Value,
    pub(super) response_tx: mpsc::Sender<Result<Value, RegistryQueryError>>,
}

pub(super) enum ServiceCommand {
    Run(RunCommand),
    Registry(RegistryCommand),
    Shutdown,
}
