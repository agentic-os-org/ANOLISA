//! Extension generation construction, binding, and resource shutdown.

use super::*;

impl CoshCore {
    /// Creates a core for the standalone legacy execution boundary.
    pub fn new_legacy(
        config: CoreConfig,
        provider: Box<dyn ContentGenerator>,
        tools: ToolRegistry,
    ) -> Self {
        Self::new_with_profile(config, provider, tools, ExecutionProfile::Legacy)
    }

    /// Creates a core after the caller explicitly selects its execution boundary.
    pub fn new_with_profile(
        config: CoreConfig,
        provider: Box<dyn ContentGenerator>,
        tools: ToolRegistry,
        execution_profile: ExecutionProfile,
    ) -> Self {
        let tools = Arc::new(tools);
        let snapshot = RuntimeSnapshot::bootstrap(
            RuntimeGeneration::healthy(1, "startup"),
            Arc::clone(&tools),
        );
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let workspace = SessionWorkspace::new(&project_root);
        Self::new_with_snapshot_and_session_id(
            config,
            provider,
            snapshot,
            uuid::Uuid::new_v4().to_string(),
            project_root,
            workspace,
            execution_profile,
        )
    }

    /// Creates a core bound to a complete validated extension runtime snapshot.
    pub fn new_with_snapshot(
        config: CoreConfig,
        provider: Box<dyn ContentGenerator>,
        snapshot: RuntimeSnapshot,
        execution_profile: ExecutionProfile,
    ) -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let workspace = SessionWorkspace::new(&project_root);
        Self::new_with_snapshot_and_session_id(
            config,
            provider,
            snapshot,
            uuid::Uuid::new_v4().to_string(),
            project_root,
            workspace,
            execution_profile,
        )
    }

    pub(crate) fn new_with_snapshot_and_session_id(
        config: CoreConfig,
        provider: Box<dyn ContentGenerator>,
        snapshot: RuntimeSnapshot,
        session_id: String,
        project_root: PathBuf,
        workspace: SessionWorkspace,
        execution_profile: ExecutionProfile,
    ) -> Self {
        let model = config.resolve_provider().model;
        let (loaded_policy, warning) = LoadedPolicy::load();
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }

        let mut hook_system = HookSystem::from_config(&config.hooks);
        hook_system.register_extension_hooks(&snapshot.hooks);
        let extension_context = snapshot.context.rendered().map(str::to_string);
        let tools = Arc::clone(&snapshot.tools);
        let bound_extension_generation = snapshot.generation.id;
        let extension_generation = GenerationController::new(snapshot);
        let audit_workspace = std::env::current_dir().ok();
        let audit = CoreAuditRecorder::initialize(&session_id, audit_workspace.as_deref());
        let mut core = Self {
            config,
            provider,
            tools,
            session_id,
            messages: Vec::new(),
            compaction: CompactionRuntime::default(),
            model,
            session_resumed: false,
            shell_context: None,
            project_root,
            workspace,
            extension_context,
            extra_params: None,
            hook_system,
            metrics: TurnMetrics::default(),
            audit,
            extension_generation,
            bound_extension_generation,
            loaded_policy,
            request_counter: AtomicU32::new(0),
            truncator: OutputTruncator::default(),
            loop_detector: LoopDetector::new(),
            client_capabilities: crate::protocol::ClientControlCapabilities::default(),
            approval_response_timeout_default: Duration::from_secs(
                APPROVAL_RESPONSE_TIMEOUT_DEFAULT_SECS,
            ),
            approval_timed_out: std::sync::atomic::AtomicBool::new(false),
            control_transport_failure: std::sync::OnceLock::new(),
            execution_profile,
        };
        core.apply_execution_profile_constraints();
        core
    }

    /// Gracefully drains MCP processes retired by safe generation switches.
    pub async fn drain_retired_extension_snapshots(&self) {
        for snapshot in self.extension_generation.take_retired() {
            snapshot.mcp.shutdown().await;
        }
    }

    /// Gracefully shuts down current and retired extension runtime resources.
    pub async fn shutdown_extension_runtime(&self) {
        self.drain_retired_extension_snapshots().await;
        self.extension_generation.current().mcp.shutdown().await;
    }

    pub(super) fn bind_current_extension_snapshot(&mut self) {
        if self.execution_profile.is_brokered() {
            return;
        }
        let snapshot = self.extension_generation.current();
        if snapshot.generation.id == self.bound_extension_generation {
            return;
        }
        self.tools = Arc::clone(&snapshot.tools);
        self.extension_context = snapshot.context.rendered().map(str::to_string);
        self.hook_system = HookSystem::from_config(&self.config.hooks);
        self.hook_system.register_extension_hooks(&snapshot.hooks);
        self.bound_extension_generation = snapshot.generation.id;
    }
}
