pub(super) const RUNTIME_START_SCHEMA_VERSION: u16 = 4;
const DEFAULT_LEASE_DURATION_MS: u64 = 180_000;
const DEFAULT_LEASE_RENEWAL_MARGIN_MS: u64 = 60_000;
const DEFAULT_RUNTIME_OPERATION_TIMEOUT_MS: u64 = 70_000;
const DEFAULT_RUNTIME_INPUT_TIMEOUT_MS: u64 = 15 * 60 * 1_000;

pub(super) fn runtime_start_delivery_kind() -> BoundedName {
    BoundedName::new("runtime_start").unwrap_or_else(|_| unreachable!())
}

pub(super) fn pre_runtime_checkpoint_delivery_kind() -> BoundedName {
    BoundedName::new("pre_runtime_checkpoint").unwrap_or_else(|_| unreachable!())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeStartIntent {
    pub(super) schema_version: u16,
    pub(super) actor: ActorRef,
    pub(super) task_id: TaskId,
    pub(super) run_id: RunId,
    pub(super) runtime: RuntimeSelector,
    pub(super) intent: BoundedText,
    pub(super) target: TargetRef,
    pub(super) workspace: WorkspaceRef,
    pub(super) capability_profile: GatewayCapabilityProfileIdentity,
    pub(super) launch: TaskLaunchSpecV1,
    pub(super) baseline_id: Option<CheckpointId>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeStartIntentV3 {
    schema_version: u16,
    actor: ActorRef,
    task_id: TaskId,
    run_id: RunId,
    runtime: RuntimeSelector,
    intent: BoundedText,
    target: TargetRef,
    workspace: WorkspaceRef,
    capability_profile: GatewayCapabilityProfileIdentity,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeStartIntentV2 {
    schema_version: u16,
    actor: ActorRef,
    task_id: TaskId,
    run_id: RunId,
    runtime: RuntimeSelector,
    intent: BoundedText,
    target: TargetRef,
    workspace: WorkspaceRef,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeStartIntentHeader {
    schema_version: u16,
}

pub(super) fn decode_runtime_start_intent(
    value: serde_json::Value,
    launch_catalog: &TaskLaunchCatalog,
) -> Result<RuntimeStartIntent, GatewayDaemonError> {
    let header =
        serde_json::from_value::<RuntimeStartIntentHeader>(value.clone()).map_err(|error| {
            GatewayDaemonError::Protocol(format!(
                "runtime start intent has no valid schema version: {error}"
            ))
        })?;
    let source_schema_version = header.schema_version;
    let intent = match source_schema_version {
        RUNTIME_START_SCHEMA_VERSION => serde_json::from_value::<RuntimeStartIntent>(value)?,
        3 => {
            let legacy = serde_json::from_value::<RuntimeStartIntentV3>(value)?;
            if legacy.schema_version != 3 {
                return Err(GatewayDaemonError::Protocol(
                    "runtime start v3 decoder observed another schema".to_owned(),
                ));
            }
            let launch = legacy_launch_spec(
                &legacy.intent,
                &legacy.runtime,
                &legacy.workspace,
            );
            RuntimeStartIntent {
                schema_version: RUNTIME_START_SCHEMA_VERSION,
                launch,
                actor: legacy.actor,
                task_id: legacy.task_id,
                run_id: legacy.run_id,
                runtime: legacy.runtime,
                intent: legacy.intent,
                target: legacy.target,
                workspace: legacy.workspace,
                capability_profile: legacy.capability_profile,
                baseline_id: None,
            }
        }
        2 => {
            let legacy = serde_json::from_value::<RuntimeStartIntentV2>(value)?;
            if legacy.schema_version != 2 {
                return Err(GatewayDaemonError::Protocol(
                    "runtime start v2 decoder observed another schema".to_owned(),
                ));
            }
            let legacy_profile = GatewayCapabilityProfile::task_only_v1();
            if legacy.target != legacy_profile.governed_target()
                || !is_task_only_runtime(&legacy.runtime)
            {
                return Err(GatewayDaemonError::Protocol(
                    "runtime start v2 intent is compatible only with the exact task-only target and Runtime"
                        .to_owned(),
                ));
            }
            let launch = legacy_launch_spec(
                &legacy.intent,
                &legacy.runtime,
                &legacy.workspace,
            );
            RuntimeStartIntent {
                schema_version: RUNTIME_START_SCHEMA_VERSION,
                actor: legacy.actor,
                task_id: legacy.task_id,
                run_id: legacy.run_id,
                runtime: legacy.runtime,
                intent: legacy.intent,
                target: legacy.target,
                workspace: legacy.workspace,
                capability_profile: legacy_profile.identity(),
                launch,
                baseline_id: None,
            }
        }
        version if version > RUNTIME_START_SCHEMA_VERSION => {
            return Err(GatewayDaemonError::Protocol(format!(
                "runtime start intent schema {version} is newer than supported schema {RUNTIME_START_SCHEMA_VERSION}"
            )))
        }
        version => {
            return Err(GatewayDaemonError::Protocol(format!(
                "runtime start intent schema {version} is not supported"
            )))
        }
    };
    if !launch_catalog.admits_identity(&intent.runtime, &intent.target, &intent.capability_profile)
    {
        return Err(GatewayDaemonError::Protocol(
            "runtime start intent is not admitted by the Gateway launch catalog".to_owned(),
        ));
    }
    if intent.launch.goal != intent.intent || intent.launch.workspace != intent.workspace {
        return Err(GatewayDaemonError::Protocol(
            "runtime start intent launch data does not match its delivery fields".to_owned(),
        ));
    }
    if source_schema_version == RUNTIME_START_SCHEMA_VERSION {
        let routed_runtime = match intent.runtime.runtime.as_str() {
            "core" => TaskRuntime::Core,
            "acp" => TaskRuntime::Codex,
            _ => {
                return Err(GatewayDaemonError::Protocol(
                    "runtime start v4 has no provider-neutral Runtime route".to_owned(),
                ));
            }
        };
        if intent.launch.runtime != routed_runtime {
            return Err(GatewayDaemonError::Protocol(
                "runtime start v4 launch choice does not match its exact Runtime route".to_owned(),
            ));
        }
    }
    if (intent.launch.checkpoint == CheckpointPolicy::Off) != intent.baseline_id.is_none() {
        return Err(GatewayDaemonError::Protocol(
            "runtime start intent baseline identity does not match its checkpoint policy"
                .to_owned(),
        ));
    }
    Ok(intent)
}

fn legacy_launch_spec(
    intent: &BoundedText,
    runtime: &RuntimeSelector,
    workspace: &WorkspaceRef,
) -> TaskLaunchSpecV1 {
    TaskLaunchSpecV1::new(
        intent.clone(),
        if runtime.runtime.as_str() == "core" {
            TaskRuntime::Core
        } else {
            TaskRuntime::Codex
        },
        workspace.clone(),
        CheckpointPolicy::Off,
        ApprovalPolicy::Interactive,
    )
}

fn is_task_only_runtime(runtime: &RuntimeSelector) -> bool {
    runtime.runtime.as_str() == "core"
        && runtime.profile.as_ref().map(BoundedName::as_str) == Some("gateway-brokered-v1")
}

/// Immutable work description passed to an injected Runtime factory.
#[derive(Debug, Clone)]
pub struct ScheduledRun {
    /// Authenticated Task owner.
    pub actor: ActorRef,
    /// Task selected by the durable queue.
    pub task_id: TaskId,
    /// Fenced Run selected by the durable queue.
    pub run_id: RunId,
    /// Runtime and optional installed profile selected at ingress.
    pub runtime: RuntimeSelector,
    /// Original bounded Task intent retained in the private Outbox.
    pub intent: BoundedText,
    /// Governed execution target.
    pub target: TargetRef,
    /// Trusted public projection of the canonical workspace.
    pub workspace: WorkspaceRef,
    /// Capability identity durably selected when this Run was admitted.
    pub capability_profile: GatewayCapabilityProfileIdentity,
    /// Immutable versioned launch data selected for this Task.
    pub launch: TaskLaunchSpecV1,
    /// Current Run-lease generation.
    pub lease_generation: u64,
}

/// Non-blocking result from one injected Runtime handle poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePoll {
    /// No public event is ready yet.
    Pending,
    /// One validated Runtime event advanced only the durable binding sequence.
    Observed {
        /// Monotonic Runtime event sequence.
        sequence: u64,
    },
    /// One bounded progress update is ready for durable recording.
    Update {
        /// Monotonic Runtime event sequence.
        sequence: u64,
        /// Safe Task progress projection.
        update: RuntimeUpdate,
    },
    /// Provider-native execution is paused on a durable approval decision.
    PermissionRequested {
        /// Exact callback identity fenced to the active Runtime generation.
        permission: RuntimePermissionRef,
        /// Trusted normalized capability request.
        request: Box<CapabilityRequest>,
        /// Provider-facing operation description sanitized for actor review.
        summary: ToolSummary,
    },
    /// The Runtime abandoned the exact provider permission callback.
    PermissionAbandoned {
        /// Monotonic Runtime event sequence carrying the abandonment.
        sequence: u64,
        /// Callback identity that must still match the durable pending approval.
        permission: RuntimePermissionRef,
    },
    /// A COSH-owned typed operation is paused before durable takeover.
    BrokeredExecutionRequested {
        /// Exact callback identity fenced to the active Runtime generation.
        brokered: BrokeredExecutionRef,
        /// Trusted normalized capability request.
        request: Box<CapabilityRequest>,
        /// Closed operation whose side effect remains outside the Runtime.
        operation: BrokeredOperation,
        /// Provider-facing operation description sanitized for actor review.
        summary: ToolSummary,
    },
    /// A side-effect-free Runtime question is paused on durable actor input.
    InputRequested {
        /// Monotonic Runtime event sequence committed with the request.
        sequence: u64,
        /// Exact bounded request presentation and correlation identity.
        request: RuntimeInputRequest,
    },
    /// The Runtime completed successfully.
    Succeeded,
    /// The Runtime completed with a safe bounded failure.
    Failed(ContractError),
    /// The Runtime ended a turn for a previously established cancellation cause.
    Cancelled {
        /// Exact cause established before the terminal event was accepted.
        cause: RuntimeCancellationCause,
    },
}

/// Runtime-local evidence that makes a cancelled terminal expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCancellationCause {
    /// The actor denied the exact provider-native operation.
    ProviderPermissionDenied {
        /// Callback whose durable denial preceded the provider response.
        permission: RuntimePermissionRef,
    },
    /// The provider cancelled while the exact callback was still pending.
    ProviderPermissionAbandoned {
        /// Callback that was durably closed before accepting cancellation.
        permission: RuntimePermissionRef,
    },
}

/// Active provider-neutral Runtime owned by the scheduler.
pub trait RuntimeHandle: Send {
    /// Starts the prompt only after the Runtime binding is durable.
    ///
    /// # Errors
    ///
    /// Returns a bounded failure when the prompt cannot be admitted.
    fn begin(&mut self) -> Result<(), ContractError>;

    /// Polls at most one event without blocking.
    fn poll(&mut self) -> RuntimePoll;

    /// Requests cancellation and returns only after the Runtime acknowledges it.
    ///
    /// # Errors
    ///
    /// Returns a safe Runtime failure when cancellation cannot be acknowledged.
    fn shutdown(&mut self, reason: CancelReason) -> Result<(), ContractError>;

    /// Dispatches one previously persisted provider-native response.
    fn resolve_provider_permission(
        &mut self,
        permission: &RuntimePermissionRef,
        decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError>;

    /// Dispatches durable ownership of one exact brokered callback.
    fn acknowledge_brokered_request(
        &mut self,
        _brokered: &BrokeredExecutionRef,
        _acknowledgement: BrokeredRequestAcknowledgement,
    ) -> Result<(), ContractError> {
        Err(runtime_handle_unsupported("brokered acknowledgement"))
    }

    /// Dispatches the terminal result of one exact brokered callback.
    fn deliver_brokered_result(
        &mut self,
        _brokered: &BrokeredExecutionRef,
        _delivery: BrokeredExecutionDelivery,
    ) -> Result<(), ContractError> {
        Err(runtime_handle_unsupported("brokered result delivery"))
    }

    /// Dispatches one exact durable input response to the waiting Runtime.
    fn resolve_input(
        &mut self,
        _request: &RuntimeInputRequest,
        _response: RuntimeInputResponse,
    ) -> Result<(), ContractError> {
        Err(runtime_handle_unsupported("input resolution"))
    }
}

fn runtime_handle_unsupported(operation: &'static str) -> ContractError {
    ContractError::new(
        "runtime_operation_unsupported",
        ErrorCategory::InvalidRequest,
        false,
        format!("The Runtime does not support {operation}"),
    )
    .unwrap_or_else(|_| unreachable!("static Runtime operation failure must remain bounded"))
}

/// Open Runtime session returned before its prompt is dispatched.
pub struct StartedRuntime {
    /// Fenced binding emitted by the opened Runtime session.
    pub binding: RuntimeBindingRef,
    /// Runtime handle whose prompt has not started yet.
    pub handle: Box<dyn RuntimeHandle>,
}

/// Injection boundary that opens one provider-neutral Runtime.
pub trait RuntimeFactory: Send {
    /// Opens a Runtime for an already-fenced durable Run without prompting it.
    ///
    /// # Errors
    ///
    /// Returns a safe Runtime failure when no handle can be started.
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError>;
}

impl<T: RuntimeFactory + ?Sized> RuntimeFactory for Box<T> {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        (**self).open(run)
    }
}

/// Lease and blocking-operation bounds for one scheduler worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSchedulerConfig {
    /// Lifetime of each acquired or renewed Run lease.
    pub lease_duration: Duration,
    /// Lease is renewed once its remaining lifetime reaches this margin.
    pub lease_renewal_margin: Duration,
    /// Maximum configured duration of one blocking Runtime operation.
    pub runtime_operation_timeout: Duration,
}

impl Default for TaskSchedulerConfig {
    fn default() -> Self {
        Self {
            lease_duration: Duration::from_millis(DEFAULT_LEASE_DURATION_MS),
            lease_renewal_margin: Duration::from_millis(DEFAULT_LEASE_RENEWAL_MARGIN_MS),
            runtime_operation_timeout: Duration::from_millis(DEFAULT_RUNTIME_OPERATION_TIMEOUT_MS),
        }
    }
}

impl TaskSchedulerConfig {
    fn validate(self) -> Result<ValidatedSchedulerConfig, GatewayDaemonError> {
        let lease_duration_ms = duration_ms(self.lease_duration, "scheduler lease duration")?;
        let lease_renewal_margin_ms =
            duration_ms(self.lease_renewal_margin, "scheduler lease renewal margin")?;
        let runtime_operation_timeout_ms = duration_ms(
            self.runtime_operation_timeout,
            "scheduler Runtime operation timeout",
        )?;
        if lease_renewal_margin_ms == 0
            || runtime_operation_timeout_ms == 0
            || lease_duration_ms
                <= lease_renewal_margin_ms.saturating_add(runtime_operation_timeout_ms)
        {
            return Err(GatewayDaemonError::Protocol(
                "scheduler lease duration must exceed the Runtime operation timeout plus renewal margin"
                    .to_owned(),
            ));
        }
        Ok(ValidatedSchedulerConfig {
            lease_duration_ms,
            lease_renewal_margin_ms,
            runtime_operation_timeout_ms,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct ValidatedSchedulerConfig {
    lease_duration_ms: u64,
    lease_renewal_margin_ms: u64,
    runtime_operation_timeout_ms: u64,
}

/// Observable result of one bounded scheduler iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerTick {
    /// No queued or active work changed durable state.
    Idle,
    /// A queued Run was fenced and its Runtime started.
    Started(TaskView),
    /// Runtime progress was committed.
    Progressed(TaskView),
    /// A Run and its Task reached a terminal state.
    Settled(TaskView),
}

struct ActiveRun {
    scheduled: ScheduledRun,
    lease: LeaseClaim,
    lease_expires_at_ms: u64,
    next_event_sequence: u64,
    abort_error: Option<ContractError>,
    binding: RuntimeBindingRef,
    terminal: Option<TerminalOutcome>,
    binding_closed: bool,
    task_settled: bool,
    pending_permission: Option<PendingPermission>,
    expected_provider_terminal: Option<ExpectedProviderTerminal>,
    pending_brokered: Option<PendingBrokered>,
    pending_input: Option<RuntimeInputRequestRecord>,
    handle: Box<dyn RuntimeHandle>,
}

#[derive(Debug, Clone)]
struct PendingPermission {
    permission: RuntimePermissionRef,
    approval: ApprovalRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedProviderTerminal {
    Denied {
        approval_id: ApprovalId,
        permission: RuntimePermissionRef,
    },
    Abandoned {
        approval_id: ApprovalId,
        permission: RuntimePermissionRef,
    },
}

#[derive(Debug, Clone)]
enum TerminalOutcome {
    Succeeded,
    Failed(ContractError),
    Cancelled,
}

enum RuntimeStartClaim {
    Empty,
    Claimed {
        outbox: OutboxClaim,
        intent: Box<RuntimeStartIntent>,
        lease: LeaseClaim,
    },
    Recovered(TaskView),
}

/// Minimal durable scheduler for one active Runtime per worker instance.
pub struct TaskScheduler<F> {
    coordinator: TaskCoordinator,
    worker_id: BoundedOpaque,
    config: ValidatedSchedulerConfig,
    factory: F,
    brokered_driver: Box<dyn BrokeredExecutionDriver>,
    checkpoint_driver: Option<Box<dyn PreRuntimeCheckpointDriver>>,
    active: Option<ActiveRun>,
    shutting_down: bool,
    #[cfg(test)]
    fail_next_brokered_result_completion: bool,
    #[cfg(test)]
    fail_next_terminal_lease_release: bool,
    #[cfg(test)]
    fail_next_input_dispatch_completion: bool,
    #[cfg(test)]
    fail_next_input_request_install: bool,
    #[cfg(test)]
    fail_next_input_unknown_cleanup: bool,
}
