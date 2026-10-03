/// Event emitted by a provider, Core, or ACP Runtime bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRuntimeEvent {
    /// A provider or ACP session was opened and fenced.
    SessionOpened {
        /// New active binding.
        binding: RuntimeBindingRef,
    },
    /// One explicitly identified prompt turn was accepted by the Runtime.
    TurnStarted {
        /// Turn that became active.
        turn_id: TurnId,
    },
    /// A bounded streaming content part was observed.
    MessageChunk {
        /// Runtime message receiving the chunk.
        message_id: RuntimeMessageId,
        /// Neutral content part.
        content: ContentPart,
    },
    /// Runtime reported a tool call without authorizing a side effect.
    ToolCallObserved {
        /// COSH-owned tool observation identity.
        tool_use_id: ToolUseId,
        /// Redacted tool summary.
        summary: ToolSummary,
    },
    /// A tool invocation was created or advanced within an explicit turn.
    ToolInvocationUpdated {
        /// Stable latest projection of the invocation.
        snapshot: ToolInvocationSnapshot,
    },
    /// Runtime requested permission for a capability.
    PermissionRequested {
        /// Neutral capability request evaluated by the broker.
        request: CapabilityRequest,
    },
    /// Runtime requested permission for a provider-owned side effect.
    ///
    /// This variant is always observation-only. Brokered effects use
    /// `BrokeredExecutionRequested`, so no authority selector is accepted.
    ExecutionPermissionRequested {
        /// Turn owning the tool invocation.
        turn_id: TurnId,
        /// Stable tool identity when a prior update established one.
        tool_use_id: Option<ToolUseId>,
        /// Agent-provided, bounded presentation that carries no authority.
        summary: ToolSummary,
        /// Neutral capability request evaluated by the broker.
        request: CapabilityRequest,
        /// Complete digested fence for the live provider callback.
        callback: ProviderPermissionCallbackV2,
    },
    /// Private cosh-core requested permission for a Runtime-owned local effect.
    ///
    /// This event does not claim that Core emitted an ACP callback. The
    /// dedicated callback fence binds the private control request instead.
    CoreExecutionPermissionRequested {
        /// Turn owning the tool invocation.
        turn_id: TurnId,
        /// Stable tool identity established by a prior update.
        tool_use_id: ToolUseId,
        /// Agent-provided, bounded presentation that carries no authority.
        summary: ToolSummary,
        /// Neutral capability request evaluated by the broker.
        request: CapabilityRequest,
        /// Complete digested fence for the live private Core callback.
        callback: CorePermissionCallbackV1,
    },
    /// A cancelled provider turn abandoned callbacks without decisions.
    ///
    /// Request identities are COSH-owned; raw provider correlation values do
    /// not cross the Runtime boundary.
    ExecutionPermissionsAbandoned {
        /// Turn that was cancelled while callbacks were pending.
        turn_id: TurnId,
        /// Pending COSH permission requests abandoned by the provider.
        request_ids: Vec<RequestId>,
    },
    /// Runtime requested a typed operation whose effect is owned by COSH.
    BrokeredExecutionRequested {
        /// Turn owning the brokered invocation.
        turn_id: TurnId,
        /// Stable tool identity when a prior update established one.
        tool_use_id: Option<ToolUseId>,
        /// Agent-provided, bounded presentation that carries no authority.
        summary: ToolSummary,
        /// Neutral capability request evaluated by the broker.
        request: CapabilityRequest,
        /// Closed typed operation executed only by a COSH target.
        operation: BrokeredOperation,
    },
    /// Runtime asked Gateway to durably coordinate bounded user input.
    InputRequested {
        /// Exact request, Run, Turn, presentation, and response constraints.
        request: RuntimeInputRequest,
    },
    /// Runtime reported cumulative token usage.
    UsageUpdated {
        /// Current cumulative usage.
        usage: RuntimeUsage,
    },
    /// Runtime turn reached a terminal outcome.
    Completed {
        /// Turn that reached a terminal result.
        turn_id: TurnId,
        /// Turn result that must not directly settle the owning Task.
        outcome: TurnOutcome,
    },
    /// Runtime transport failed before a domain result was known.
    TransportFailed {
        /// Safe bounded transport error.
        error: ContractError,
    },
}
