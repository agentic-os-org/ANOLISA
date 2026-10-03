impl RuntimeHandle for ScheduledAgentRuntimeHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        if self.terminal || self.prompt_started {
            return Err(contract_error(
                "runtime_begin_state_invalid",
                ErrorCategory::Conflict,
                false,
                "The Runtime prompt cannot start in its current state",
            ));
        }
        if let Err(error) = self.port.dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: self.run_id.clone(),
                turn_id: self.turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: self.intent.clone(),
                }],
            },
            deadline(self.command_timeout),
        ) {
            let mapped = map_port_error(error);
            let _ = self.close();
            return Err(mapped);
        }
        self.prompt_started = true;
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        if self.terminal || !self.prompt_started {
            return RuntimePoll::Failed(contract_error(
                "runtime_already_terminal",
                ErrorCategory::Conflict,
                false,
                "The Runtime handle is already terminal",
            ));
        }
        if self
            .pending_permission
            .as_ref()
            .is_some_and(|permission| permission.core_callback.is_some())
            || self.pending_brokered.is_some()
            || self.pending_input.is_some()
        {
            return RuntimePoll::Pending;
        }
        self.poll_event()
    }

    fn shutdown(&mut self, reason: CancelReason) -> Result<(), ContractError> {
        if self.terminal {
            return Ok(());
        }
        if !self.prompt_started {
            return self.close();
        }
        let shutdown_deadline = deadline(self.command_timeout);
        self.port
            .dispatch(
                AgentRuntimeCommand::Cancel {
                    run_id: self.run_id.clone(),
                    turn_id: self.turn_id.clone(),
                    cause: reason,
                },
                shutdown_deadline,
            )
            .map_err(map_port_error)?;
        // ACP and Core cancellation dispatches already wait for process
        // settlement. Close is therefore cleanup-only and may legitimately
        // report that the port is terminal after cancellation was acknowledged.
        let _ = self.port.dispatch(
            AgentRuntimeCommand::Close {
                binding: self.binding.clone(),
            },
            shutdown_deadline,
        );
        self.terminal = true;
        Ok(())
    }

    fn resolve_provider_permission(
        &mut self,
        permission: &RuntimePermissionRef,
        decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError> {
        if self.pending_permission.as_ref() != Some(permission) {
            return Err(contract_error(
                "runtime_permission_identity_invalid",
                ErrorCategory::Conflict,
                false,
                "The provider permission response does not match the pending callback",
            ));
        }
        if permission.callback.is_some() == permission.core_callback.is_some() {
            return Err(contract_error(
                "runtime_permission_callback_invalid",
                ErrorCategory::Conflict,
                false,
                "The provider permission response does not carry exactly one callback fence",
            ));
        }
        let denied = matches!(&decision, RuntimePermissionDecision::Deny { .. });
        self.port
            .dispatch(
                AgentRuntimeCommand::ResolvePermission {
                    request_id: permission.request_id.clone(),
                    decision,
                },
                deadline(self.command_timeout),
            )
            .map_err(map_port_error)?;
        self.pending_permission = None;
        self.expected_cancellation = if denied && permission.callback.is_some() {
            Some(RuntimeCancellationCause::ProviderPermissionDenied {
                permission: permission.clone(),
            })
        } else {
            None
        };
        Ok(())
    }

    fn resolve_input(
        &mut self,
        request: &RuntimeInputRequest,
        response: RuntimeInputResponse,
    ) -> Result<(), ContractError> {
        if self.pending_input.as_ref() != Some(request.request_id())
            || request.run_id() != &self.run_id
            || request.turn_id() != &self.turn_id
        {
            return Err(contract_error(
                "runtime_input_identity_invalid",
                ErrorCategory::Conflict,
                false,
                "The input response does not match the pending Runtime request",
            ));
        }
        self.port
            .dispatch(
                AgentRuntimeCommand::ResolveInput {
                    request_id: request.request_id().clone(),
                    run_id: request.run_id().clone(),
                    turn_id: request.turn_id().clone(),
                    response,
                },
                deadline(self.command_timeout),
            )
            .map_err(map_port_error)?;
        self.pending_input = None;
        Ok(())
    }

    fn acknowledge_brokered_request(
        &mut self,
        brokered: &BrokeredExecutionRef,
        acknowledgement: BrokeredRequestAcknowledgement,
    ) -> Result<(), ContractError> {
        let pending = self.pending_brokered.as_mut().ok_or_else(|| {
            contract_error(
                "runtime_brokered_identity_invalid",
                ErrorCategory::Conflict,
                false,
                "The brokered acknowledgement does not match a pending callback",
            )
        })?;
        if pending.reference != *brokered
            || pending.reference.request_id != acknowledgement.request_id
            || pending.acknowledged
            || pending.dispatch_indeterminate
        {
            return Err(contract_error(
                "runtime_brokered_identity_invalid",
                ErrorCategory::Conflict,
                false,
                "The brokered acknowledgement does not match the pending callback",
            ));
        }
        pending.dispatch_indeterminate = true;
        let dispatch = self.port.dispatch(
            AgentRuntimeCommand::AcknowledgeBrokeredRequest { acknowledgement },
            deadline(self.command_timeout),
        );
        match dispatch {
            Ok(()) => {
                let pending = self
                    .pending_brokered
                    .as_mut()
                    .unwrap_or_else(|| unreachable!("pending callback cannot disappear"));
                pending.acknowledged = true;
                pending.dispatch_indeterminate = false;
                Ok(())
            }
            Err(error) => {
                let error = map_port_error(error);
                let _ = self.close();
                Err(error)
            }
        }
    }

    fn deliver_brokered_result(
        &mut self,
        brokered: &BrokeredExecutionRef,
        delivery: BrokeredExecutionDelivery,
    ) -> Result<(), ContractError> {
        let pending = self.pending_brokered.as_mut().ok_or_else(|| {
            contract_error(
                "runtime_brokered_identity_invalid",
                ErrorCategory::Conflict,
                false,
                "The brokered result does not match a pending callback",
            )
        })?;
        if pending.reference != *brokered
            || pending.reference.request_id != delivery.request_id
            || !pending.acknowledged
            || pending.dispatch_indeterminate
        {
            return Err(contract_error(
                "runtime_brokered_identity_invalid",
                ErrorCategory::Conflict,
                false,
                "The brokered result does not match the pending callback",
            ));
        }
        pending.dispatch_indeterminate = true;
        let dispatch = self.port.dispatch(
            AgentRuntimeCommand::DeliverBrokeredResult { delivery },
            deadline(self.command_timeout),
        );
        match dispatch {
            Ok(()) => {
                self.pending_brokered = None;
                Ok(())
            }
            Err(error) => {
                let error = map_port_error(error);
                let _ = self.close();
                Err(error)
            }
        }
    }
}
