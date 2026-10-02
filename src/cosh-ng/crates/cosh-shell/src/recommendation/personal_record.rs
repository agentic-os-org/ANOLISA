use std::time::{SystemTime, UNIX_EPOCH};

use crate::tools::{known_provider_tool, KnownProviderTool};
use crate::types::{
    AgentContextBinding, AgentEvent, AgentRequest, CommandBlock, CommandOrigin, CommandStatus,
};

use super::personal_model::{
    ActivityContext, ActivityOutcome, ActivityPayload, ActivityRecord, ActivitySource,
    AgentRequestBindingKind, RedactionKind, RedactionReport, ShellActivityOrigin, ToolCategory,
};
use super::personal_sanitize::{sanitize_agent_request, sanitize_shell_command, SanitizedText};

pub(crate) fn shell_command_record(
    block: &CommandBlock,
    activity_id: &str,
    session_scope_id: &str,
    source_fingerprint: &str,
    context: ActivityContext,
    parent_request_activity_id: Option<String>,
) -> Option<ActivityRecord> {
    let origin = match block.origin {
        CommandOrigin::UserInteractive => ShellActivityOrigin::Interactive,
        CommandOrigin::UserSendToShell => ShellActivityOrigin::SendToShell,
        CommandOrigin::UserAnalysisAction => ShellActivityOrigin::AnalysisAction,
        CommandOrigin::AgentHandoff
        | CommandOrigin::ProviderTool
        | CommandOrigin::ShellInternal
        | CommandOrigin::Unknown => return None,
    };
    let sanitized = sanitize_shell_command(&block.command).ok()?;
    let outcome = match block.status {
        CommandStatus::Completed if block.exit_code == 0 => ActivityOutcome::Success,
        CommandStatus::Completed | CommandStatus::Failed => ActivityOutcome::Failure,
    };

    Some(ActivityRecord {
        activity_id: activity_id.to_string(),
        session_scope_id: Some(session_scope_id.to_string()),
        source_fingerprint: source_fingerprint.to_string(),
        observed_hour_bucket: block.ended_at_ms / 3_600_000,
        source: ActivitySource::ShellCommand,
        context,
        payload: ActivityPayload::ShellCommand {
            command: sanitized.text,
            origin,
            parent_request_activity_id,
            outcome,
        },
        redaction: sanitized.report,
        summarized_generation: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_request_record(
    request: &AgentRequest,
    binding: AgentContextBinding,
    activity_id: &str,
    session_scope_id: &str,
    source_fingerprint: &str,
    intent_lifecycle_id: &str,
    context: ActivityContext,
    context_command_activity_id: Option<String>,
) -> Option<ActivityRecord> {
    let binding = match binding {
        AgentContextBinding::FreeForm => AgentRequestBindingKind::FreeForm,
        AgentContextBinding::FailedCommand => AgentRequestBindingKind::FailedCommand,
        AgentContextBinding::HookConsultation => AgentRequestBindingKind::HookConsultation,
        AgentContextBinding::StartupHealthFollowUp => {
            AgentRequestBindingKind::StartupHealthFollowUp
        }
        AgentContextBinding::SelectedCommand => AgentRequestBindingKind::SelectedCommand,
        AgentContextBinding::ControlProtocolEvidence
        | AgentContextBinding::ShellHandoffContinuation => return None,
    };
    let user_input = request.user_input.as_deref()?;
    // The shell-side secret gate flagged this input sensitive: redact the
    // whole field (journal precedent, #2138) instead of trusting the
    // sanitizer regexes to re-detect every shell-gate form (`sk-fbaa6`
    // is below the opaque-token minimum length).
    let sanitized = if crate::types::request_has_sensitive_input(request) {
        SanitizedText {
            text: "<redacted>".to_string(),
            report: RedactionReport {
                replacements: vec![RedactionKind::Secret],
                truncated: false,
                sanitizer_version: 1,
            },
        }
    } else {
        sanitize_agent_request(user_input).ok()?
    };

    Some(ActivityRecord {
        activity_id: activity_id.to_string(),
        session_scope_id: Some(session_scope_id.to_string()),
        source_fingerprint: source_fingerprint.to_string(),
        observed_hour_bucket: current_hour_bucket(),
        source: ActivitySource::AgentRequest,
        context,
        payload: ActivityPayload::AgentRequest {
            text: sanitized.text,
            binding,
            context_command_activity_id,
            intent_lifecycle_id: intent_lifecycle_id.to_string(),
            system_recommended_skill: request.recommended_skill.clone(),
        },
        redaction: sanitized.report,
        summarized_generation: None,
    })
}

pub(crate) fn agent_run_record(
    request_activity_id: &str,
    events: &[AgentEvent],
    activity_id: &str,
    session_scope_id: &str,
    source_fingerprint: &str,
    context: ActivityContext,
) -> Option<ActivityRecord> {
    if request_activity_id.is_empty() {
        return None;
    }
    let mut tool_categories = Vec::new();
    let mut outcome = None;
    for event in events {
        match event {
            AgentEvent::ToolCall { name, .. }
            | AgentEvent::ToolPermissionRequest {
                tool_name: name, ..
            } => push_tool_category(&mut tool_categories, classify_tool(name)),
            AgentEvent::AgentCompleted { .. } => outcome = Some(ActivityOutcome::Success),
            AgentEvent::AgentFailed { .. } => outcome = Some(ActivityOutcome::Failure),
            AgentEvent::AgentCancelled { .. } => outcome = Some(ActivityOutcome::Cancelled),
            _ => {}
        }
    }

    Some(ActivityRecord {
        activity_id: activity_id.to_string(),
        session_scope_id: Some(session_scope_id.to_string()),
        source_fingerprint: source_fingerprint.to_string(),
        observed_hour_bucket: current_hour_bucket(),
        source: ActivitySource::AgentRun,
        context,
        payload: ActivityPayload::AgentRun {
            request_activity_id: request_activity_id.to_string(),
            tool_categories,
            outcome: outcome?,
        },
        redaction: Default::default(),
        summarized_generation: None,
    })
}

fn push_tool_category(categories: &mut Vec<ToolCategory>, category: ToolCategory) {
    if categories.len() < 8 && !categories.contains(&category) {
        categories.push(category);
    }
}

fn classify_tool(name: &str) -> ToolCategory {
    match known_provider_tool(name) {
        Some(KnownProviderTool::Shell) => ToolCategory::Shell,
        Some(
            KnownProviderTool::ReadFile
            | KnownProviderTool::Grep
            | KnownProviderTool::Glob
            | KnownProviderTool::ListDirectory
            | KnownProviderTool::ReadManyFiles,
        ) => ToolCategory::FilesystemRead,
        Some(
            KnownProviderTool::WriteFile
            | KnownProviderTool::Edit
            | KnownProviderTool::NotebookEdit
            | KnownProviderTool::SaveMemory,
        ) => ToolCategory::FilesystemWrite,
        Some(KnownProviderTool::Skill) => ToolCategory::Skill,
        Some(KnownProviderTool::WebFetch | KnownProviderTool::WebSearch) => {
            ToolCategory::ExternalService
        }
        Some(_) | None => ToolCategory::Other,
    }
}

fn current_hour_bucket() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 3600)
        .unwrap_or_default()
}
