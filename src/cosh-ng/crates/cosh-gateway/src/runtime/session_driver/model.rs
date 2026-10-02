/// Deadlines and immutable launch inputs for one local ACP session.
#[derive(Debug, Clone)]
pub struct AcpSessionDriverConfig {
    /// Direct supervised Agent launch specification.
    pub launch: RuntimeLaunchSpec,
    /// ACP client identity and frame bound.
    pub client: AcpV1ClientConfig,
    /// Canonical workspace bound to the single Agent session.
    pub workspace: PathBuf,
    /// Optional workspace roots passed only when the Agent advertises support.
    pub additional_directories: Vec<PathBuf>,
    /// Maximum wait for initialize and `session/new` responses.
    pub initialize_timeout: Duration,
    /// Maximum lifetime of one active prompt.
    pub prompt_timeout: Duration,
    /// TERM grace before KILL escalation during settlement.
    pub shutdown_grace: Duration,
    /// Maximum caller wait for actor acknowledgements.
    pub command_timeout: Duration,
    /// Maximum serialized bytes retained by outstanding observation envelopes.
    pub event_byte_budget: usize,
}

impl AcpSessionDriverConfig {
    /// Builds a local single-session configuration with conservative deadlines.
    #[must_use]
    pub fn new(
        launch: RuntimeLaunchSpec,
        client: AcpV1ClientConfig,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            launch,
            client,
            workspace: workspace.into(),
            additional_directories: Vec::new(),
            initialize_timeout: DEFAULT_INITIALIZE_TIMEOUT,
            prompt_timeout: Duration::from_secs(30 * 60),
            shutdown_grace: Duration::from_secs(2),
            // Keep the caller alive after the actor's protocol deadline so it
            // receives the operation-specific result instead of a racing ack timeout.
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            event_byte_budget: DEFAULT_EVENT_BYTE_BUDGET,
        }
    }

    fn validate(&self) -> Result<(), AcpSessionDriverError> {
        let initialize_minimum = self
            .initialize_timeout
            .checked_add(self.launch.stdin_write_timeout)
            .and_then(|timeout| timeout.checked_add(CONTROL_POLL_INTERVAL))
            .ok_or(AcpSessionDriverError::InvalidDeadlineConfiguration)?;
        let shutdown_minimum = self
            .shutdown_grace
            // Supervisor settlement also reaps the child and drains the
            // bounded stderr collector after the TERM grace expires.
            .checked_add(SHUTDOWN_SETTLEMENT_MARGIN)
            .ok_or(AcpSessionDriverError::InvalidDeadlineConfiguration)?;
        if self.initialize_timeout.is_zero()
            || self.command_timeout <= initialize_minimum
            || self.command_timeout <= shutdown_minimum
            || self.prompt_timeout.is_zero()
            || self.shutdown_grace.is_zero()
            || self.event_byte_budget == 0
        {
            return Err(AcpSessionDriverError::InvalidDeadlineConfiguration);
        }
        Ok(())
    }
}

/// One locally ordered ACP observation retained against the driver byte budget.
#[derive(Debug)]
pub struct AcpSessionObservation {
    /// Strictly increasing driver-generation sequence, starting at one.
    pub sequence: u64,
    /// Validated ACP v1 observation; sequence is never part of the ACP wire value.
    pub observation: AcpV1Observation,
    _budget_lease: Option<ObservationBudgetLease>,
}

impl AcpSessionObservation {
    /// Builds an unbudgeted envelope for neutral-port adapters and test doubles.
    #[must_use]
    pub fn new(sequence: u64, observation: AcpV1Observation) -> Self {
        Self {
            sequence,
            observation,
            _budget_lease: None,
        }
    }
}

/// One bounded event delivered by the ACP session actor.
#[derive(Debug)]
pub enum AcpSessionEvent {
    /// Validated protocol observation in wire order.
    Observation(AcpSessionObservation),
    /// The sole terminal event for this driver generation.
    Terminal(AcpSessionTerminal),
}

/// Stable reason for the sole session-driver terminal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpSessionTerminalKind {
    /// Caller requested orderly shutdown.
    Shutdown,
    /// Independent control handle cancelled the active prompt.
    Cancelled,
    /// Protocol, transport, deadline, or actor coordination failed closed.
    Failed,
}

/// Final driver result emitted after the runtime child has been reaped.
#[derive(Debug)]
pub struct AcpSessionTerminal {
    /// Stable terminal classification.
    pub kind: AcpSessionTerminalKind,
    /// Bounded diagnostic without protocol payloads or secrets.
    pub detail: Option<String>,
    /// Reaped process terminal when cleanup returned it.
    pub process: Option<ProcessTerminal>,
}

/// Failure returned to a session-driver caller.
#[derive(Debug, Error)]
pub enum AcpSessionDriverError {
    /// Launching the supervised bridge failed before an actor was exposed.
    #[error(transparent)]
    Bridge(#[from] AcpV1BridgeError),
    /// A command was rejected in the current driver state.
    #[error("ACP session command {operation} is invalid while state is {state}")]
    InvalidState {
        /// Requested operation.
        operation: &'static str,
        /// Compact actor state name.
        state: &'static str,
    },
    /// A mandatory response did not arrive before its explicit deadline.
    #[error("ACP {operation} exceeded its deadline")]
    Deadline {
        /// Timed-out operation.
        operation: &'static str,
    },
    /// The actor or bounded queue is unavailable.
    #[error("ACP session actor is unavailable")]
    ActorUnavailable,
    /// The independent cancellation slot already contains a request.
    #[error("ACP cancellation is already pending")]
    CancellationPending,
    /// An event consumer failed to keep up with the bounded stream.
    #[error("ACP observation queue reached its bound")]
    ObservationBackpressure,
    /// Independent control cancelled a deadline-bound operation.
    #[error("ACP operation was cancelled")]
    Cancelled,
    /// Configured deadlines cannot preserve actor-before-caller settlement.
    #[error("ACP session deadline configuration is invalid")]
    InvalidDeadlineConfiguration,
}

#[derive(Debug)]
struct ObservationBudget {
    limit: usize,
    outstanding: AtomicUsize,
}

impl ObservationBudget {
    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<ObservationBudgetLease, ()> {
        let mut current = self.outstanding.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(bytes).ok_or(())?;
            if next > self.limit {
                return Err(());
            }
            match self.outstanding.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(ObservationBudgetLease {
                        budget: Arc::clone(self),
                        bytes,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

#[derive(Debug)]
struct ObservationBudgetLease {
    budget: Arc<ObservationBudget>,
    bytes: usize,
}

impl Drop for ObservationBudgetLease {
    fn drop(&mut self) {
        self.budget
            .outstanding
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct ObservationEmitter {
    events: SyncSender<AcpSessionEvent>,
    budget: Arc<ObservationBudget>,
    next_sequence: u64,
}

impl ObservationEmitter {
    fn new(events: SyncSender<AcpSessionEvent>, byte_budget: usize) -> Self {
        Self {
            events,
            budget: Arc::new(ObservationBudget {
                limit: byte_budget,
                outstanding: AtomicUsize::new(0),
            }),
            next_sequence: 1,
        }
    }

    fn emit(
        &mut self,
        observation: AcpV1Observation,
        cancel: &Receiver<()>,
        deadline: Instant,
        operation: &'static str,
    ) -> Result<(), AcpSessionDriverError> {
        let sequence = self.next_sequence;
        let next_sequence = sequence
            .checked_add(1)
            .ok_or(AcpSessionDriverError::ObservationBackpressure)?;
        let bytes = serialized_observation_bytes(sequence, &observation);
        if bytes > self.budget.limit {
            return Err(AcpSessionDriverError::ObservationBackpressure);
        }
        let budget_lease = loop {
            match self.budget.reserve(bytes) {
                Ok(lease) => break lease,
                Err(()) => wait_for_observation_capacity(cancel, deadline, operation)?,
            }
        };
        let mut event = AcpSessionEvent::Observation(AcpSessionObservation {
            sequence,
            observation,
            _budget_lease: Some(budget_lease),
        });
        loop {
            match self.events.try_send(event) {
                Ok(()) => {
                    self.next_sequence = next_sequence;
                    return Ok(());
                }
                Err(TrySendError::Full(returned)) => {
                    event = returned;
                    wait_for_observation_capacity(cancel, deadline, operation)?;
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(AcpSessionDriverError::ActorUnavailable);
                }
            }
        }
    }
}

fn wait_for_observation_capacity(
    cancel: &Receiver<()>,
    deadline: Instant,
    operation: &'static str,
) -> Result<(), AcpSessionDriverError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(AcpSessionDriverError::Deadline { operation });
    }
    let wait = remaining.min(CONTROL_POLL_INTERVAL);
    match cancel.recv_timeout(wait) {
        Ok(()) => Err(AcpSessionDriverError::Cancelled),
        Err(RecvTimeoutError::Timeout) => Ok(()),
        // The driver owns a sender for its entire lifetime, so disconnect is
        // equivalent to losing the caller while the actor is backpressured.
        Err(RecvTimeoutError::Disconnected) => Err(AcpSessionDriverError::Cancelled),
    }
}

type Reply = SyncSender<Result<(), AcpSessionDriverError>>;

#[derive(Debug)]
enum DriverCommand {
    Initialize(Reply),
    OpenSession(Reply),
    Prompt {
        text: String,
        reply: Reply,
    },
    Permission {
        request_id: AcpV1RequestId,
        decision: AcpV1PermissionDecision,
        reply: Reply,
    },
    Shutdown(Reply),
}

/// Cloneable cancellation path that is independent from ordinary commands.
#[derive(Debug, Clone)]
pub struct AcpSessionControl {
    cancel: SyncSender<()>,
}

impl AcpSessionControl {
    /// Enqueues cancellation without waiting for Agent stdout or actor work.
    ///
    /// # Errors
    ///
    /// Returns when cancellation is already pending or the actor exited.
    pub fn cancel(&self) -> Result<(), AcpSessionDriverError> {
        match self.cancel.try_send(()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(())) => Err(AcpSessionDriverError::CancellationPending),
            Err(TrySendError::Disconnected(())) => Err(AcpSessionDriverError::ActorUnavailable),
        }
    }
}
