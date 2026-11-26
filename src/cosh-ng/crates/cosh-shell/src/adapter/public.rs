#[allow(dead_code, unused_imports)]
#[path = "mod.rs"]
mod implementation;

pub use implementation::{
    adapter_for_kind, AcpAgentAdapter, AcpHostAuthority, AcpHostDecision, AcpHostExecuteOutcome,
    AcpHostExecuteRequest, AcpHostInputWait, AcpHostRefusal, AcpPermissionDecision,
    AcpPermissionPolicy, AdapterError, AdapterInstance, AdapterKind, AgentAdapter,
    AgentBackendCapabilities, AgentRunHandle, AgentRunPoll, ApprovalDecision, ApprovalResponse,
    AuthFieldInfo, AuthProviderInfo, AuthResponse, ClaudeCodeAdapter, ControlProtocolCapabilities,
    CoshCoreAdapter, EcsProbeTask, FakeAgentAdapter, FreshSessionOutcome,
    HostExecutedShellMetadata, HostExecutedShellResult, ProviderCancellationArtifact,
    ProviderCancellationArtifactKind, ProviderCancellationArtifactStore, QwenCliAdapter,
    SessionClearFailure, SessionClearInterruption, SessionClearPlan, SessionClearResult,
    SessionErrorInfo, SessionHealth, SessionList, SessionManagementClient, SessionRecovery,
    SessionRecoveryState, SessionRuntimeState, SessionSummary, ShellEvidenceAction,
};

#[allow(unused_imports)]
pub(crate) use implementation::{
    command_with_trusted_startup_path, prompt_from_request, provider_prompt_contract,
};
