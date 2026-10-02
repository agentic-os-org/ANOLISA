/// A newly created Runtime port and its trusted workspace projection.
///
/// Profile resolution, process identity, actor authentication, and permission
/// normalization remain owned by the injected factory. The scheduler adapter
/// never derives those security-sensitive values from a Task's display data.
pub struct ScheduledRuntimePort {
    port: Box<dyn AgentRuntimePort>,
    workspace: WorkspaceRef,
}

impl ScheduledRuntimePort {
    /// Binds a Runtime port to the trusted workspace accepted by `OpenSession`.
    #[must_use]
    pub fn new(port: Box<dyn AgentRuntimePort>, workspace: WorkspaceRef) -> Self {
        Self { port, workspace }
    }
}

/// Injection boundary that resolves a scheduled Run into a concrete port.
///
/// Production implementations are responsible for validating the Runtime
/// selector and target, allocating fenced identities, and installing a safe
/// permission normalizer before returning the port.
pub trait AgentRuntimePortFactory: Send {
    /// Creates one unopened Runtime port for an already-fenced scheduled Run.
    ///
    /// # Errors
    ///
    /// Returns a bounded failure when the profile, target, identity, workspace,
    /// or supervised Runtime cannot be resolved safely.
    fn create(&mut self, run: &ScheduledRun) -> Result<ScheduledRuntimePort, ContractError>;
}

impl<T: AgentRuntimePortFactory + ?Sized> AgentRuntimePortFactory for Box<T> {
    fn create(&mut self, run: &ScheduledRun) -> Result<ScheduledRuntimePort, ContractError> {
        (**self).create(run)
    }
}

/// Scheduler factory backed by an injected provider-neutral Runtime factory.
pub struct ScheduledAgentRuntimeFactory<F> {
    port_factory: F,
    command_timeout: Duration,
}

impl<F> ScheduledAgentRuntimeFactory<F> {
    /// Creates an adapter with the conservative Runtime command timeout.
    #[must_use]
    pub fn new(port_factory: F) -> Self {
        Self {
            port_factory,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }

    /// Overrides the deadline shared by open, prompt, permission, and close.
    ///
    /// Zero is rejected when the first Runtime is started.
    #[must_use]
    pub fn with_command_timeout(mut self, command_timeout: Duration) -> Self {
        self.command_timeout = command_timeout;
        self
    }
}

impl<F: AgentRuntimePortFactory> RuntimeFactory for ScheduledAgentRuntimeFactory<F> {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        if self.command_timeout.is_zero() {
            return Err(contract_error(
                "runtime_deadline_invalid",
                ErrorCategory::InvalidRequest,
                false,
                "The Runtime command deadline is invalid",
            ));
        }

        let ScheduledRuntimePort {
            mut port,
            workspace,
        } = self.port_factory.create(run)?;
        if workspace != run.workspace {
            return Err(contract_error(
                "runtime_workspace_projection_mismatch",
                ErrorCategory::InvalidRequest,
                false,
                "The resolved Runtime workspace does not match the admitted workspace",
            ));
        }
        let binding_id = port.binding_id().clone();
        let open_deadline = deadline(self.command_timeout);
        port.dispatch(
            AgentRuntimeCommand::OpenSession {
                task_id: run.task_id.clone(),
                run_id: run.run_id.clone(),
                workspace,
            },
            open_deadline,
        )
        .map_err(map_port_error)?;

        let opened = port.next_event(open_deadline).map_err(map_port_error)?;
        validate_event(run, &binding_id, 1, &opened)?;
        let binding = match opened.event {
            AgentRuntimeEvent::SessionOpened { binding }
                if binding.binding_id == binding_id
                    && binding.task_id == run.task_id
                    && binding.run_id == run.run_id
                    && binding.runtime_generation == run.lease_generation =>
            {
                binding
            }
            _ => {
                return Err(contract_error(
                    "runtime_event_order_invalid",
                    ErrorCategory::Internal,
                    false,
                    "The Runtime emitted an invalid session event",
                ));
            }
        };

        let handle = ScheduledAgentRuntimeHandle {
            port,
            actor_id: run.actor.actor_id.clone(),
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            binding_id,
            binding: binding.clone(),
            turn_id: TurnId::new(),
            intent: run.intent.clone(),
            last_sequence: 1,
            turn_started: false,
            prompt_started: false,
            pending_permission: None,
            expected_cancellation: None,
            pending_brokered: None,
            pending_input: None,
            command_timeout: self.command_timeout,
            terminal: false,
        };
        Ok(StartedRuntime {
            binding,
            handle: Box::new(handle),
        })
    }
}

struct ScheduledAgentRuntimeHandle {
    port: Box<dyn AgentRuntimePort>,
    actor_id: cosh_gateway_contracts::ids::ActorId,
    task_id: cosh_gateway_contracts::ids::TaskId,
    run_id: cosh_gateway_contracts::ids::RunId,
    target: cosh_gateway_contracts::common::TargetRef,
    binding_id: RuntimeBindingId,
    binding: RuntimeBindingRef,
    turn_id: TurnId,
    intent: BoundedText,
    last_sequence: u64,
    turn_started: bool,
    prompt_started: bool,
    pending_permission: Option<RuntimePermissionRef>,
    expected_cancellation: Option<RuntimeCancellationCause>,
    pending_brokered: Option<PendingBrokeredCallback>,
    pending_input: Option<InputRequestId>,
    command_timeout: Duration,
    terminal: bool,
}

struct PendingBrokeredCallback {
    reference: BrokeredExecutionRef,
    acknowledged: bool,
    dispatch_indeterminate: bool,
}
