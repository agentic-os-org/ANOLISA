impl AgentRuntimePort for AcpAgentRuntime {
    fn binding_id(&self) -> &RuntimeBindingId {
        &self.config.identity.binding_id
    }
    fn dispatch(
        &mut self,
        command: AgentRuntimeCommand,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        match command {
            AgentRuntimeCommand::OpenSession {
                task_id,
                run_id,
                workspace,
            } => self.open(task_id, run_id, workspace, deadline),
            AgentRuntimeCommand::Prompt {
                run_id,
                turn_id,
                input,
            } => self.prompt(run_id, turn_id, input, deadline),
            AgentRuntimeCommand::ResolvePermission {
                request_id,
                decision,
            } => self.resolve(request_id, decision, deadline),
            AgentRuntimeCommand::AcknowledgeBrokeredRequest { .. }
            | AgentRuntimeCommand::DeliverBrokeredResult { .. } => {
                Err(AgentRuntimePortError::Unsupported {
                    operation: "COSH-brokered execution over ACP",
                })
            }
            AgentRuntimeCommand::ResolveInput { .. } => Err(AgentRuntimePortError::Unsupported {
                operation: "resolve_input",
            }),
            AgentRuntimeCommand::Cancel {
                run_id, turn_id, ..
            } => self.cancel(run_id, turn_id, deadline),
            AgentRuntimeCommand::Close { binding } => {
                self.require_time(deadline, "close")?;
                if self.binding.as_ref() != Some(&binding) {
                    return Err(AgentRuntimePortError::IdentityMismatch);
                }
                if self.state != PortState::Terminal {
                    self.backend.shutdown().map_err(map_driver_error)?;
                    self.state = PortState::Terminal;
                }
                Ok(())
            }
            AgentRuntimeCommand::ResumeSession { .. } => Err(AgentRuntimePortError::Unsupported {
                operation: "resume_session",
            }),
        }
    }
    fn next_event(
        &mut self,
        deadline: Instant,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        self.next(deadline)
    }
}
