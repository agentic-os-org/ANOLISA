impl AcpAgentRuntime {
    /// Launches one ACP adapter without admitting a prompt or side effect.
    pub fn launch(
        config: AcpAgentRuntimeConfig,
        normalizer: Box<dyn AcpPermissionNormalizer>,
    ) -> Result<Self, AgentRuntimePortError> {
        let driver = AcpSessionDriver::launch(config.session.clone()).map_err(map_driver_error)?;
        Ok(Self::with_backend(config, normalizer, Box::new(driver)))
    }

    fn with_backend(
        config: AcpAgentRuntimeConfig,
        normalizer: Box<dyn AcpPermissionNormalizer>,
        backend: Box<dyn AcpSessionBackend>,
    ) -> Self {
        Self {
            backend,
            normalizer,
            config,
            state: PortState::Created,
            binding: None,
            provider_session: None,
            events: VecDeque::new(),
            sequence: 0,
            messages: BTreeMap::new(),
            active_turn: None,
            tools: ToolInvocationAccumulator::provider_native(),
            permissions: BTreeMap::new(),
            terminal_delivered: false,
        }
    }

    fn open(
        &mut self,
        task_id: TaskId,
        run_id: RunId,
        workspace: WorkspaceRef,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_state(PortState::Created, "open_session")?;
        self.require_run(&task_id, &run_id)?;
        if workspace != self.config.workspace {
            return Err(AgentRuntimePortError::WorkspaceMismatch);
        }
        self.require_time(deadline, "open_session")?;
        let result = self
            .backend
            .initialize()
            .and_then(|()| self.backend.open_session())
            .map_err(map_driver_error);
        if result.is_err() || Instant::now() >= deadline {
            self.fail_and_shutdown("acp_session_open_failed")?;
            return result.and(Err(AgentRuntimePortError::Deadline {
                operation: "open_session",
            }));
        }
        loop {
            if Instant::now() >= deadline {
                self.fail_and_shutdown("acp_session_open_failed")?;
                return Err(AgentRuntimePortError::Deadline {
                    operation: "open_session",
                });
            }
            match self.backend.receive_timeout(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(EVENT_POLL_INTERVAL),
            ) {
                Ok(AcpSessionEvent::Observation(observation)) => match observation.observation {
                    AcpV1Observation::Initialized { .. } => {}
                    AcpV1Observation::SessionOpened { session_id } => {
                        self.bind_session(session_id)?;
                        self.state = PortState::SessionOpenedPending;
                        return Ok(());
                    }
                    _ => {
                        self.fail_and_shutdown("acp_session_open_failed")?;
                        return Err(AgentRuntimePortError::Protocol);
                    }
                },
                Ok(AcpSessionEvent::Terminal(_))
                | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.settle(AgentRuntimeEvent::TransportFailed {
                        error: safe_error(
                            "acp_session_open_failed",
                            ErrorCategory::Transport,
                            false,
                            "The ACP runtime transport failed",
                        ),
                    });
                    return Err(AgentRuntimePortError::Transport);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
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
        self.require_state(PortState::SessionOpen, "prompt")?;
        self.require_run(&self.config.identity.task_id.clone(), &run_id)?;
        self.require_time(deadline, "prompt")?;
        self.backend
            .prompt(prompt_text(input)?)
            .map_err(map_driver_error)?;
        if Instant::now() >= deadline {
            self.backend.cancel().map_err(map_driver_error)?;
            self.await_terminal(deadline, "prompt")?;
            self.settle(AgentRuntimeEvent::TransportFailed {
                error: safe_error(
                    "acp_prompt_deadline",
                    ErrorCategory::Transport,
                    false,
                    "The ACP runtime transport failed",
                ),
            });
            return Err(AgentRuntimePortError::Deadline {
                operation: "prompt",
            });
        }
        self.active_turn = Some(turn_id.clone());
        self.messages.clear();
        self.state = PortState::PromptActive;
        let started = self.event(AgentRuntimeEvent::TurnStarted { turn_id });
        self.events.push_back(started);
        Ok(())
    }

    fn resolve(
        &mut self,
        request_id: RequestId,
        decision: RuntimePermissionDecision,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_state(PortState::PromptActive, "resolve_permission")?;
        self.require_time(deadline, "resolve_permission")?;
        let pending = self
            .permissions
            .get(&request_id)
            .cloned()
            .ok_or(AgentRuntimePortError::IdentityMismatch)?;
        let selected = match decision {
            RuntimePermissionDecision::ProviderNativeAllowOnce => pending.allow_once,
            RuntimePermissionDecision::RuntimeNativeAllowOnce => {
                return Err(AgentRuntimePortError::Unsupported {
                    operation: "runtime-native permission decision",
                });
            }
            RuntimePermissionDecision::Deny { .. } => pending.reject_once,
        };
        let missing_one_shot = selected.is_none();
        let acp_decision = selected.map_or(AcpV1PermissionDecision::Cancelled, |option_id| {
            AcpV1PermissionDecision::Selected { option_id }
        });
        if let Err(error) = self
            .backend
            .answer_permission(pending.acp_request_id, acp_decision)
            .map_err(map_driver_error)
        {
            self.fail_and_reap("acp_permission_failed", deadline)?;
            return Err(error);
        }
        self.permissions.remove(&request_id);
        if missing_one_shot {
            return Err(AgentRuntimePortError::Unsupported {
                operation: "one-shot permission option",
            });
        }
        self.require_time(deadline, "resolve_permission")
    }

    fn cancel(
        &mut self,
        run_id: RunId,
        turn_id: TurnId,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_run(&self.config.identity.task_id.clone(), &run_id)?;
        self.require_time(deadline, "cancel")?;
        self.require_state(PortState::PromptActive, "cancel")?;
        if self.active_turn.as_ref() != Some(&turn_id) {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        self.backend.cancel().map_err(map_driver_error)?;
        self.await_terminal(deadline, "cancel")?;
        self.active_turn = None;
        self.settle(AgentRuntimeEvent::Completed {
            turn_id,
            outcome: TurnOutcome::Cancelled,
        });
        Ok(())
    }

    fn next(&mut self, deadline: Instant) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        if let Some(event) = self.events.pop_front() {
            return self.deliver(event);
        }
        if self.state == PortState::Terminal {
            return Err(AgentRuntimePortError::Terminal);
        }
        if self.state != PortState::PromptActive {
            return Err(AgentRuntimePortError::InvalidState {
                operation: "next_event",
                state: self.state.name(),
            });
        }
        loop {
            self.require_time(deadline, "next_event")?;
            match self.backend.receive_timeout(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(EVENT_POLL_INTERVAL),
            ) {
                Ok(AcpSessionEvent::Observation(observation)) => {
                    match self.map_observation(observation.observation) {
                        Ok(Some(event)) => return self.deliver(event),
                        Ok(None) => {}
                        Err(error) => {
                            self.fail_and_reap("acp_protocol_failed", deadline)?;
                            return self
                                .events
                                .pop_front()
                                .ok_or(error)
                                .and_then(|event| self.deliver(event));
                        }
                    }
                }
                Ok(AcpSessionEvent::Terminal(terminal)) => {
                    match terminal.kind {
                        AcpSessionTerminalKind::Cancelled => {
                            let turn_id = self
                                .active_turn
                                .take()
                                .ok_or(AgentRuntimePortError::Protocol)?;
                            self.settle(AgentRuntimeEvent::Completed {
                                turn_id,
                                outcome: TurnOutcome::Cancelled,
                            });
                        }
                        AcpSessionTerminalKind::Failed | AcpSessionTerminalKind::Shutdown => self
                            .settle(AgentRuntimeEvent::TransportFailed {
                                error: safe_error(
                                    "acp_transport_failed",
                                    ErrorCategory::Transport,
                                    false,
                                    "The ACP runtime transport failed",
                                ),
                            }),
                    }
                    return self
                        .events
                        .pop_front()
                        .ok_or(AgentRuntimePortError::Terminal)
                        .and_then(|event| self.deliver(event));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.settle(AgentRuntimeEvent::TransportFailed {
                        error: safe_error(
                            "acp_transport_failed",
                            ErrorCategory::Transport,
                            false,
                            "The ACP runtime transport failed",
                        ),
                    });
                    return self
                        .events
                        .pop_front()
                        .ok_or(AgentRuntimePortError::Terminal)
                        .and_then(|event| self.deliver(event));
                }
            }
        }
    }

    fn map_observation(
        &mut self,
        observation: AcpV1Observation,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        match observation {
            AcpV1Observation::SessionUpdate { session_id, update } => {
                self.require_session(&session_id)?;
                self.map_update(&update)
            }
            AcpV1Observation::PermissionRequested(request) => {
                self.map_permission_requested(request)
            }
            AcpV1Observation::PromptCancelledWithPendingPermissions {
                session_id,
                request_ids,
            } => {
                self.require_session(&session_id)?;
                let turn_id = self
                    .active_turn
                    .take()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let mut abandoned = Vec::with_capacity(request_ids.len());
                for provider_request_id in request_ids {
                    let request_id = self
                        .permissions
                        .iter()
                        .find_map(|(request_id, pending)| {
                            (pending.acp_request_id == provider_request_id)
                                .then(|| request_id.clone())
                        })
                        .ok_or(AgentRuntimePortError::IdentityMismatch)?;
                    self.permissions.remove(&request_id);
                    abandoned.push(request_id);
                }
                if !self.permissions.is_empty() {
                    return Err(AgentRuntimePortError::IdentityMismatch);
                }
                self.tools.release_turn(&session_id, &turn_id);
                self.state = PortState::SessionOpen;
                let abandoned_event =
                    self.event(AgentRuntimeEvent::ExecutionPermissionsAbandoned {
                        turn_id: turn_id.clone(),
                        request_ids: abandoned,
                    });
                let cancelled = self.event(AgentRuntimeEvent::Completed {
                    turn_id,
                    outcome: TurnOutcome::Cancelled,
                });
                self.events.push_back(cancelled);
                Ok(Some(abandoned_event))
            }
            AcpV1Observation::PromptFinished {
                session_id,
                stop_reason,
            } => {
                self.require_session(&session_id)?;
                let turn_id = self
                    .active_turn
                    .take()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let outcome = match stop_reason {
                    AcpV1StopReason::EndTurn => TurnOutcome::Completed,
                    AcpV1StopReason::MaxTokens => TurnOutcome::LimitReached {
                        limit: TurnLimit::Tokens,
                    },
                    AcpV1StopReason::MaxTurnRequests => TurnOutcome::LimitReached {
                        limit: TurnLimit::Requests,
                    },
                    AcpV1StopReason::Cancelled => TurnOutcome::Cancelled,
                    AcpV1StopReason::Refusal => TurnOutcome::Refused,
                    AcpV1StopReason::Unsupported => TurnOutcome::Failed {
                        error: safe_error(
                            "acp_turn_stop_unsupported",
                            ErrorCategory::RuntimeUnavailable,
                            false,
                            "The ACP Agent returned an unsupported stop reason",
                        ),
                    },
                };
                self.permissions.clear();
                self.tools.release_turn(&session_id, &turn_id);
                self.state = PortState::SessionOpen;
                Ok(Some(
                    self.event(AgentRuntimeEvent::Completed { turn_id, outcome }),
                ))
            }
            AcpV1Observation::RequestFailed { .. } => {
                let turn_id = self
                    .active_turn
                    .take()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                self.permissions.clear();
                self.state = PortState::SessionOpen;
                Ok(Some(self.event(AgentRuntimeEvent::Completed {
                    turn_id,
                    outcome: TurnOutcome::Failed {
                        error: safe_error(
                            "acp_request_failed",
                            ErrorCategory::RuntimeUnavailable,
                            false,
                            "The ACP Agent request failed",
                        ),
                    },
                })))
            }
            AcpV1Observation::TransportClosed => Err(AgentRuntimePortError::Transport),
            AcpV1Observation::Initialized { .. }
            | AcpV1Observation::UnsupportedClientRequest { .. }
            | AcpV1Observation::UnsupportedNotification { .. } => Ok(None),
            AcpV1Observation::SessionOpened { .. } => Err(AgentRuntimePortError::Protocol),
        }
    }

    fn map_update(
        &mut self,
        update: &serde_json::Value,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        match update
            .get("sessionUpdate")
            .and_then(serde_json::Value::as_str)
        {
            Some("agent_message_chunk") => {
                let text = update
                    .get("content")
                    .and_then(|v| v.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or(AgentRuntimePortError::Protocol)?;
                if text.is_empty() {
                    return Ok(None);
                }
                let external = update
                    .get("messageId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let message_id = self.messages.entry(external).or_default().clone();
                let text = BoundedText::new(text).map_err(|_| AgentRuntimePortError::Protocol)?;
                Ok(Some(self.event(AgentRuntimeEvent::MessageChunk {
                    message_id,
                    content: ContentPart::Text { text },
                })))
            }
            Some("tool_call" | "tool_call_update") => {
                if !self.permissions.is_empty() {
                    return Err(AgentRuntimePortError::Protocol);
                }
                let session_id = self
                    .provider_session
                    .clone()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let turn_id = self
                    .active_turn
                    .clone()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                match self
                    .tools
                    .observe(&session_id, &turn_id, update)
                    .map_err(|_| AgentRuntimePortError::Protocol)?
                {
                    AcpToolAccumulation::Updated(snapshot) => {
                        Ok(Some(self.event(AgentRuntimeEvent::ToolInvocationUpdated {
                            snapshot: snapshot.projection,
                        })))
                    }
                    AcpToolAccumulation::Buffered { .. }
                    | AcpToolAccumulation::Unchanged { .. }
                    | AcpToolAccumulation::NotToolCall => Ok(None),
                }
            }
            Some(_) => Ok(None),
            None => Err(AgentRuntimePortError::Protocol),
        }
    }

    fn bind_session(&mut self, session_id: String) -> Result<(), AgentRuntimePortError> {
        if self.provider_session.is_some() {
            return Err(AgentRuntimePortError::Protocol);
        }
        let session_digest = Sha256::digest(session_id.as_bytes());
        let external_session = ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: self.config.identity.adapter_authority.clone(),
            scope_digest: self.config.identity.connection_scope_digest.clone(),
            value: BoundedOpaque::new(format!("sha256:{session_digest:x}"))
                .map_err(|_| AgentRuntimePortError::Protocol)?,
        };
        let binding = RuntimeBindingRef {
            binding_id: self.config.identity.binding_id.clone(),
            task_id: self.config.identity.task_id.clone(),
            run_id: self.config.identity.run_id.clone(),
            agent_session_id: self.config.identity.agent_session_id.clone(),
            runtime_instance_id: self.config.identity.runtime_instance_id.clone(),
            runtime_generation: self.config.identity.runtime_generation,
            external_session,
        };
        self.provider_session = Some(session_id);
        self.binding = Some(binding.clone());
        let event = self.event(AgentRuntimeEvent::SessionOpened { binding });
        self.events.push_back(event);
        Ok(())
    }

    fn event(&mut self, event: AgentRuntimeEvent) -> RuntimeEventEnvelope {
        self.sequence = self.sequence.saturating_add(1);
        let mut correlation = Correlation::new(self.config.identity.installation_id.clone());
        correlation.actor_id = Some(self.config.identity.actor.actor_id.clone());
        correlation.task_id = Some(self.config.identity.task_id.clone());
        correlation.run_id = Some(self.config.identity.run_id.clone());
        correlation.agent_session_id = Some(self.config.identity.agent_session_id.clone());
        correlation.runtime_binding_id = Some(self.config.identity.binding_id.clone());
        RuntimeEventEnvelope {
            header: ContractHeader::new(
                ContractSchema::RuntimeEvent,
                MessageId::new(),
                now_ms(),
                correlation,
            ),
            binding_id: self.config.identity.binding_id.clone(),
            sequence: self.sequence,
            event,
        }
    }
    fn settle(&mut self, event: AgentRuntimeEvent) {
        if self.state != PortState::Terminal {
            let event = self.event(event);
            self.events.push_back(event);
            self.state = PortState::Terminal;
            self.active_turn = None;
            self.permissions.clear();
        }
    }
    fn fail_and_reap(
        &mut self,
        code: &'static str,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        self.backend.cancel().map_err(map_driver_error)?;
        self.await_terminal(deadline, "runtime settlement")?;
        self.settle(AgentRuntimeEvent::TransportFailed {
            error: safe_error(
                code,
                ErrorCategory::Transport,
                false,
                "The ACP runtime transport failed",
            ),
        });
        Ok(())
    }
    fn fail_and_shutdown(&mut self, code: &'static str) -> Result<(), AgentRuntimePortError> {
        self.backend.shutdown().map_err(map_driver_error)?;
        self.settle(AgentRuntimeEvent::TransportFailed {
            error: safe_error(
                code,
                ErrorCategory::Transport,
                false,
                "The ACP runtime transport failed",
            ),
        });
        Ok(())
    }
    fn await_terminal(
        &self,
        deadline: Instant,
        operation: &'static str,
    ) -> Result<(), AgentRuntimePortError> {
        loop {
            self.require_time(deadline, operation)?;
            match self.backend.receive_timeout(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(EVENT_POLL_INTERVAL),
            ) {
                Ok(AcpSessionEvent::Terminal(_)) => return Ok(()),
                Ok(AcpSessionEvent::Observation(_)) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(AgentRuntimePortError::Transport);
                }
            }
        }
    }
    fn deliver(
        &mut self,
        event: RuntimeEventEnvelope,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        if matches!(event.event, AgentRuntimeEvent::SessionOpened { .. })
            && self.state == PortState::SessionOpenedPending
        {
            self.state = PortState::SessionOpen;
        }
        if matches!(event.event, AgentRuntimeEvent::TransportFailed { .. }) {
            if self.terminal_delivered {
                return Err(AgentRuntimePortError::Terminal);
            }
            self.terminal_delivered = true;
        }
        Ok(event)
    }
    fn require_state(
        &self,
        expected: PortState,
        operation: &'static str,
    ) -> Result<(), AgentRuntimePortError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(AgentRuntimePortError::InvalidState {
                operation,
                state: self.state.name(),
            })
        }
    }
    fn require_run(&self, task: &TaskId, run: &RunId) -> Result<(), AgentRuntimePortError> {
        if task == &self.config.identity.task_id && run == &self.config.identity.run_id {
            Ok(())
        } else {
            Err(AgentRuntimePortError::IdentityMismatch)
        }
    }
    fn require_session(&self, session: &str) -> Result<(), AgentRuntimePortError> {
        if self.provider_session.as_deref() == Some(session) {
            Ok(())
        } else {
            Err(AgentRuntimePortError::IdentityMismatch)
        }
    }
    fn require_time(
        &self,
        deadline: Instant,
        operation: &'static str,
    ) -> Result<(), AgentRuntimePortError> {
        if Instant::now() < deadline {
            Ok(())
        } else {
            Err(AgentRuntimePortError::Deadline { operation })
        }
    }
}
