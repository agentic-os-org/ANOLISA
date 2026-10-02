/// Public handle for one actor-owned ACP connection and session.
#[derive(Debug)]
pub struct AcpSessionDriver {
    commands: SyncSender<DriverCommand>,
    events: Receiver<AcpSessionEvent>,
    terminal: Receiver<AcpSessionTerminal>,
    control: AcpSessionControl,
    actor: Option<JoinHandle<()>>,
    command_timeout: Duration,
}

impl AcpSessionDriver {
    /// Launches the Agent and starts the sole bridge owner thread.
    ///
    /// # Errors
    ///
    /// Returns bridge launch or actor thread creation failures.
    pub fn launch(config: AcpSessionDriverConfig) -> Result<Self, AcpSessionDriverError> {
        config.validate()?;
        let bridge = AcpV1RuntimeBridge::launch(&config.launch, config.client.clone())?;
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (cancel_sender, cancel_receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let (event_sender, event_receiver) = mpsc::sync_channel(EVENT_CAPACITY);
        let (terminal_sender, terminal_receiver) = mpsc::sync_channel(1);
        let command_timeout = config.command_timeout;
        let actor = thread::Builder::new()
            .name("cosh-acp-session".to_owned())
            .spawn(move || {
                let observation_emitter =
                    ObservationEmitter::new(event_sender, config.event_byte_budget);
                run_actor(
                    bridge,
                    config,
                    command_receiver,
                    cancel_receiver,
                    observation_emitter,
                    terminal_sender,
                )
            })
            .map_err(|_| AcpSessionDriverError::ActorUnavailable)?;
        Ok(Self {
            commands: command_sender,
            events: event_receiver,
            terminal: terminal_receiver,
            control: AcpSessionControl {
                cancel: cancel_sender,
            },
            actor: Some(actor),
            command_timeout,
        })
    }

    /// Returns an independent cancellation handle.
    #[must_use]
    pub fn control(&self) -> AcpSessionControl {
        self.control.clone()
    }

    /// Negotiates ACP wire version 1 within the initialization deadline.
    pub fn initialize(&self) -> Result<(), AcpSessionDriverError> {
        self.request(DriverCommand::Initialize)
    }

    /// Opens the single configured canonical workspace session.
    pub fn open_session(&self) -> Result<(), AcpSessionDriverError> {
        self.request(DriverCommand::OpenSession)
    }

    /// Starts the only active text prompt.
    pub fn prompt(&self, text: impl Into<String>) -> Result<(), AcpSessionDriverError> {
        let text = text.into();
        self.request(move |reply| DriverCommand::Prompt { text, reply })
    }

    /// Answers one correlated permission callback exactly once.
    pub fn answer_permission(
        &self,
        request_id: AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<(), AcpSessionDriverError> {
        self.request(move |reply| DriverCommand::Permission {
            request_id,
            decision,
            reply,
        })
    }

    /// Receives one event before `timeout` expires.
    pub fn receive_timeout(&self, timeout: Duration) -> Result<AcpSessionEvent, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            match self
                .events
                .recv_timeout(remaining.min(CONTROL_POLL_INTERVAL))
            {
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return self
                        .terminal
                        .recv_timeout(remaining)
                        .map(AcpSessionEvent::Terminal);
                }
            }
        }
    }

    /// Requests orderly process settlement.
    pub fn shutdown(&self) -> Result<(), AcpSessionDriverError> {
        self.request(DriverCommand::Shutdown)
    }

    fn request<F>(&self, build: F) -> Result<(), AcpSessionDriverError>
    where
        F: FnOnce(Reply) -> DriverCommand,
    {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let deadline = Instant::now() + self.command_timeout;
        let mut command = build(reply_sender);
        loop {
            match self.commands.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    command = returned;
                    if Instant::now() >= deadline {
                        let _ = self.control.cancel();
                        return Err(AcpSessionDriverError::Deadline {
                            operation: "command queue",
                        });
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(AcpSessionDriverError::ActorUnavailable);
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match reply_receiver.recv_timeout(remaining) {
            Ok(result) => result,
            Err(_) => {
                let _ = self.control.cancel();
                Err(AcpSessionDriverError::Deadline {
                    operation: "command acknowledgement",
                })
            }
        }
    }
}

impl Drop for AcpSessionDriver {
    fn drop(&mut self) {
        let _ = self.control.cancel();
        if let Some(actor) = self.actor.take() {
            let _ = actor.join();
        }
    }
}
