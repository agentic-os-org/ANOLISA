/// Trusted authority fields used to normalize brokered Core requests.
#[derive(Debug, Clone)]
pub struct CoshCoreBrokeredContext {
    /// Authenticated actor on whose behalf Core proposes the operation.
    pub actor: ActorRef,
    /// Immutable target selected by Gateway admission.
    pub target: TargetRef,
}

/// COSH-owned identities and provider scope for one Core process generation.
#[derive(Debug, Clone)]
pub struct CoshCoreBridgeIdentity {
    /// Durable Gateway installation.
    pub installation_id: InstallationId,
    /// Authenticated actor propagated into public correlation when known.
    pub actor_id: Option<ActorId>,
    /// Task owning this bridge.
    pub task_id: TaskId,
    /// Run owning this bridge generation.
    pub run_id: RunId,
    /// Logical Agent session allocated by COSH.
    pub agent_session_id: AgentSessionId,
    /// Fenced binding allocated by COSH.
    pub binding_id: RuntimeBindingId,
    /// Supervised process identity allocated by COSH.
    pub runtime_instance_id: RuntimeInstanceId,
    /// Monotonic process generation used to reject stale output.
    pub runtime_generation: u64,
    /// Trusted provider namespace, such as `cosh-core`.
    pub provider_authority: BoundedName,
    /// Digest of the complete provider-session parent scope.
    pub provider_scope_digest: Digest,
}

/// Immutable launch, identity, and deadline settings for a Core bridge.
#[derive(Clone)]
pub struct CoshCoreBridgeConfig {
    /// Direct supervised cosh-core launch specification.
    pub launch: RuntimeLaunchSpec,
    /// Public workspace scope expected by `OpenSession`.
    pub workspace: WorkspaceRef,
    /// COSH-owned lifecycle identities.
    pub identity: CoshCoreBridgeIdentity,
    /// Maximum private JSONL frame size.
    pub max_frame_bytes: usize,
    /// Maximum lifetime of one active turn.
    pub prompt_timeout: Duration,
    /// TERM grace before KILL escalation.
    pub shutdown_grace: Duration,
    /// Profile fixed before the child is launched.
    pub execution_profile: CoshCoreExecutionProfile,
    /// Trusted normalization context required by the brokered profile.
    pub brokered_context: Option<CoshCoreBrokeredContext>,
}

impl CoshCoreBridgeConfig {
    /// Builds a configuration with conservative local deadlines.
    #[must_use]
    pub fn new(
        launch: RuntimeLaunchSpec,
        workspace: WorkspaceRef,
        identity: CoshCoreBridgeIdentity,
    ) -> Self {
        let max_frame_bytes = launch.stdout_line_limit;
        Self {
            launch,
            workspace,
            identity,
            max_frame_bytes,
            prompt_timeout: Duration::from_secs(30 * 60),
            shutdown_grace: Duration::from_secs(2),
            execution_profile: CoshCoreExecutionProfile::Legacy,
            brokered_context: None,
        }
    }

    /// Selects the fail-closed Gateway-brokered profile.
    #[must_use]
    pub fn gateway_brokered(mut self, context: CoshCoreBrokeredContext) -> Self {
        self.execution_profile = CoshCoreExecutionProfile::GatewayBrokeredV1;
        self.brokered_context = Some(context);
        self
    }

    /// Selects the fail-closed Gateway-brokered checkpoint profile.
    #[must_use]
    pub fn gateway_brokered_checkpoint(mut self, context: CoshCoreBrokeredContext) -> Self {
        self.execution_profile = CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1;
        self.brokered_context = Some(context);
        self
    }

    /// Selects the fail-closed Gateway-brokered workspace-write profile.
    #[must_use]
    pub fn gateway_brokered_workspace_write(mut self, context: CoshCoreBrokeredContext) -> Self {
        self.execution_profile = CoshCoreExecutionProfile::GatewayBrokeredWorkspaceWriteV1;
        self.brokered_context = Some(context);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeState {
    Created,
    Opening,
    SessionOpenedPending,
    SessionOpen,
    PromptActive,
    Terminal,
}

impl BridgeState {
    fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Opening => "opening",
            Self::SessionOpenedPending => "session-opened-pending",
            Self::SessionOpen => "session-open",
            Self::PromptActive => "prompt-active",
            Self::Terminal => "terminal",
        }
    }
}

/// One supervised private Core process exposed through `AgentRuntimePort`.
pub struct CoshCoreBridge {
    supervisor: RuntimeSupervisor,
    codec: CoshCoreJsonlCodec,
    config: CoshCoreBridgeConfig,
    state: BridgeState,
    binding: Option<RuntimeBindingRef>,
    provider_session_id: Option<String>,
    pending_events: VecDeque<RuntimeEventEnvelope>,
    sequence: u64,
    current_message: Option<RuntimeMessageId>,
    tool_ids: BTreeMap<String, ObservedToolUse>,
    active_turn: Option<TurnId>,
    prompt_deadline: Option<Instant>,
    terminal_delivered: bool,
    pending_input: Option<PendingInputRequest>,
    pending_permission: Option<PendingCorePermission>,
    pending_brokered: Option<PendingBrokeredRequest>,
}

struct PendingInputRequest {
    private_request_id: String,
    request: RuntimeInputRequest,
}

struct ObservedToolUse {
    tool_use_id: ToolUseId,
    name: BoundedName,
}

struct PendingCorePermission {
    private_request_id: String,
    request_id: RequestId,
    callback: CorePermissionCallbackV1,
}

struct PendingBrokeredRequest {
    private_request_id: String,
    request_id: RequestId,
    operation: BrokeredOperation,
    acknowledged: bool,
}
