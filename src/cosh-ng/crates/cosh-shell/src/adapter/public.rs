#[allow(dead_code, unused_imports)]
#[path = "mod.rs"]
mod implementation;

pub use implementation::{
    adapter_for_kind, AdapterError, AdapterInstance, AdapterKind, AgentAdapter,
    AgentBackendCapabilities, AgentRunHandle, AgentRunPoll, ApprovalDecision, ApprovalResponse,
    AuthFieldInfo, AuthProviderInfo, AuthResponse, ClaudeCodeAdapter, ControlProtocolCapabilities,
    CoshCoreAdapter, EcsProbeTask, FakeAgentAdapter, HostExecutedShellMetadata,
    HostExecutedShellResult, ProviderCancellationArtifact, ProviderCancellationArtifactKind,
    ProviderCancellationArtifactStore, QwenCliAdapter, SessionClearFailure,
    SessionClearInterruption, SessionClearPlan, SessionClearResult, SessionErrorInfo,
    SessionHealth, SessionList, SessionManagementClient, SessionRecovery, SessionRecoveryState,
    SessionRuntimeState, SessionSummary, ShellEvidenceAction,
};

#[allow(unused_imports)]
pub(crate) use implementation::{
    prompt_from_request, provider_prompt_contract, FreshSessionOutcome,
};
