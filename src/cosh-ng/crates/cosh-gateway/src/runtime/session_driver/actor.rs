#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorState {
    Created,
    Initialized,
    SessionOpen,
    PromptActive,
    Terminal,
}

impl ActorState {
    fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Initialized => "initialized",
            Self::SessionOpen => "session-open",
            Self::PromptActive => "prompt-active",
            Self::Terminal => "terminal",
        }
    }
}

fn run_actor(
    mut bridge: AcpV1RuntimeBridge,
    config: AcpSessionDriverConfig,
    commands: Receiver<DriverCommand>,
    cancel: Receiver<()>,
    mut events: ObservationEmitter,
    terminal: SyncSender<AcpSessionTerminal>,
) {
    let mut state = ActorState::Created;
    let mut prompt_deadline = None;
    loop {
        match cancel.try_recv() {
            Ok(()) => {
                settle_cancel(&mut bridge, &config, &terminal, state);
                break;
            }
            Err(TryRecvError::Disconnected | TryRecvError::Empty) => {}
        }

        match commands.try_recv() {
            Ok(command) => {
                if handle_command(
                    command,
                    &mut bridge,
                    &config,
                    &mut events,
                    &terminal,
                    &cancel,
                    &mut state,
                    &mut prompt_deadline,
                ) {
                    break;
                }
                continue;
            }
            Err(TryRecvError::Disconnected) => {
                settle_cancel(&mut bridge, &config, &terminal, state);
                break;
            }
            Err(TryRecvError::Empty) => {}
        }

        if state != ActorState::PromptActive {
            thread::sleep(CONTROL_POLL_INTERVAL);
            continue;
        }
        if prompt_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            fail_terminal(
                &mut bridge,
                &config,
                &terminal,
                AcpSessionDriverError::Deadline {
                    operation: "prompt",
                },
            );
            break;
        }
        match bridge.read_observation_timeout(CONTROL_POLL_INTERVAL) {
            Ok(AcpV1BridgeRead::TimedOut) => {}
            Ok(AcpV1BridgeRead::Observation(observation)) => {
                if let Err(error) = settle_unsupported(&mut bridge, &observation) {
                    fail_terminal(&mut bridge, &config, &terminal, error);
                    break;
                }
                let finished = matches!(
                    observation,
                    AcpV1Observation::PromptFinished { .. }
                        | AcpV1Observation::PromptCancelledWithPendingPermissions { .. }
                        | AcpV1Observation::RequestFailed { .. }
                );
                let Some(deadline) = prompt_deadline else {
                    fail_terminal(
                        &mut bridge,
                        &config,
                        &terminal,
                        AcpSessionDriverError::ActorUnavailable,
                    );
                    break;
                };
                match events.emit(observation, &cancel, deadline, "prompt") {
                    Ok(()) => {}
                    Err(AcpSessionDriverError::Cancelled) => {
                        settle_cancel(&mut bridge, &config, &terminal, state);
                        break;
                    }
                    Err(error) => {
                        fail_terminal(&mut bridge, &config, &terminal, error);
                        break;
                    }
                }
                if finished {
                    state = ActorState::SessionOpen;
                    prompt_deadline = None;
                }
            }
            Err(error) => {
                fail_terminal(&mut bridge, &config, &terminal, error.into());
                break;
            }
        }
    }
}

fn handle_command(
    command: DriverCommand,
    bridge: &mut AcpV1RuntimeBridge,
    config: &AcpSessionDriverConfig,
    events: &mut ObservationEmitter,
    terminal_events: &SyncSender<AcpSessionTerminal>,
    cancel: &Receiver<()>,
    state: &mut ActorState,
    prompt_deadline: &mut Option<Instant>,
) -> bool {
    let (reply, result, terminal) = match command {
        DriverCommand::Initialize(reply) => {
            let result = require_state(*state, ActorState::Created, "initialize").and_then(|()| {
                bridge.send_initialize()?;
                wait_for(
                    bridge,
                    events,
                    cancel,
                    config.initialize_timeout,
                    "initialize",
                    |observation| matches!(observation, AcpV1Observation::Initialized { .. }),
                )?;
                *state = ActorState::Initialized;
                Ok(())
            });
            (reply, result, false)
        }
        DriverCommand::OpenSession(reply) => {
            let result =
                require_state(*state, ActorState::Initialized, "open_session").and_then(|()| {
                    bridge.send_new_session(
                        config.workspace.clone(),
                        config.additional_directories.clone(),
                    )?;
                    wait_for(
                        bridge,
                        events,
                        cancel,
                        config.initialize_timeout,
                        "session/new",
                        |observation| matches!(observation, AcpV1Observation::SessionOpened { .. }),
                    )?;
                    *state = ActorState::SessionOpen;
                    Ok(())
                });
            (reply, result, false)
        }
        DriverCommand::Prompt { text, reply } => {
            let result = require_state(*state, ActorState::SessionOpen, "prompt").and_then(|()| {
                bridge.send_prompt(text)?;
                *state = ActorState::PromptActive;
                *prompt_deadline = Some(Instant::now() + config.prompt_timeout);
                Ok(())
            });
            (reply, result, false)
        }
        DriverCommand::Permission {
            request_id,
            decision,
            reply,
        } => {
            let result = require_state(*state, ActorState::PromptActive, "answer_permission")
                .and_then(|()| {
                    bridge.send_permission_decision(&request_id, decision)?;
                    Ok(())
                });
            (reply, result, false)
        }
        DriverCommand::Shutdown(reply) => {
            let result = settle(
                bridge,
                config,
                terminal_events,
                AcpSessionTerminalKind::Shutdown,
                None,
            );
            *state = ActorState::Terminal;
            (reply, result, true)
        }
    };
    let fatal = result.as_ref().err().and_then(|error| match error {
        AcpSessionDriverError::InvalidState { .. } => None,
        AcpSessionDriverError::Cancelled => {
            Some((AcpSessionTerminalKind::Cancelled, error.to_string()))
        }
        error => Some((AcpSessionTerminalKind::Failed, error.to_string())),
    });
    let _ = reply.send(result);
    if terminal {
        return true;
    }
    if let Some((kind, detail)) = fatal {
        let _ = settle(bridge, config, terminal_events, kind, Some(detail));
        *state = ActorState::Terminal;
        return true;
    }
    false
}

fn require_state(
    actual: ActorState,
    expected: ActorState,
    operation: &'static str,
) -> Result<(), AcpSessionDriverError> {
    if actual == expected {
        Ok(())
    } else {
        Err(AcpSessionDriverError::InvalidState {
            operation,
            state: actual.name(),
        })
    }
}

fn wait_for(
    bridge: &mut AcpV1RuntimeBridge,
    events: &mut ObservationEmitter,
    cancel: &Receiver<()>,
    timeout: Duration,
    operation: &'static str,
    expected: impl Fn(&AcpV1Observation) -> bool,
) -> Result<(), AcpSessionDriverError> {
    let deadline = Instant::now() + timeout;
    loop {
        match cancel.try_recv() {
            Ok(()) => return Err(AcpSessionDriverError::Cancelled),
            Err(TryRecvError::Disconnected | TryRecvError::Empty) => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AcpSessionDriverError::Deadline { operation });
        }
        match bridge.read_observation_timeout(remaining.min(CONTROL_POLL_INTERVAL))? {
            AcpV1BridgeRead::TimedOut => {}
            AcpV1BridgeRead::Observation(observation) => {
                settle_unsupported(bridge, &observation)?;
                let matched = expected(&observation);
                events.emit(observation, cancel, deadline, operation)?;
                if matched {
                    return Ok(());
                }
            }
        }
    }
}

fn settle_unsupported(
    bridge: &mut AcpV1RuntimeBridge,
    observation: &AcpV1Observation,
) -> Result<(), AcpSessionDriverError> {
    if let AcpV1Observation::UnsupportedClientRequest { request_id, .. } = observation {
        bridge.reject_unsupported_request(request_id)?;
    }
    Ok(())
}

fn serialized_observation_bytes(sequence: u64, observation: &AcpV1Observation) -> usize {
    // This local accounting projection intentionally is not an ACP wire schema.
    // Debug includes every validated payload while JSON escaping accounts for
    // expansion in downstream structured reporters.
    serde_json::to_vec(&(sequence, format!("{observation:?}")))
        .map_or(usize::MAX, |encoded| encoded.len())
}

fn settle_cancel(
    bridge: &mut AcpV1RuntimeBridge,
    config: &AcpSessionDriverConfig,
    terminal: &SyncSender<AcpSessionTerminal>,
    state: ActorState,
) {
    let detail = if state == ActorState::PromptActive {
        bridge.send_cancel().err().map(|error| error.to_string())
    } else {
        None
    };
    let _ = settle(
        bridge,
        config,
        terminal,
        AcpSessionTerminalKind::Cancelled,
        detail,
    );
}

fn fail_terminal(
    bridge: &mut AcpV1RuntimeBridge,
    config: &AcpSessionDriverConfig,
    terminal: &SyncSender<AcpSessionTerminal>,
    error: AcpSessionDriverError,
) {
    let _ = settle(
        bridge,
        config,
        terminal,
        AcpSessionTerminalKind::Failed,
        Some(error.to_string()),
    );
}

fn settle(
    bridge: &mut AcpV1RuntimeBridge,
    config: &AcpSessionDriverConfig,
    terminal_events: &SyncSender<AcpSessionTerminal>,
    kind: AcpSessionTerminalKind,
    detail: Option<String>,
) -> Result<(), AcpSessionDriverError> {
    let shutdown = bridge.shutdown(config.shutdown_grace);
    let (process, cleanup_error) = match &shutdown {
        Ok(process) => (process.clone(), None),
        Err(error) => (
            bridge.poll_terminal().ok().flatten(),
            Some(error.to_string()),
        ),
    };
    let detail = match (detail, cleanup_error) {
        (Some(detail), Some(cleanup)) => Some(format!("{detail}; cleanup failed: {cleanup}")),
        (Some(detail), None) => Some(detail),
        (None, Some(cleanup)) => Some(format!("cleanup failed: {cleanup}")),
        (None, None) => None,
    }
    .map(|detail| bounded_detail(&detail));
    // The dedicated one-shot slot reserves terminal delivery even when a
    // consumer stopped draining the bounded observation stream.
    let _ = terminal_events.try_send(AcpSessionTerminal {
        kind,
        detail,
        process,
    });
    match shutdown {
        Ok(_) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn bounded_detail(detail: &str) -> String {
    if detail.len() <= MAX_TERMINAL_DETAIL_BYTES {
        return detail.to_owned();
    }
    let mut end = MAX_TERMINAL_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_owned()
}
