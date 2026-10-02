impl CoshCoreBridge {
    fn resolve_core_permission(
        &mut self,
        request_id: RequestId,
        decision: RuntimePermissionDecision,
    ) -> Result<(), AgentRuntimePortError> {
        if self.config.execution_profile
            != CoshCoreExecutionProfile::GatewayBrokeredWorkspaceWriteV1
        {
            return Err(AgentRuntimePortError::Unsupported {
                operation: "resolve_permission",
            });
        }
        self.require_state(BridgeState::PromptActive, "resolve_permission")?;
        let pending = self
            .pending_permission
            .as_ref()
            .ok_or(AgentRuntimePortError::IdentityMismatch)?;
        if pending.request_id != request_id
            || pending.callback.private_request_id_digest
                != sha256_digest(pending.private_request_id.as_bytes())
        {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        let frame = match decision {
            RuntimePermissionDecision::RuntimeNativeAllowOnce => self
                .codec
                .core_permission_allow_frame(&pending.private_request_id),
            RuntimePermissionDecision::ProviderNativeAllowOnce => {
                return Err(AgentRuntimePortError::Unsupported {
                    operation: "provider-native permission decision",
                });
            }
            RuntimePermissionDecision::Deny { safe_message, .. } => self
                .codec
                .brokered_denial_frame(&pending.private_request_id, safe_message.as_str()),
        }
        .map_err(|_| AgentRuntimePortError::Protocol)?;
        if self.supervisor.write_frame(&frame).is_err() {
            self.fail_transport("core_permission_response_write_failed");
            return Err(AgentRuntimePortError::Transport);
        }
        self.pending_permission = None;
        Ok(())
    }

    fn acknowledge_brokered_request(
        &mut self,
        acknowledgement: cosh_gateway_contracts::runtime::BrokeredRequestAcknowledgement,
    ) -> Result<(), AgentRuntimePortError> {
        if self.config.execution_profile != CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1 {
            return Err(AgentRuntimePortError::Unsupported {
                operation: "brokered acknowledgement",
            });
        }
        self.require_state(BridgeState::PromptActive, "acknowledge_brokered_request")?;
        let pending = self
            .pending_brokered
            .as_ref()
            .ok_or(AgentRuntimePortError::IdentityMismatch)?;
        if pending.request_id != acknowledgement.request_id || pending.acknowledged {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        let frame = self
            .codec
            .brokered_acknowledgement_frame(&pending.private_request_id)
            .map_err(|_| AgentRuntimePortError::Protocol)?;
        if self.supervisor.write_frame(&frame).is_err() {
            self.fail_transport("core_brokered_acknowledgement_write_failed");
            return Err(AgentRuntimePortError::Transport);
        }
        self.pending_brokered
            .as_mut()
            .ok_or(AgentRuntimePortError::IdentityMismatch)?
            .acknowledged = true;
        Ok(())
    }

    fn deliver_brokered_result(
        &mut self,
        delivery: BrokeredExecutionDelivery,
    ) -> Result<(), AgentRuntimePortError> {
        if self.config.execution_profile != CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1 {
            return Err(AgentRuntimePortError::Unsupported {
                operation: "brokered result delivery",
            });
        }
        self.require_state(BridgeState::PromptActive, "deliver_brokered_result")?;
        let pending = self
            .pending_brokered
            .as_ref()
            .ok_or(AgentRuntimePortError::IdentityMismatch)?;
        if pending.request_id != delivery.request_id || !pending.acknowledged {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        validate_checkpoint_delivery(&pending.operation, &delivery)?;
        let private_request_id = pending.private_request_id.clone();
        let frame = self
            .codec
            .brokered_checkpoint_result_frame(&private_request_id, &delivery)
            .map_err(|_| AgentRuntimePortError::Protocol)?;
        if self.supervisor.write_frame(&frame).is_err() {
            self.fail_transport("core_brokered_result_write_failed");
            return Err(AgentRuntimePortError::Transport);
        }
        self.pending_brokered = None;
        Ok(())
    }

    fn resolve_input(
        &mut self,
        request_id: InputRequestId,
        run_id: RunId,
        turn_id: TurnId,
        response: RuntimeInputResponse,
    ) -> Result<(), AgentRuntimePortError> {
        self.require_state(BridgeState::PromptActive, "resolve_input")?;
        let pending = self
            .pending_input
            .as_ref()
            .ok_or(AgentRuntimePortError::IdentityMismatch)?;
        if pending.request.request_id() != &request_id
            || pending.request.run_id() != &run_id
            || pending.request.turn_id() != &turn_id
        {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        let answer = match response {
            RuntimeInputResponse::Text { text } => {
                if !pending.request.allows_free_text() {
                    return Err(AgentRuntimePortError::IdentityMismatch);
                }
                text.as_str().to_owned()
            }
            RuntimeInputResponse::Options { selections } => {
                if (!pending.request.allows_multiple() && selections.as_slice().len() != 1)
                    || selections
                        .as_slice()
                        .iter()
                        .any(|index| usize::from(*index) >= pending.request.options().len())
                {
                    return Err(AgentRuntimePortError::IdentityMismatch);
                }
                selections
                    .as_slice()
                    .iter()
                    .map(|index| {
                        pending.request.options()[usize::from(*index)]
                            .label()
                            .as_str()
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        let frame = self
            .codec
            .brokered_input_response_frame(&pending.private_request_id, &answer)
            .map_err(|_| AgentRuntimePortError::Protocol)?;
        if self.supervisor.write_frame(&frame).is_err() {
            self.fail_transport("core_input_response_write_failed");
            return Err(AgentRuntimePortError::Transport);
        }
        self.pending_input = None;
        Ok(())
    }

    fn map_stream(
        &mut self,
        event: CoshCoreStreamEvent,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        match event {
            CoshCoreStreamEvent::MessageStart => {
                if self.current_message.is_some() {
                    return Err(AgentRuntimePortError::Protocol);
                }
                self.current_message = Some(RuntimeMessageId::new());
                Ok(None)
            }
            CoshCoreStreamEvent::ContentBlockStart {
                content_block: CoshCoreContentBlockInfo::ToolUse { id, name },
                ..
            } => {
                if self.current_message.is_none() {
                    return Err(AgentRuntimePortError::Protocol);
                }
                let name = BoundedName::new(name).map_err(|_| AgentRuntimePortError::Protocol)?;
                if self.tool_ids.len() >= MAX_TOOL_USES_PER_TURN {
                    return Err(AgentRuntimePortError::Protocol);
                }
                let tool_use_id = match self.tool_ids.entry(id) {
                    Entry::Vacant(entry) => {
                        let observed = entry.insert(ObservedToolUse {
                            tool_use_id: ToolUseId::new(),
                            name: name.clone(),
                        });
                        observed.tool_use_id.clone()
                    }
                    Entry::Occupied(_) => return Err(AgentRuntimePortError::Protocol),
                };
                let turn_id = self
                    .active_turn
                    .clone()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let authority = if self.config.execution_profile
                    == CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1
                    && name.as_str() == "workspace_checkpoint_create"
                {
                    ExecutionAuthority::CoshBrokered
                } else {
                    ExecutionAuthority::ProviderNativeObserved
                };
                Ok(Some(
                    self.event(AgentRuntimeEvent::ToolInvocationUpdated {
                        snapshot: ToolInvocationSnapshot {
                            turn_id,
                            tool_use_id,
                            revision: 1,
                            summary: ToolSummary {
                                name,
                                summary: BoundedText::new("Agent runtime declared a tool call")
                                    .map_err(|_| AgentRuntimePortError::Protocol)?,
                            },
                            status: ToolInvocationStatus::Pending,
                            authority,
                        },
                    }),
                ))
            }
            CoshCoreStreamEvent::ContentBlockDelta {
                delta: CoshCoreContentDelta::TextDelta { text },
                ..
            } => {
                if text.is_empty() {
                    return Ok(None);
                }
                let message_id = self
                    .current_message
                    .clone()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let text = BoundedText::new(text).map_err(|_| AgentRuntimePortError::Protocol)?;
                Ok(Some(self.event(AgentRuntimeEvent::MessageChunk {
                    message_id,
                    content: ContentPart::Text { text },
                })))
            }
            CoshCoreStreamEvent::MessageStop => {
                if self.current_message.is_none() {
                    return Err(AgentRuntimePortError::Protocol);
                }
                self.current_message = None;
                Ok(None)
            }
            CoshCoreStreamEvent::ContentBlockStart { .. }
            | CoshCoreStreamEvent::ContentBlockDelta { .. }
            | CoshCoreStreamEvent::ContentBlockStop { .. } => {
                if self.current_message.is_none() {
                    Err(AgentRuntimePortError::Protocol)
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn bind_provider_session(
        &mut self,
        provider_session_id: String,
    ) -> Result<(), AgentRuntimePortError> {
        if self.provider_session_id.is_some() {
            return Err(AgentRuntimePortError::Protocol);
        }
        let external_session = ExternalRef {
            kind: ExternalRefKind::ProviderSession,
            authority: self.config.identity.provider_authority.clone(),
            scope_digest: self.config.identity.provider_scope_digest.clone(),
            value: BoundedOpaque::new(provider_session_id.clone())
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
        self.provider_session_id = Some(provider_session_id);
        self.binding = Some(binding.clone());
        let event = self.event(AgentRuntimeEvent::SessionOpened { binding });
        self.pending_events.push_back(event);
        Ok(())
    }

    fn require_provider_session(
        &self,
        provider_session_id: &str,
    ) -> Result<(), AgentRuntimePortError> {
        if self.provider_session_id.as_deref() == Some(provider_session_id) {
            Ok(())
        } else {
            Err(AgentRuntimePortError::IdentityMismatch)
        }
    }

    fn event(&mut self, event: AgentRuntimeEvent) -> RuntimeEventEnvelope {
        self.sequence = self.sequence.saturating_add(1);
        let mut correlation = Correlation::new(self.config.identity.installation_id.clone());
        correlation.actor_id = self.config.identity.actor_id.clone();
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
        if self.state == BridgeState::Terminal {
            return;
        }
        let event = self.event(event);
        self.pending_events.push_back(event);
        self.state = BridgeState::Terminal;
        self.active_turn = None;
        self.prompt_deadline = None;
    }

    fn fail_transport(&mut self, code: &'static str) {
        self.settle(AgentRuntimeEvent::TransportFailed {
            error: safe_error(
                code,
                ErrorCategory::Transport,
                false,
                "The Agent runtime transport failed",
            ),
        });
        self.shutdown_process();
    }

    fn shutdown_process(&mut self) {
        if matches!(
            self.supervisor.state(),
            RuntimeState::Initializing | RuntimeState::Ready | RuntimeState::Stopping
        ) && self
            .supervisor
            .shutdown(self.config.shutdown_grace)
            .is_err()
        {
            // Dropping the old supervisor synchronously kills and reaps its
            // direct child before a terminal event can be delivered.
            drop(std::mem::take(&mut self.supervisor));
        }
    }

    fn deliver(
        &mut self,
        event: RuntimeEventEnvelope,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        if matches!(event.event, AgentRuntimeEvent::SessionOpened { .. })
            && self.state == BridgeState::SessionOpenedPending
        {
            self.state = BridgeState::SessionOpen;
        }
        if matches!(
            event.event,
            AgentRuntimeEvent::Completed { .. } | AgentRuntimeEvent::TransportFailed { .. }
        ) {
            if self.terminal_delivered {
                return Err(AgentRuntimePortError::Terminal);
            }
            self.terminal_delivered = true;
        }
        Ok(event)
    }

    fn require_state(
        &self,
        expected: BridgeState,
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

    fn require_run(&self, task_id: &TaskId, run_id: &RunId) -> Result<(), AgentRuntimePortError> {
        if task_id == &self.config.identity.task_id && run_id == &self.config.identity.run_id {
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

fn validate_checkpoint_delivery(
    operation: &BrokeredOperation,
    delivery: &BrokeredExecutionDelivery,
) -> Result<(), AgentRuntimePortError> {
    let BrokeredOperation::WorkspaceCheckpointCreateV1(operation) = operation;
    if let BrokeredExecutionOutcome::Succeeded { result, .. } = &delivery.outcome {
        let BrokeredOperationResult::WorkspaceCheckpointCreateV1(result) = result;
        if result.checkpoint_id != operation.checkpoint_id {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
    }
    Ok(())
}
