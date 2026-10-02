#[allow(dead_code)]
#[path = "mod.rs"]
mod implementation;

pub use implementation::{
    AgentEvent, AgentMode, AgentRequest, AuditRecord, CommandBlock, CommandOrigin, CommandStatus,
    CoshApprovalMode, Finding, FindingKind, FindingSeverity, GovernanceDecision,
    GovernancePolicyDecision, GovernedEvent, HookFinding, ImplicitPagerPolicy, Intervention,
    InterventionDecision, OutputRefs, Policy, QuestionSelectionMode, ShellCaptureLifecycle,
    ShellCaptureMetadata, ShellCommandAuditIdentity, ShellEvent, ShellEventKind,
    ShellHandoffRequest, ShellRoutingMetadata, COMMAND_OUTPUT_REF_MAX_BYTES,
    SESSION_OUTPUT_REF_MAX_BYTES,
};

#[allow(unused_imports)]
pub(crate) use implementation::{
    mark_request_sensitive_input, request_has_sensitive_input,
    request_is_analysis_only_continuation, set_request_context_binding, AgentContextBinding,
    BuiltinFactRecord, BuiltinFindingFacts, CardKind, CardModel, EvaluatedHookFinding,
    HighMemoryProcessFacts, HookProvenance, InputModel, InputOwner, MemoryPressureFacts,
    MetricsConfidence, PermissionCardRequest, ProcessMemoryFact, ShellEnvironmentSnapshot,
    BOUNDED_HANDOFF_COMMAND, NON_INTERACTIVE_PAGER_PREFIX, PROVIDER_TIMEOUT_ERROR_CODE,
    SHELL_HANDOFF_CONTINUATION_HINT, SHELL_HANDOFF_UNTRACKED_STATUS, TOOL_ARGUMENTS_STATUS_PHASE,
    TOOL_ARGUMENTS_STATUS_PREFIX, USER_APPROVAL_MODE_HINT_PREFIX,
};

pub(crate) use implementation::audit;
pub(crate) use implementation::composer;
