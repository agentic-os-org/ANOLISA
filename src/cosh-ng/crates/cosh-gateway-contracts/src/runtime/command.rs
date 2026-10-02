
/// Command issued through the neutral Agent Runtime port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRuntimeCommand {
    /// Open a new provider or ACP session for a Task Run.
    OpenSession {
        /// Task owning the session.
        task_id: TaskId,
        /// Run opening the session.
        run_id: RunId,
        /// Workspace scope exposed to the Runtime.
        workspace: WorkspaceRef,
    },
    /// Resume an existing fenced session binding.
    ResumeSession {
        /// Task owning the session.
        task_id: TaskId,
        /// Run resuming the session.
        run_id: RunId,
        /// Existing fenced binding.
        binding: RuntimeBindingRef,
    },
    /// Send bounded content to an active Agent turn.
    Prompt {
        /// Run receiving the input.
        run_id: RunId,
        /// COSH-owned identity for this prompt turn.
        turn_id: TurnId,
        /// Neutral content parts.
        input: Vec<ContentPart>,
    },
    /// Return a broker decision to a pending Runtime request.
    ResolvePermission {
        /// Capability request being resolved.
        request_id: RequestId,
        /// Provider-native decision translated for the Runtime.
        decision: RuntimePermissionDecision,
    },
    /// Confirms that Gateway durably took ownership of a brokered request.
    ///
    /// This acknowledgement carries neither a policy decision nor executable
    /// authority. The final outcome arrives through `DeliverBrokeredResult`.
    AcknowledgeBrokeredRequest {
        /// Durable takeover acknowledgement for the pending Runtime request.
        acknowledgement: BrokeredRequestAcknowledgement,
    },
    /// Delivers the terminal COSH-owned outcome of a brokered request.
    DeliverBrokeredResult {
        /// Typed result correlated to the original Runtime request.
        delivery: BrokeredExecutionDelivery,
    },
    /// Resolves one exact pending Runtime input request.
    ///
    /// The Task plane's durable `SubmitInput` command is the only intended
    /// source of this Runtime-local callback.
    ResolveInput {
        /// Independently allocated request being resolved.
        request_id: InputRequestId,
        /// Run that owns the request.
        run_id: RunId,
        /// Prompt turn waiting for the response.
        turn_id: TurnId,
        /// Bounded answer validated against the pending request.
        response: RuntimeInputResponse,
    },
    /// Request cancellation of an active Agent turn.
    Cancel {
        /// Run to cancel.
        run_id: RunId,
        /// Active turn to cancel.
        turn_id: TurnId,
        /// Stable cancellation cause.
        cause: CancelReason,
    },
    /// Close a Runtime session binding.
    Close {
        /// Binding to close.
        binding: RuntimeBindingRef,
    },
}
