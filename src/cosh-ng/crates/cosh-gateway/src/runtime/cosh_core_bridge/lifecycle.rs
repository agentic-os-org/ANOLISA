impl CoshCoreBridge {
    /// Launches one direct Core child without admitting user input.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error when validation or spawn fails.
    pub fn launch(config: CoshCoreBridgeConfig) -> Result<Self, AgentRuntimePortError> {
        if config.prompt_timeout.is_zero() || config.shutdown_grace.is_zero() {
            return Err(AgentRuntimePortError::Deadline {
                operation: "configuration",
            });
        }
        match (config.execution_profile, config.brokered_context.as_ref()) {
            (CoshCoreExecutionProfile::Legacy, None) => {}
            (
                CoshCoreExecutionProfile::GatewayBrokeredV1
                | CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1
                | CoshCoreExecutionProfile::GatewayBrokeredWorkspaceWriteV1,
                Some(context),
            ) if config.identity.actor_id.as_ref() == Some(&context.actor.actor_id) => {}
            _ => return Err(AgentRuntimePortError::IdentityMismatch),
        }
        let initialize_request_id = format!("init-{}", config.identity.runtime_instance_id);
        let codec = match config.execution_profile {
            CoshCoreExecutionProfile::Legacy => {
                CoshCoreJsonlCodec::new(initialize_request_id, config.max_frame_bytes)
            }
            CoshCoreExecutionProfile::GatewayBrokeredV1 => {
                CoshCoreJsonlCodec::new_gateway_brokered(
                    initialize_request_id,
                    config.max_frame_bytes,
                )
            }
            CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1 => {
                CoshCoreJsonlCodec::new_gateway_brokered_checkpoint(
                    initialize_request_id,
                    config.max_frame_bytes,
                )
            }
            CoshCoreExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => {
                CoshCoreJsonlCodec::new_gateway_brokered_workspace_write(
                    initialize_request_id,
                    config.max_frame_bytes,
                )
            }
        }
        .map_err(|_| AgentRuntimePortError::Protocol)?;
        let mut supervisor = RuntimeSupervisor::new();
        supervisor
            .launch(&config.launch)
            .map_err(|_| AgentRuntimePortError::Transport)?;
        Ok(Self {
            supervisor,
            codec,
            config,
            state: BridgeState::Created,
            binding: None,
            provider_session_id: None,
            pending_events: VecDeque::new(),
            sequence: 0,
            current_message: None,
            tool_ids: BTreeMap::new(),
            active_turn: None,
            prompt_deadline: None,
            terminal_delivered: false,
            pending_input: None,
            pending_permission: None,
            pending_brokered: None,
        })
    }

    fn open_session(
        &mut self,
        task_id: TaskId,
        run_id: RunId,
        workspace: WorkspaceRef,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_state(BridgeState::Created, "open_session")?;
        self.require_run(&task_id, &run_id)?;
        if workspace != self.config.workspace {
            return Err(AgentRuntimePortError::WorkspaceMismatch);
        }
        self.require_time(deadline, "open_session")?;
        let result = (|| {
            let frame = self
                .codec
                .initialize_frame(true)
                .map_err(|_| AgentRuntimePortError::Protocol)?;
            self.supervisor
                .write_frame(&frame)
                .map_err(|_| AgentRuntimePortError::Transport)?;
            self.state = BridgeState::Opening;
            self.wait_until_session_open(deadline)
        })();
        if result.is_err() {
            self.fail_transport("core_session_open_failed");
        }
        result
    }

    fn wait_until_session_open(&mut self, deadline: Instant) -> Result<(), AgentRuntimePortError> {
        loop {
            self.require_time(deadline, "open_session")?;
            let observation = self.read_observation(deadline, "open_session")?;
            match observation {
                CoshCoreObservation::Initialized(_) => self
                    .supervisor
                    .mark_ready()
                    .map_err(|_| AgentRuntimePortError::Transport)?,
                CoshCoreObservation::System(message) if message.subtype == "init" => {
                    let provider_session_id = message
                        .provider_session_id
                        .ok_or(AgentRuntimePortError::Protocol)?;
                    self.bind_provider_session(provider_session_id)?;
                    self.state = BridgeState::SessionOpenedPending;
                    return Ok(());
                }
                CoshCoreObservation::ControlRequest(envelope)
                    if matches!(
                        envelope.request,
                        CoshCoreControlRequest::AuthRequired { .. }
                    ) =>
                {
                    return Err(AgentRuntimePortError::Unsupported {
                        operation: "core authentication bootstrap",
                    });
                }
                _ => return Err(AgentRuntimePortError::Protocol),
            }
        }
    }

    fn prompt(
        &mut self,
        run_id: RunId,
        turn_id: TurnId,
        input: Vec<ContentPart>,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_state(BridgeState::SessionOpen, "prompt")?;
        if run_id != self.config.identity.run_id {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        self.require_time(deadline, "prompt")?;
        let content = prompt_text(input, self.config.max_frame_bytes)?;
        let result = self
            .codec
            .user_frame(&CoshCoreUserTurn {
                raw_user_input: Some(content.clone()),
                content,
                provider_session_id: self.provider_session_id.clone(),
                shell_context: None,
            })
            .map_err(|_| AgentRuntimePortError::Protocol)
            .and_then(|frame| {
                self.supervisor
                    .write_frame(&frame)
                    .map_err(|_| AgentRuntimePortError::Transport)
            });
        if let Err(error) = result {
            self.fail_transport("core_prompt_write_failed");
            return Err(error);
        }
        self.active_turn = Some(turn_id.clone());
        self.tool_ids.clear();
        self.state = BridgeState::PromptActive;
        self.prompt_deadline = Instant::now().checked_add(self.config.prompt_timeout);
        if self.prompt_deadline.is_none() {
            self.fail_transport("core_prompt_deadline_invalid");
            return Err(AgentRuntimePortError::Deadline {
                operation: "prompt",
            });
        }
        let started = self.event(AgentRuntimeEvent::TurnStarted { turn_id });
        self.pending_events.push_back(started);
        Ok(())
    }

    fn cancel(
        &mut self,
        run_id: RunId,
        turn_id: TurnId,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        if run_id != self.config.identity.run_id {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        self.require_time(deadline, "cancel")?;
        if self.state == BridgeState::Terminal {
            return Ok(());
        }
        self.require_state(BridgeState::PromptActive, "cancel")?;
        if self.active_turn.as_ref() != Some(&turn_id) {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        let result = self
            .codec
            .interrupt_frame("gateway-interrupt")
            .map_err(|_| AgentRuntimePortError::Protocol)
            .and_then(|frame| {
                self.supervisor
                    .write_frame(&frame)
                    .map_err(|_| AgentRuntimePortError::Transport)
            });
        if let Err(error) = result {
            self.fail_transport("core_cancel_write_failed");
            return Err(error);
        }
        self.active_turn = None;
        self.pending_input = None;
        self.pending_permission = None;
        self.pending_brokered = None;
        self.settle(AgentRuntimeEvent::Completed {
            turn_id,
            outcome: TurnOutcome::Cancelled,
        });
        self.shutdown_process();
        Ok(())
    }

    fn close(
        &mut self,
        binding: RuntimeBindingRef,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_time(deadline, "close")?;
        if self.binding.as_ref() != Some(&binding) {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        if self.state == BridgeState::Terminal {
            return Ok(());
        }
        if matches!(
            self.state,
            BridgeState::SessionOpen | BridgeState::PromptActive
        ) {
            if let Ok(frame) = self.codec.shutdown_frame("gateway-shutdown") {
                let _ = self.supervisor.write_frame(&frame);
            }
        }
        if self.state == BridgeState::PromptActive {
            let turn_id = self
                .active_turn
                .take()
                .ok_or(AgentRuntimePortError::Protocol)?;
            self.settle(AgentRuntimeEvent::Completed {
                turn_id,
                outcome: TurnOutcome::Cancelled,
            });
            self.pending_input = None;
            self.pending_permission = None;
            self.pending_brokered = None;
        } else {
            self.state = BridgeState::Terminal;
        }
        self.shutdown_process();
        Ok(())
    }
}
