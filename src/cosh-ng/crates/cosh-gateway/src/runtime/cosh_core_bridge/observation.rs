impl CoshCoreBridge {
    fn read_next_event(
        &mut self,
        deadline: Instant,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        if let Some(event) = self.pending_events.pop_front() {
            return self.deliver(event);
        }
        if self.state == BridgeState::Terminal {
            return Err(AgentRuntimePortError::Terminal);
        }
        if self.state != BridgeState::PromptActive {
            return Err(AgentRuntimePortError::InvalidState {
                operation: "next_event",
                state: self.state.name(),
            });
        }

        loop {
            let turn_deadline = self.prompt_deadline.unwrap_or(deadline).min(deadline);
            if Instant::now() >= turn_deadline {
                self.fail_transport("core_prompt_deadline");
                return self
                    .pending_events
                    .pop_front()
                    .ok_or(AgentRuntimePortError::Terminal)
                    .and_then(|event| self.deliver(event));
            }
            let observation = match self.read_observation(turn_deadline, "next_event") {
                Ok(observation) => observation,
                Err(AgentRuntimePortError::Deadline { .. })
                    if deadline < self.prompt_deadline.unwrap_or(deadline) =>
                {
                    return Err(AgentRuntimePortError::Deadline {
                        operation: "next_event",
                    });
                }
                Err(_) => {
                    self.fail_transport("core_transport_failed");
                    return self
                        .pending_events
                        .pop_front()
                        .ok_or(AgentRuntimePortError::Terminal)
                        .and_then(|event| self.deliver(event));
                }
            };
            match self.map_observation(observation) {
                Ok(Some(event)) => return self.deliver(event),
                Ok(None) => {}
                Err(_) => {
                    self.fail_transport("core_protocol_failed");
                    return self
                        .pending_events
                        .pop_front()
                        .ok_or(AgentRuntimePortError::Terminal)
                        .and_then(|event| self.deliver(event));
                }
            }
        }
    }

    fn read_observation(
        &mut self,
        deadline: Instant,
        operation: &'static str,
    ) -> Result<CoshCoreObservation, AgentRuntimePortError> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AgentRuntimePortError::Deadline { operation });
            }
            match self
                .supervisor
                .read_frame_timeout(remaining.min(READ_POLL_INTERVAL))
                .map_err(|_| AgentRuntimePortError::Transport)?
            {
                RuntimeFrameRead::Frame(frame) => {
                    return self
                        .codec
                        .decode_frame(frame.as_bytes())
                        .map_err(|_| AgentRuntimePortError::Protocol);
                }
                RuntimeFrameRead::Eof => {
                    return Ok(self
                        .codec
                        .finish_stdout()
                        .unwrap_or(CoshCoreObservation::ProtocolEndedWithoutResult));
                }
                RuntimeFrameRead::TimedOut => {
                    if self
                        .supervisor
                        .poll_terminal()
                        .map_err(|_| AgentRuntimePortError::Transport)?
                        .is_some()
                    {
                        return Ok(CoshCoreObservation::ProtocolEndedWithoutResult);
                    }
                }
            }
        }
    }

    fn map_observation(
        &mut self,
        observation: CoshCoreObservation,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        match observation {
            CoshCoreObservation::Stream(event) => self.map_stream(event),
            CoshCoreObservation::System(message) => {
                if let Some(provider_session_id) = message.provider_session_id {
                    self.require_provider_session(&provider_session_id)?;
                }
                Ok(None)
            }
            CoshCoreObservation::Assistant(message) => {
                self.require_provider_session(&message.provider_session_id)?;
                Ok(None)
            }
            CoshCoreObservation::ToolResults {
                provider_session_id,
                ..
            } => {
                self.require_provider_session(&provider_session_id)?;
                Ok(None)
            }
            CoshCoreObservation::Result(result) => {
                if self.current_message.is_some()
                    || self.pending_input.is_some()
                    || self.pending_permission.is_some()
                    || self.pending_brokered.is_some()
                {
                    return Err(AgentRuntimePortError::Protocol);
                }
                if let Some(provider_session_id) = result.provider_session_id.as_deref() {
                    self.require_provider_session(provider_session_id)?;
                }
                let turn_id = self
                    .active_turn
                    .take()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let event = if result.is_error {
                    AgentRuntimeEvent::Completed {
                        turn_id,
                        outcome: TurnOutcome::Failed {
                            error: safe_error(
                                "core_turn_failed",
                                ErrorCategory::RuntimeUnavailable,
                                false,
                                "The Agent runtime reported a failed turn",
                            ),
                        },
                    }
                } else {
                    AgentRuntimeEvent::Completed {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                    }
                };
                self.settle(event);
                self.shutdown_process();
                Ok(self.pending_events.pop_front())
            }
            CoshCoreObservation::ProtocolEndedWithoutResult => {
                Err(AgentRuntimePortError::Transport)
            }
            CoshCoreObservation::ControlRequest(request) => self.map_control_request(request),
            CoshCoreObservation::ControlResponse(_)
            | CoshCoreObservation::RegistryResponse { .. }
            | CoshCoreObservation::Initialized(_) => Err(AgentRuntimePortError::Protocol),
        }
    }

    fn map_control_request(
        &mut self,
        envelope: super::CoshCoreControlRequestEnvelope,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        if self.config.execution_profile != CoshCoreExecutionProfile::GatewayBrokeredV1
            && self.config.execution_profile
                != CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1
            && self.config.execution_profile
                != CoshCoreExecutionProfile::GatewayBrokeredWorkspaceWriteV1
        {
            return Err(AgentRuntimePortError::Protocol);
        }
        if self.state != BridgeState::PromptActive
            || self.pending_input.is_some()
            || self.pending_permission.is_some()
            || self.pending_brokered.is_some()
        {
            return Err(AgentRuntimePortError::Protocol);
        }
        let private_request_id = envelope.request_id;
        match envelope.request {
            CoshCoreControlRequest::AskUser {
                tool_use_id,
                question,
                options,
                allow_free_text,
                multi_select,
            } => {
                let private_tool_use_id = tool_use_id
                    .as_ref()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let observed_tool = self
                    .tool_ids
                    .get(private_tool_use_id)
                    .ok_or(AgentRuntimePortError::Protocol)?;
                if observed_tool.name.as_str() != "ask_user_question" {
                    return Err(AgentRuntimePortError::Protocol);
                }
                let stable_tool_id = observed_tool.tool_use_id.clone();
                let turn_id = self
                    .active_turn
                    .clone()
                    .ok_or(AgentRuntimePortError::Protocol)?;
                let options = options
                    .into_iter()
                    .map(|option| {
                        Ok(RuntimeInputOption::new(
                            BoundedText::new(option.label)
                                .map_err(|_| AgentRuntimePortError::Protocol)?,
                            option
                                .description
                                .map(BoundedText::new)
                                .transpose()
                                .map_err(|_| AgentRuntimePortError::Protocol)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, AgentRuntimePortError>>()?;
                let request = RuntimeInputRequest::new(
                    InputRequestId::new(),
                    self.config.identity.run_id.clone(),
                    turn_id,
                    Some(stable_tool_id),
                    BoundedText::new(question).map_err(|_| AgentRuntimePortError::Protocol)?,
                    options,
                    allow_free_text,
                    multi_select,
                )
                .map_err(|_| AgentRuntimePortError::Protocol)?;
                self.pending_input = Some(PendingInputRequest {
                    private_request_id,
                    request: request.clone(),
                });
                return Ok(Some(
                    self.event(AgentRuntimeEvent::InputRequested { request }),
                ));
            }
            CoshCoreControlRequest::CanUseTool {
                tool_name,
                input,
                description,
                tool_use_id,
                audit_ref,
                hook_requires_approval,
            } if self.config.execution_profile
                == CoshCoreExecutionProfile::GatewayBrokeredCheckpointV1 =>
            {
                return self.map_checkpoint_request(
                    private_request_id,
                    tool_name,
                    input,
                    description,
                    tool_use_id,
                    audit_ref,
                    hook_requires_approval,
                );
            }
            CoshCoreControlRequest::CanUseTool {
                tool_name,
                input,
                description,
                tool_use_id,
                audit_ref,
                hook_requires_approval,
            } if self.config.execution_profile
                == CoshCoreExecutionProfile::GatewayBrokeredWorkspaceWriteV1 =>
            {
                return self.map_workspace_write_request(
                    private_request_id,
                    tool_name,
                    input,
                    description,
                    tool_use_id,
                    audit_ref,
                    hook_requires_approval,
                );
            }
            _ => {}
        }
        // The task-only Core inventory contains no hosted side-effect tool.
        // Reject every `can_use_tool` request before a CapabilityRequest,
        // Approval, Permit, or ExecutionTarget can be created.
        let _ = private_request_id;
        Err(AgentRuntimePortError::Unsupported {
            operation: "task-only core tool request",
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn map_workspace_write_request(
        &mut self,
        private_request_id: String,
        tool_name: String,
        input: serde_json::Value,
        description: Option<String>,
        private_tool_use_id: String,
        audit_ref: Option<String>,
        hook_requires_approval: bool,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        let input_object = input.as_object().ok_or(AgentRuntimePortError::Protocol)?;
        if tool_name != "write_file"
            || input_object
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_none()
            || input_object
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_none()
            || hook_requires_approval
        {
            return Err(AgentRuntimePortError::Unsupported {
                operation: "workspace-write core tool request",
            });
        }
        let observed_tool = self
            .tool_ids
            .get(&private_tool_use_id)
            .ok_or(AgentRuntimePortError::Protocol)?;
        if observed_tool.name.as_str() != "write_file" {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        let tool_use_id = observed_tool.tool_use_id.clone();
        let turn_id = self
            .active_turn
            .clone()
            .ok_or(AgentRuntimePortError::Protocol)?;
        let context = self
            .config
            .brokered_context
            .as_ref()
            .ok_or(AgentRuntimePortError::Protocol)?;
        let request_id = RequestId::new();
        let input_digest = digest_json(&input)?;
        let operation = OperationDescriptor {
            namespace: BoundedName::new("workspace")
                .map_err(|_| AgentRuntimePortError::Protocol)?,
            name: BoundedName::new("write_file").map_err(|_| AgentRuntimePortError::Protocol)?,
            arguments_digest: input_digest.clone(),
        };
        let operation_digest = digest_json(&(
            "cosh.core-private.workspace-write.v1",
            &context.target,
            &operation,
        ))?;
        let callback_payload_digest = digest_json(&(
            &private_request_id,
            &tool_name,
            &input,
            &description,
            &private_tool_use_id,
            &audit_ref,
            hook_requires_approval,
        ))?;
        let callback = CorePermissionCallbackV1 {
            private_request_id_digest: sha256_digest(private_request_id.as_bytes()),
            private_tool_use_id_digest: sha256_digest(private_tool_use_id.as_bytes()),
            callback_payload_digest,
            normalized_operation_digest: operation_digest.clone(),
        };
        let now = now_ms();
        let remaining = self
            .prompt_deadline
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            .ok_or(AgentRuntimePortError::Deadline {
                operation: "workspace_write_request",
            })?;
        let lifetime_ms =
            u64::try_from(remaining.as_millis()).map_err(|_| AgentRuntimePortError::Protocol)?;
        let expires_at_ms = now
            .checked_add(lifetime_ms)
            .filter(|expires| *expires > now)
            .ok_or(AgentRuntimePortError::Deadline {
                operation: "workspace_write_request",
            })?;
        let request = CapabilityRequest {
            request_id: request_id.clone(),
            task_id: self.config.identity.task_id.clone(),
            run_id: self.config.identity.run_id.clone(),
            actor: context.actor.clone(),
            target: context.target.clone(),
            operation,
            operation_digest,
            requested_scope: CapabilityScope {
                resource: BoundedName::new("workspace")
                    .map_err(|_| AgentRuntimePortError::Protocol)?,
                access: BoundedName::new("write").map_err(|_| AgentRuntimePortError::Protocol)?,
            },
            input_digest,
            expires_at_ms,
        };
        self.pending_permission = Some(PendingCorePermission {
            private_request_id,
            request_id,
            callback: callback.clone(),
        });
        Ok(Some(
            self.event(AgentRuntimeEvent::CoreExecutionPermissionRequested {
                turn_id,
                tool_use_id,
                summary: ToolSummary {
                    name: BoundedName::new("write_file")
                        .map_err(|_| AgentRuntimePortError::Protocol)?,
                    summary: BoundedText::new("Write one file in the bound workspace")
                        .map_err(|_| AgentRuntimePortError::Protocol)?,
                },
                request,
                callback,
            }),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn map_checkpoint_request(
        &mut self,
        private_request_id: String,
        tool_name: String,
        input: serde_json::Value,
        description: Option<String>,
        private_tool_use_id: String,
        audit_ref: Option<String>,
        hook_requires_approval: bool,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        if tool_name != "workspace_checkpoint_create"
            || input.as_object().is_none_or(|object| !object.is_empty())
        {
            return Err(AgentRuntimePortError::Unsupported {
                operation: "checkpoint core tool request",
            });
        }
        let observed_tool = self
            .tool_ids
            .get(&private_tool_use_id)
            .ok_or(AgentRuntimePortError::Protocol)?;
        if observed_tool.name.as_str() != "workspace_checkpoint_create" {
            return Err(AgentRuntimePortError::Protocol);
        }
        let tool_use_id = observed_tool.tool_use_id.clone();
        let turn_id = self
            .active_turn
            .clone()
            .ok_or(AgentRuntimePortError::Protocol)?;
        let context = self
            .config
            .brokered_context
            .as_ref()
            .ok_or(AgentRuntimePortError::Protocol)?;
        let request_id = RequestId::new();
        let operation_input = WorkspaceCheckpointCreateV1 {
            checkpoint_id: CheckpointId::new(),
        };
        let arguments_digest = digest_json(&operation_input)?;
        let operation = BrokeredOperation::WorkspaceCheckpointCreateV1(operation_input);
        let operation_descriptor = OperationDescriptor {
            namespace: BoundedName::new("workspace")
                .map_err(|_| AgentRuntimePortError::Protocol)?,
            name: BoundedName::new("checkpoint_create")
                .map_err(|_| AgentRuntimePortError::Protocol)?,
            arguments_digest,
        };
        let operation_digest = digest_json(&(&operation_descriptor, &operation))?;
        let input_digest = digest_json(&(
            &tool_name,
            &input,
            &description,
            &private_tool_use_id,
            &audit_ref,
            hook_requires_approval,
        ))?;
        let now = now_ms();
        let remaining = self
            .prompt_deadline
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            .ok_or(AgentRuntimePortError::Deadline {
                operation: "checkpoint_request",
            })?;
        let lifetime_ms =
            u64::try_from(remaining.as_millis()).map_err(|_| AgentRuntimePortError::Protocol)?;
        let expires_at_ms = now
            .checked_add(lifetime_ms)
            .filter(|expires| *expires > now)
            .ok_or(AgentRuntimePortError::Deadline {
                operation: "checkpoint_request",
            })?;
        let request = CapabilityRequest {
            request_id: request_id.clone(),
            task_id: self.config.identity.task_id.clone(),
            run_id: self.config.identity.run_id.clone(),
            actor: context.actor.clone(),
            target: context.target.clone(),
            operation: operation_descriptor,
            operation_digest,
            requested_scope: CapabilityScope {
                resource: BoundedName::new("workspace")
                    .map_err(|_| AgentRuntimePortError::Protocol)?,
                access: BoundedName::new("checkpoint_create")
                    .map_err(|_| AgentRuntimePortError::Protocol)?,
            },
            input_digest,
            expires_at_ms,
        };
        self.pending_brokered = Some(PendingBrokeredRequest {
            private_request_id,
            request_id,
            operation: operation.clone(),
            acknowledged: false,
        });
        Ok(Some(
            self.event(AgentRuntimeEvent::BrokeredExecutionRequested {
                turn_id,
                tool_use_id: Some(tool_use_id),
                summary: ToolSummary {
                    name: BoundedName::new("workspace_checkpoint_create")
                        .map_err(|_| AgentRuntimePortError::Protocol)?,
                    summary: BoundedText::new("Create a governed workspace checkpoint")
                        .map_err(|_| AgentRuntimePortError::Protocol)?,
                },
                request,
                operation,
            }),
        ))
    }
}
