//! Synchronous ACP v1 client adapter for external Agent subprocesses.
//!
//! Wire types and framing come from the official `agent-client-protocol` crate;
//! this module owns only Shell-side lifecycle, projection and bounds. It keeps
//! `cosh-shell` standalone: no internal crate dependency and no async runtime.
//! Foreground shell actions are answered by one authority round trip; when the
//! runtime supplies its approval channel, that authority is the runtime itself.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::{
    ContentBlock, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SelectedPermissionOutcome, StopReason, TextContent,
};

use crate::evidence::ShellEvidenceAccess;
use crate::types::{
    request_is_analysis_only_continuation, AgentEvent, AgentRequest, CoshApprovalMode,
};

use super::{
    acp_prompt_contract, prompt_from_request_with_evidence_policy, AdapterError, AgentAdapter,
    AgentBackendCapabilities, FreshSessionOutcome, PreambleTools,
};

mod codec;
mod host_runtime;
mod host_shell;
mod permission;
mod projection;
mod session;
mod transport;
mod turn;

#[cfg(test)]
mod tests;

pub use permission::{AcpHostAuthority, AcpPermissionDecision, AcpPermissionPolicy};

pub use host_shell::{
    AcpHostDecision, AcpHostExecuteOutcome, AcpHostExecuteRequest, AcpHostInputWait, AcpHostRefusal,
};

use codec::{AcpClientCodec, Inbound, Phase, MAX_FRAME_BYTES};
use permission::{permission_outcome, reject_by_default};
use session::AcpSession;
use transport::{AgentTransport, LineEvent};

/// Default bound on trailing Agent stderr retained for failure reporting.
const STDERR_TAIL_BYTES: usize = 8192;
/// Default idle deadline while waiting for the next Agent frame.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Grace period for the Agent to confirm cancellation before it is terminated.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// Poll slice used so cancellation is observed without blocking on a frame.
const POLL_SLICE: Duration = Duration::from_millis(50);

/// Adapter that runs one standard ACP session against an external Agent.
#[derive(Clone)]
pub struct AcpAgentAdapter {
    pub program: String,
    pub args: Vec<String>,
    pub allow_model_call: bool,
    pub idle_timeout: Duration,
    pub max_frame_bytes: usize,
    pub permission_policy: AcpPermissionPolicy,
    pub host_authority: Option<AcpHostAuthority>,
    pub host_pty_instance: Option<String>,
    /// Local admission for the per-turn read-only profile. Off by default: a
    /// peer's own advertisement is a promise the Shell cannot verify, so the
    /// profile must be granted by local construction that has established the
    /// peer's execution restriction.
    read_only_turn_profile: bool,
    host_channels: Option<HostChannels>,
    /// The invocation mode for the next run. Set per run by the Shell entry,
    /// because a kept connection must not carry the previous turn's mode.
    approval_mode: CoshApprovalMode,
    cancelled: Arc<AtomicBool>,
    /// The one peer and session this Shell owns, kept across user turns.
    peer: Arc<Mutex<Option<LivePeer>>>,
    /// The committed provider session id, kept behind its own short lock so a
    /// read-only identity query never waits on the whole-turn peer lock.
    committed_id: Arc<Mutex<Option<String>>>,
    /// An explicit persisted session to load on the next run, set by session
    /// selection; consumed once so the committed identity takes over after load.
    resume_target: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for AcpAgentAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpAgentAdapter")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("allow_model_call", &self.allow_model_call)
            .field("idle_timeout", &self.idle_timeout)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .finish_non_exhaustive()
    }
}

impl Default for AcpAgentAdapter {
    fn default() -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            allow_model_call: false,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_frame_bytes: MAX_FRAME_BYTES,
            permission_policy: Arc::new(reject_by_default),
            host_authority: None,
            host_pty_instance: None,
            read_only_turn_profile: false,
            host_channels: None,
            approval_mode: CoshApprovalMode::default(),
            cancelled: Arc::new(AtomicBool::new(false)),
            peer: Arc::new(Mutex::new(None)),
            committed_id: Arc::new(Mutex::new(None)),
            resume_target: Arc::new(Mutex::new(None)),
        }
    }
}

impl AcpAgentAdapter {
    /// Builds an adapter for one Agent program; model-call admission stays off.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    /// Resolves the Agent from `COSH_ACP_AGENT_PROGRAM` and whitespace-separated
    /// `COSH_ACP_AGENT_ARGS`. Selection never starts the process or probes the
    /// peer, so an unset program stays empty and fails at run time.
    pub fn from_environment() -> Self {
        let program = std::env::var("COSH_ACP_AGENT_PROGRAM").unwrap_or_default();
        let args = std::env::var("COSH_ACP_AGENT_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Self {
            program,
            args,
            ..Self::default()
        }
    }

    pub fn with_model_call(mut self, allow_model_call: bool) -> Self {
        self.allow_model_call = allow_model_call;
        self
    }

    /// Grants the per-turn read-only profile to this construction.
    ///
    /// Only call this where the peer's execution restriction has been
    /// established locally, such as the Shell's own Core. The peer's handshake
    /// advertisement stays a second, necessary condition; it is never sufficient
    /// on its own, because any peer can copy it and still run its own executor.
    pub fn with_read_only_turn_profile(mut self) -> Self {
        self.read_only_turn_profile = true;
        self
    }

    pub fn with_permission_policy(mut self, policy: AcpPermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    /// Composes a policy review and an executor into one authority, keeping
    /// the mandatory-ask invariant at the adapter boundary.
    pub fn with_host_policy(
        self,
        review: Arc<dyn Fn(&AcpHostExecuteRequest) -> AcpHostDecision + Send + Sync>,
        executor: Arc<dyn Fn(&AcpHostExecuteRequest) -> AcpHostExecuteOutcome + Send + Sync>,
        pty_instance: impl Into<String>,
    ) -> Self {
        self.with_host_authority(
            Arc::new(move |request| {
                host_shell::review(request, review(request))?;
                Ok(executor(request))
            }),
            pty_instance,
        )
    }

    /// Wires the foreground authority and the PTY instance it owns, which
    /// together advertise the host-shell capability. Without both, the client
    /// never promises a takeover and the peer cannot target a stale PTY.
    pub fn with_host_authority(
        mut self,
        authority: AcpHostAuthority,
        pty_instance: impl Into<String>,
    ) -> Self {
        self.host_authority = Some(authority);
        self.host_pty_instance = Some(pty_instance.into());
        self
    }

    /// Wires the runtime's own approval channel as the foreground authority.
    ///
    /// The PTY instance is the Shell session identity carried on each request,
    /// so the advertisement always names the session that will actually run.
    pub(crate) fn with_host_channels(
        mut self,
        events: mpsc::Sender<Result<AgentEvent, AdapterError>>,
        approvals: mpsc::Receiver<super::ApprovalChannelMessage>,
    ) -> Self {
        self.host_channels = Some(HostChannels {
            events,
            approvals: Arc::new(Mutex::new(approvals)),
        });
        self
    }

    /// Sets the invocation mode for the next run on this adapter.
    pub(crate) fn with_approval_mode(mut self, mode: CoshApprovalMode) -> Self {
        self.approval_mode = mode;
        self
    }

    /// Shared flag the Shell runtime sets to request cooperative cancellation.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// The provider session id the kept peer has committed, if a session is
    /// established. Lets the Shell resume via session/load instead of treating
    /// the ACP path as having no recoverable identity.
    pub fn committed_session_id(&self) -> Option<String> {
        self.committed_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The explicit session selection awaiting the next run, if any. Consumed
    /// by the next run, so a steady-state connection reports None.
    pub fn pending_resume(&self) -> Option<String> {
        self.resume_target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Detaches the kept peer so the next run establishes a fresh session.
    /// Dropping the peer closes its transport; the persisted session is not
    /// deleted. Idempotent: with no kept peer the previous id is None.
    pub fn start_fresh_session(&self) -> FreshSessionOutcome {
        let previous_session_id = self
            .committed_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Ok(mut peer) = self.peer.lock() {
            *peer = None;
        }
        FreshSessionOutcome::Detached {
            previous_session_id,
        }
    }

    /// Selects an explicit persisted session to load on the next run. Drops any
    /// kept peer so the next turn opens a fresh connection and resumes the named
    /// session via session/load; a peer without that capability fails closed
    /// before any prompt. The committed identity takes over after a successful
    /// load.
    pub fn resume_session(&self, session_id: &str) {
        *self
            .resume_target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(session_id.to_string());
        if let Ok(mut peer) = self.peer.lock() {
            *peer = None;
        }
    }
}

impl AgentAdapter for AcpAgentAdapter {
    fn name(&self) -> &'static str {
        "acp"
    }

    fn capabilities(&self) -> AgentBackendCapabilities {
        AgentBackendCapabilities {
            text_stream: true,
            thinking_stream: true,
            session_resume: true,
            tool_intent: true,
            user_question: false,
            cancellable: true,
            control_protocol: false,
        }
    }

    fn run(&self, request: &AgentRequest) -> Result<Vec<AgentEvent>, AdapterError> {
        let mut events = Vec::new();
        self.run_stream(request, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    fn run_stream(
        &self,
        request: &AgentRequest,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        if !self.allow_model_call {
            return Err(AdapterError {
                message: "acp adapter requires explicit model-call admission".to_string(),
            });
        }
        if self.program.trim().is_empty() {
            return Err(AdapterError {
                message: "acp adapter requires an agent program".to_string(),
            });
        }
        let cwd = Path::new(&request.command_block.cwd);
        if !cwd.is_absolute() {
            return Err(AdapterError {
                message: format!(
                    "acp session requires an absolute cwd, got {}",
                    request.command_block.cwd
                ),
            });
        }
        // The ACP entry has no control protocol, so evidence reads stay on the
        // fenced request fallback the Shell parses out of peer text.
        let access = ShellEvidenceAccess::FencedRequestFallback;
        // Recommend mode forbids automatic output requests on both entries, so
        // the preamble carries the same restriction the contract already states.
        let preamble = prompt_from_request_with_evidence_policy(
            request,
            access,
            self.approval_mode != CoshApprovalMode::Recommend,
            PreambleTools::PeerAdvertised,
        );
        if preamble.trim().is_empty() {
            return Err(AdapterError {
                message: "acp session requires a non-empty prompt".to_string(),
            });
        }
        let restricted = request_is_analysis_only_continuation(request);
        if restricted && !self.read_only_turn_profile {
            // Refused before the peer is even started: without a locally
            // established execution restriction there is nothing to negotiate,
            // and a prompt could reach the peer's own executor.
            return Err(AdapterError {
                message: format!(
                    "acp peer has no locally verified {} profile; the analysis-only continuation was refused before the peer was started",
                    turn::CAPABILITY_KEY
                ),
            });
        }
        let runtime_authority = self.host_channels.as_ref().map(|channels| {
            host_runtime::authority(
                request.id.clone(),
                channels.events.clone(),
                Arc::clone(&channels.approvals),
                Arc::clone(&self.cancelled),
            )
        });
        let host_authority = runtime_authority.as_ref().or(self.host_authority.as_ref());
        let instance = host_authority
            .is_some()
            .then(|| {
                self.host_pty_instance
                    .clone()
                    .unwrap_or_else(|| request.session_id.clone())
            })
            .filter(|id| !id.is_empty() && id.len() <= host_shell::MAX_ID_BYTES);
        // The Shell admits one run at a time, so holding the peer for the whole
        // turn is the same ownership the single active run slot already gives.
        let mut slot = self
            .peer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let explicit_resume = self
            .resume_target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let mut kept = slot.take();
        let mut resume = None;
        if let Some(id) = explicit_resume {
            // Explicit selection forces a fresh connection that loads the named
            // session, regardless of any kept peer.
            kept = None;
            resume = Some(id);
        } else if let Some(peer) = kept.as_mut() {
            if !peer.serves(cwd, instance.as_deref()) {
                // Same workspace but the peer died or is no longer reusable:
                // resume its committed session on the fresh connection instead
                // of losing the persisted context. A different workspace is a
                // fresh session, not a load of the old workspace's session.
                if peer.serves_workspace(cwd, instance.as_deref()) {
                    resume = self
                        .committed_id
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                }
                // Clearing the slot terminates and reaps the stale child before
                // the replacement starts, so no orphan keeps the old session.
                kept = None;
            }
        }
        let mut peer = match kept {
            Some(peer) => peer,
            None => LivePeer {
                transport: AgentTransport::launch(
                    &self.program,
                    &self.args,
                    self.max_frame_bytes,
                    STDERR_TAIL_BYTES,
                    Arc::clone(&self.cancelled),
                )?,
                codec: AcpClientCodec::new(self.max_frame_bytes),
                owned: HashSet::new(),
                cwd: cwd.to_path_buf(),
                host_instance: instance.clone(),
            },
        };
        if let Some(instance) = &instance {
            peer.codec.enable_host_shell(instance.clone());
        }
        peer.codec.set_read_only_turn(restricted);
        // The foreground claim follows the capability this connection actually
        // advertises, so the prompt cannot promise a PTY the peer cannot target.
        let prompt = format!(
            "{}{}",
            preamble,
            acp_prompt_contract(request, self.approval_mode, instance.is_some(), access)
        );
        let mut session = AcpSession {
            transport: &mut peer.transport,
            codec: &mut peer.codec,
            owned: &mut peer.owned,
            run_id: request.id.clone(),
            idle_timeout: self.idle_timeout,
            policy: &self.permission_policy,
            host_authority,
            cancelled: &self.cancelled,
            committed_id: &self.committed_id,
            stop_reason: None,
            failed: false,
            tool_update_dedup: session::ToolUpdateDedup::new(),
        };
        let result = session.run(cwd, &prompt, resume.as_deref(), sink);
        let keep = result.is_ok() && session.reusable();
        *slot = keep.then_some(peer);
        result
    }
}

/// One live ACP connection kept for the Shell session: the transport, the
/// client codec that owns its protocol state, and the tool-call responsibility
/// already taken on it.
struct LivePeer {
    transport: AgentTransport,
    codec: AcpClientCodec,
    owned: HashSet<String>,
    cwd: PathBuf,
    host_instance: Option<String>,
}

impl LivePeer {
    /// A kept connection only serves the workspace and host identity it was
    /// handshaken for, and only while its peer is still alive. A peer that
    /// exited while idle is evicted here, before a new request is dispatched.
    fn serves(&mut self, cwd: &Path, host_instance: Option<&str>) -> bool {
        self.serves_workspace(cwd, host_instance)
            && self.codec.phase() == Phase::Ready
            && !self.transport.has_exited()
    }

    /// Workspace identity only, without liveness: a peer that served this same
    /// workspace but died leaves a committed session worth resuming, while a
    /// different workspace is a fresh session.
    fn serves_workspace(&self, cwd: &Path, host_instance: Option<&str>) -> bool {
        self.cwd == cwd && self.host_instance.as_deref() == host_instance
    }
}

/// The runtime channels a foreground authority needs for one run.
#[derive(Clone)]
struct HostChannels {
    events: mpsc::Sender<Result<AgentEvent, AdapterError>>,
    approvals: Arc<Mutex<mpsc::Receiver<super::ApprovalChannelMessage>>>,
}
