
/// Provider-neutral execution status for one observed tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolInvocationStatus {
    /// Input is still being prepared or permission is pending.
    Pending,
    /// The provider reports that execution is in progress.
    InProgress,
    /// The provider reports successful completion.
    Completed,
    /// The provider reports failed completion.
    Failed,
}

impl ToolInvocationStatus {
    /// Returns whether no later state mutation is valid for this invocation.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Stable COSH projection of one ACP or provider tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationSnapshot {
    /// Prompt turn that owns the invocation.
    pub turn_id: TurnId,
    /// Stable COSH identity retained across provider updates.
    pub tool_use_id: ToolUseId,
    /// Monotonic revision within this invocation.
    pub revision: u64,
    /// Redacted provider-neutral presentation.
    pub summary: ToolSummary,
    /// Latest provider-reported execution status.
    pub status: ToolInvocationStatus,
    /// Boundary at which a side effect is enforced.
    pub authority: ExecutionAuthority,
}

/// Limit that ended an Agent turn before normal completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnLimit {
    /// The provider reached its token budget.
    Tokens,
    /// The provider reached its request budget for the turn.
    Requests,
}

/// Terminal result of one prompt turn, independent from Task or Run settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    /// The Agent completed the turn normally.
    Completed,
    /// A configured limit stopped the turn before normal completion.
    LimitReached {
        /// Limit that stopped the turn.
        limit: TurnLimit,
    },
    /// The Agent refused to process the turn.
    Refused,
    /// The Agent acknowledged cancellation of the turn.
    Cancelled,
    /// The turn ended with a bounded provider or Runtime failure.
    Failed {
        /// Safe failure that does not expose provider payloads.
        error: ContractError,
    },
}

/// Terminal result reported by a legacy Runtime for an entire Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RunOutcome {
    /// Runtime turn completed successfully.
    Succeeded,
    /// Runtime turn completed with a bounded failure.
    Failed {
        /// Safe Runtime failure.
        error: ContractError,
    },
    /// Runtime acknowledged cancellation.
    Cancelled,
}
