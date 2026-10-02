pub(crate) use std::collections::HashSet;
pub(crate) use std::io::Write;

#[allow(unused_imports)]
pub(crate) use crate::adapter::{adapter_for_kind, AdapterInstance, AdapterKind, AgentAdapter};
pub(crate) use crate::adapter::{
    AgentRunHandle, AgentRunPoll, ApprovalDecision, ApprovalResponse, AuthFieldInfo,
    AuthProviderInfo, AuthResponse, FakeAgentAdapter, HostExecutedShellMetadata,
    HostExecutedShellResult, ProviderCancellationArtifactKind, ProviderCancellationArtifactStore,
};
pub(crate) use crate::agent::govern_agent_events;
pub(crate) use crate::agent::govern_agent_events_with_language;
#[allow(unused_imports)]
pub(crate) use crate::command::{
    classify_exit, classify_shell_handoff_command_outcome, first_program_token, ExitCodeCategory,
};
pub(crate) use crate::config::{
    clear_hook_feedback_store, clear_project_trust_store, record_hook_feedback_preference,
    trust_project_root, untrust_project_root, HookFeedbackPreference,
};
pub(crate) use crate::config::{
    language_config_status, load_config, parse_language_setting, resolve_language_setting,
    write_user_language_config, CoshConfig, Language, LanguageConfigStatus,
};
pub(crate) use crate::evidence::{
    build_related_history_index, context_blocks_from_entries, redact_provider_command_text,
    RelatedHistoryConfig,
};
pub(crate) use crate::hooks::builtin::default_builtin_hooks;
pub(crate) use crate::hooks::HookEngine;
pub(crate) use crate::hooks::{HookSourceInfo, RegisteredHookInfo};
pub(crate) use crate::i18n::{I18n, MessageId};
pub(crate) use crate::ledger::build_command_blocks;
pub(crate) use crate::parser::{
    agent_request_confirmed_by_events, agent_request_for_auto_failure,
    agent_request_from_insight_binding, agent_request_from_intercepted_input,
    approval_command_from_event, event_cancels_failed_command_analysis,
    event_confirms_failed_command_analysis, event_requests_agent_cancel,
    failed_command_agent_request_after_confirmation as agent_request_after_confirmation,
    findings_from_blocks, interventions_from_findings, recommendation_action_from_event,
    ApprovalCommandKind, RecommendationActionKind,
};
pub(crate) use crate::raw_input::{RawInputCapture, RawObserverAction};
pub(crate) use crate::shell_host::{
    run_line_interactive_bash, run_raw_interactive_bash_with_event_view,
    run_raw_interactive_zsh_with_event_view, run_scripted_bash, ScriptedInput, ShellHostConfig,
};
pub(crate) use crate::slash::registry::{
    active_slash_commands, visible_slash_commands, SlashCommandSpec,
};
pub(crate) use crate::tools::apply_readonly_config;
pub(crate) use crate::tools::classification::agent_implicit_pager_policy;
pub(crate) use crate::tools::{
    assess_shell_command, blocked_shell_binding_assessment, display::display_for_tool,
    is_readonly_builtin_tool_name, is_shell_tool_name, AssessmentConfidence, AssessmentPolicy,
    AssessmentSource, AutoAllowEvidence, AutoExecutionPolicy, AutoExecutionRoute,
    CommandAssessment, CommandRiskOutputStability, ExecutionDecision, OutputExposure,
};
pub(crate) use crate::types::{
    AgentContextBinding, AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandOrigin,
    CommandStatus, Finding, FindingSeverity, GovernanceDecision, GovernancePolicyDecision,
    GovernedEvent, OutputRefs, Policy, QuestionSelectionMode, ShellEvent, ShellEventKind,
    ShellHandoffRequest,
};
pub(crate) use crate::ui::AgentHooksView;
pub(crate) use crate::ui::{
    health_uses_startup_row, hook_warning_icon, render_transcript, ActivityDetailsPanelModel,
    ActivityPanelModel, ActivityRowModel, ActivityToolRowModel, AgentStatusAnimation,
    ApprovalActionSet, ApprovalDetailsPanelModel, ApprovalJournalEntryModel,
    ApprovalJournalPanelModel, ApprovalPanelAction, ApprovalPanelModel, ApprovalReceiptPanelModel,
    CommandAssessmentSummaryModel, HealthBannerModel, HelpPanelEntry, HelpPanelGroup,
    HelpPanelModel, HookEntryView, HookEventGroup, HookStatusPanelModel, HookWarningView,
    MarkdownStreamBlock, NoticePanelModel, QuestionAnswerPanelModel, QuestionInputFeedback,
    QuestionPanelModel, QuestionPanelPresentation, RatatuiInlineRenderer,
    RecommendationActionPanelModel, RecommendationPanelModel, ToolInvocationCardModel,
    ToolInvocationDensity, ToolInvocationTone,
};

#[cfg(test)]
pub(crate) use crate::adapter::{CoshCoreAdapter, QwenCliAdapter};
#[cfg(test)]
pub(crate) use crate::hooks::model::{HookMatcher, HookTrigger};
#[cfg(test)]
pub(crate) use crate::hooks::{ExternalHookConfig, ExternalHookSource};
#[cfg(test)]
pub(crate) use crate::types::COMMAND_OUTPUT_REF_MAX_BYTES;

pub(crate) use crate::activity::runtime::{
    record_activity_rows_with_policy, render_activity_rows,
    render_provider_native_shell_transcript, ActivityRecordPolicy,
};
pub(crate) use crate::agent::events::flush_held_agent_events;
pub(crate) use crate::agent::run::{
    control_queue_has_capacity, start_agent_run, start_agent_run_control_response,
    start_agent_run_with_origin_disposition, stop_active_agent_run_without_rendering,
    AgentRunOrigin, AgentStartDisposition, AgentStartIntent,
};
pub(crate) use crate::approval::approved_tool::{
    request_is_executable_bash_tool, request_is_readonly_builtin_tool,
};
pub(crate) use crate::approval::handoff::{
    command_matches_trust_key, queue_approved_shell_handoff,
};
pub(crate) use crate::approval::panel::render_approval_requests;
pub(crate) use crate::approval::requests::{
    approval_request_from_governed_event, record_approval_requests, record_auto_approved_request,
    record_deferred_fallback_request, record_hook_blocked_staged_request,
    record_policy_refused_request, record_staged_unresolved_request,
    refresh_shell_request_assessment,
};
pub(crate) use crate::approval::runtime::render_approval_resolution;
pub(crate) use crate::question::runtime::{
    agent_request_from_pending_question_answer, has_pending_question, record_user_questions,
    render_question_answer_notice, render_user_questions,
};
pub(crate) use crate::recommendation::runtime::{
    record_selectable_recommendations, render_selectable_recommendations,
};
pub(crate) use crate::runtime::approval_state::{
    ApprovalRequestKind, ApprovalRequestStatus, ProviderShellRequestKind,
    RuntimeApprovalJournalEntry, RuntimeApprovalRequest, RuntimeCommandAssessmentSummary,
};
pub(crate) use crate::runtime::continuity::{
    continuity_debug_lines, continuity_prompt_hint, record_agent_run_facts, record_user_intent,
};
pub(crate) use crate::runtime::details::render_runtime_details;
pub(crate) use crate::runtime::dispatcher::stable_event_key;
pub(crate) use crate::runtime::hooks::hook_routing_hints_for_block;
pub(crate) use crate::runtime::state::{AnalysisMode, CoshApprovalMode, InlineState};
