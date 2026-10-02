impl AgentRuntimePort for CoshCoreBridge {
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
            } => self.open_session(task_id, run_id, workspace, deadline),
            AgentRuntimeCommand::Prompt {
                run_id,
                turn_id,
                input,
            } => self.prompt(run_id, turn_id, input, deadline),
            AgentRuntimeCommand::Cancel {
                run_id, turn_id, ..
            } => self.cancel(run_id, turn_id, deadline),
            AgentRuntimeCommand::Close { binding } => self.close(binding, deadline),
            AgentRuntimeCommand::ResumeSession { .. } => Err(AgentRuntimePortError::Unsupported {
                operation: "resume_session",
            }),
            AgentRuntimeCommand::ResolvePermission {
                request_id,
                decision,
            } => self.resolve_core_permission(request_id, decision),
            AgentRuntimeCommand::AcknowledgeBrokeredRequest { acknowledgement } => {
                self.acknowledge_brokered_request(acknowledgement)
            }
            AgentRuntimeCommand::DeliverBrokeredResult { delivery } => {
                self.deliver_brokered_result(delivery)
            }
            AgentRuntimeCommand::ResolveInput {
                request_id,
                run_id,
                turn_id,
                response,
            } => self.resolve_input(request_id, run_id, turn_id, response),
        }
    }

    fn next_event(
        &mut self,
        deadline: Instant,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        self.read_next_event(deadline)
    }
}

impl Drop for CoshCoreBridge {
    fn drop(&mut self) {
        self.shutdown_process();
    }
}
