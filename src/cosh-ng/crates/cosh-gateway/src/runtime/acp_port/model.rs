/// COSH-owned identities and ACP connection scope for one process generation.
#[derive(Debug, Clone)]
pub struct AcpAgentRuntimeIdentity {
    /// Durable Gateway installation.
    pub installation_id: InstallationId,
    /// Authenticated actor whose policy context governs permissions.
    pub actor: ActorRef,
    /// Task owning this runtime.
    pub task_id: TaskId,
    /// Run owning this runtime generation.
    pub run_id: RunId,
    /// Logical Agent session allocated by COSH.
    pub agent_session_id: AgentSessionId,
    /// Fenced binding allocated by COSH.
    pub binding_id: RuntimeBindingId,
    /// Supervised process identity allocated by COSH.
    pub runtime_instance_id: RuntimeInstanceId,
    /// Monotonic process generation used to reject stale output.
    pub runtime_generation: u64,
    /// Trusted ACP adapter authority.
    pub adapter_authority: BoundedName,
    /// Digest of the complete ACP connection parent scope.
    pub connection_scope_digest: Digest,
}

/// Immutable launch, identity, and workspace settings for the ACP port.
#[derive(Clone)]
pub struct AcpAgentRuntimeConfig {
    /// Supervised ACP session configuration.
    pub session: AcpSessionDriverConfig,
    /// Public workspace scope expected by `OpenSession`.
    pub workspace: WorkspaceRef,
    /// COSH-owned lifecycle identities.
    pub identity: AcpAgentRuntimeIdentity,
}

/// Trusted context supplied while normalizing an ACP permission callback.
#[derive(Debug, Clone)]
pub struct AcpPermissionContext {
    /// Authenticated actor bound to the runtime.
    pub actor: ActorRef,
    /// Task owning the callback.
    pub task_id: TaskId,
    /// Run owning the callback.
    pub run_id: RunId,
}

/// Trusted boundary that canonicalizes an untrusted ACP tool call.
///
/// Implementations must resolve the target and operation from trusted local
/// configuration. Copying Agent-provided labels into authorization fields is
/// unsafe and violates this contract.
pub trait AcpPermissionNormalizer: Send {
    /// Produces a bounded request for Capability Broker evaluation.
    ///
    /// # Errors
    ///
    /// Returns a stable port error when the callback cannot be canonicalized
    /// without trusting Agent-controlled authorization data.
    fn normalize(
        &mut self,
        request: &AcpV1PermissionRequest,
        context: &AcpPermissionContext,
    ) -> Result<CapabilityRequest, AgentRuntimePortError>;
}

trait AcpSessionBackend: Send {
    fn initialize(&self) -> Result<(), AcpSessionDriverError>;
    fn open_session(&self) -> Result<(), AcpSessionDriverError>;
    fn prompt(&self, text: String) -> Result<(), AcpSessionDriverError>;
    fn answer_permission(
        &self,
        request_id: AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<(), AcpSessionDriverError>;
    fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<AcpSessionEvent, std::sync::mpsc::RecvTimeoutError>;
    fn cancel(&self) -> Result<(), AcpSessionDriverError>;
    fn shutdown(&self) -> Result<(), AcpSessionDriverError>;
}

impl AcpSessionBackend for AcpSessionDriver {
    fn initialize(&self) -> Result<(), AcpSessionDriverError> {
        self.initialize()
    }
    fn open_session(&self) -> Result<(), AcpSessionDriverError> {
        self.open_session()
    }
    fn prompt(&self, text: String) -> Result<(), AcpSessionDriverError> {
        self.prompt(text)
    }
    fn answer_permission(
        &self,
        request_id: AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<(), AcpSessionDriverError> {
        self.answer_permission(request_id, decision)
    }
    fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<AcpSessionEvent, std::sync::mpsc::RecvTimeoutError> {
        self.receive_timeout(timeout)
    }
    fn cancel(&self) -> Result<(), AcpSessionDriverError> {
        self.control().cancel()
    }
    fn shutdown(&self) -> Result<(), AcpSessionDriverError> {
        self.shutdown()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortState {
    Created,
    SessionOpenedPending,
    SessionOpen,
    PromptActive,
    Terminal,
}

impl PortState {
    fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::SessionOpenedPending => "session-opened-pending",
            Self::SessionOpen => "session-open",
            Self::PromptActive => "prompt-active",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Clone)]
struct PendingPermission {
    acp_request_id: AcpV1RequestId,
    allow_once: Option<String>,
    reject_once: Option<String>,
}

/// One supervised ACP process exposed through `AgentRuntimePort`.
pub struct AcpAgentRuntime {
    backend: Box<dyn AcpSessionBackend>,
    normalizer: Box<dyn AcpPermissionNormalizer>,
    config: AcpAgentRuntimeConfig,
    state: PortState,
    binding: Option<RuntimeBindingRef>,
    provider_session: Option<String>,
    events: VecDeque<RuntimeEventEnvelope>,
    sequence: u64,
    messages: BTreeMap<String, RuntimeMessageId>,
    active_turn: Option<TurnId>,
    tools: ToolInvocationAccumulator,
    permissions: BTreeMap<RequestId, PendingPermission>,
    terminal_delivered: bool,
}
