use std::collections::HashMap;

use crate::tools::is_shell_tool_name;
use crate::{
    config::Language,
    i18n::{I18n, MessageId},
    types::{
        AgentEvent, AuditRecord, GovernanceDecision, GovernancePolicyDecision, GovernedEvent,
        Policy,
    },
};

use super::display::display_agent_error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceOutput {
    pub events: Vec<GovernedEvent>,
    pub audit: Vec<AuditRecord>,
}

pub fn govern_agent_events(events: &[AgentEvent], policy: &Policy) -> GovernanceOutput {
    govern_agent_events_with_language(events, policy, Language::EnUs)
}

pub fn govern_agent_events_with_language(
    events: &[AgentEvent],
    policy: &Policy,
    language: Language,
) -> GovernanceOutput {
    let i18n = I18n::new(language);
    let mut governed = Vec::new();
    let mut audit = Vec::new();

    for (idx, event) in events.iter().cloned().enumerate() {
        let (decision, policy_decision, reason, display_text, auto_execute) = match &event {
            AgentEvent::StatusChanged { phase, message, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "agent status is display-only".to_string(),
                format!(
                    "{}\n{message}",
                    i18n.format(MessageId::AgentGovernanceStatusLine, &[("phase", phase)])
                ),
                false,
            ),
            AgentEvent::Recommendation {
                summary,
                commands,
                auto_execute,
                ..
            } => {
                let stripped = *auto_execute || policy.recommend_only;
                let reason = if stripped {
                    "recommendation is display-only in MVP".to_string()
                } else {
                    "recommendation allowed for display".to_string()
                };
                (
                    if stripped {
                        GovernanceDecision::Degraded
                    } else {
                        GovernanceDecision::Display
                    },
                    GovernancePolicyDecision::DisplayOnly,
                    reason,
                    format!(
                        "{}{}",
                        summary,
                        render_recommended_commands(commands.as_slice(), &i18n)
                    ),
                    false,
                )
            }
            AgentEvent::ToolCall { name, input, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::NeedsUserApproval,
                "tool call requires explicit approval before execution".to_string(),
                render_blocked_tool_request(name, input, &i18n),
                false,
            ),
            AgentEvent::UserQuestion {
                question, options, ..
            } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "agent question requires explicit user input".to_string(),
                render_user_question(question, options, &i18n),
                false,
            ),
            AgentEvent::Action { command, .. } => (
                GovernanceDecision::Rejected,
                GovernancePolicyDecision::HostBlocked,
                "agent actions cannot execute commands in MVP".to_string(),
                render_blocked_shell_command(command, &i18n),
                false,
            ),
            AgentEvent::ToolPermissionRequest {
                tool_name,
                tool_input,
                ..
            } => {
                let input_str = serde_json::to_string(tool_input).unwrap_or_default();
                (
                    GovernanceDecision::Display,
                    GovernancePolicyDecision::NeedsUserApproval,
                    "tool permission request via control protocol".to_string(),
                    render_blocked_tool_request(tool_name, &input_str, &i18n),
                    false,
                )
            }

            AgentEvent::ToolOutputDelta {
                tool_id,
                stream,
                text,
                ..
            } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::AuditOnly,
                "tool output is display-only".to_string(),
                format!(
                    "{}\n{text}",
                    i18n.format(
                        MessageId::AgentGovernanceToolOutputLine,
                        &[("tool_id", tool_id), ("stream", stream)]
                    )
                ),
                false,
            ),
            AgentEvent::ToolHookVerdict { .. } => (
                // The verdict marker is audit metadata; the rejection is
                // already visible via the failed tool result and the
                // governance hook panel.
                GovernanceDecision::Display,
                GovernancePolicyDecision::AuditOnly,
                "hook verdict marker is audit-only".to_string(),
                String::new(),
                false,
            ),
            AgentEvent::ToolCompleted {
                tool_id, status, ..
            } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::AuditOnly,
                "tool completion is display-only".to_string(),
                format!(
                    "{}\n{}",
                    i18n.format(
                        MessageId::AgentGovernanceToolCompletedLine,
                        &[("tool_id", tool_id)]
                    ),
                    i18n.format(MessageId::AgentGovernanceStatusLine, &[("phase", status)])
                ),
                false,
            ),
            AgentEvent::TextDelta { text, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "assistant text is display-only".to_string(),
                text.clone(),
                false,
            ),
            AgentEvent::AgentCompleted { summary, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "agent completion is display-only".to_string(),
                summary.clone(),
                false,
            ),
            AgentEvent::AgentFailed { error, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "agent failure is display-only".to_string(),
                display_agent_error(error, &i18n),
                false,
            ),
            AgentEvent::AgentCancelled { reason, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "agent cancellation is display-only".to_string(),
                format!(
                    "{}\n{}",
                    i18n.t(MessageId::FailedAnalysisCancelledTitle),
                    i18n.format(
                        MessageId::AgentGovernanceReasonLine,
                        &[("reason", &agent_cancelled_reason(reason, &i18n))]
                    )
                ),
                false,
            ),
            AgentEvent::AuthRequired { .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "auth required is display-only".to_string(),
                "Authentication credentials required".to_string(),
                false,
            ),
            AgentEvent::ShellEvidenceRequest { action, .. } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::AuditOnly,
                "shell evidence request is handled through control protocol".to_string(),
                format!("shell evidence requested: {}", action.as_str()),
                false,
            ),
            AgentEvent::HookNotification {
                hook_name,
                message,
                decision,
                ..
            } => (
                GovernanceDecision::Display,
                GovernancePolicyDecision::DisplayOnly,
                "hook notification is display-only".to_string(),
                hook_notification_display_text(hook_name, message, decision.as_deref(), &i18n),
                false,
            ),
        };

        let governed_event = GovernedEvent {
            decision: decision.clone(),
            policy_decision,
            event,
            reason: reason.clone(),
            display_text,
            auto_execute,
        };

        audit.push(AuditRecord {
            id: format!("audit-{idx}"),
            subject: format!("{:?}", governed_event.event),
            decision,
            reason,
        });
        governed.push(governed_event);
    }

    GovernanceOutput {
        events: governed,
        audit,
    }
}

pub(crate) fn hook_notification_display_text(
    hook_name: &str,
    message: &str,
    decision: Option<&str>,
    i18n: &I18n,
) -> String {
    let hook_name = hook_name.trim();
    let hook_name = if hook_name.is_empty() {
        i18n.t(MessageId::AgentGovernanceHookUnknown)
    } else {
        hook_name
    };
    let message = message.trim();
    let message = if message.is_empty() {
        i18n.t(MessageId::AgentGovernanceHookNoMessage)
    } else {
        message
    };
    let decision = decision
        .map(str::trim)
        .filter(|decision| !decision.is_empty())
        .unwrap_or_else(|| i18n.t(MessageId::AgentGovernanceHookDecisionUnspecified));

    i18n.format(
        MessageId::AgentGovernanceHookNotification,
        &[
            ("hook", hook_name),
            ("message", message),
            ("decision", decision),
        ],
    )
}

/// Builds the throwaway Governance-panel projection for one batch of governed
/// events, collapsing repeated permissive hook notices into one weak line each.
///
/// Permissive notices (`allow` / `approve`) carry no decision for the user to
/// make, so a hook that fires on every tool call floods the panel with byte
/// identical three-line blocks (issue #2197). They collapse to
/// `• {hook}: {message}` plus a ` ×N` hit count; every other decision —
/// including an absent or unrecognized one — keeps the full multi-line form so
/// blocked and asked-about actions stay equally prominent.
///
/// The returned events are for rendering only: callers must keep the input
/// slice intact, because approval-card linking and the audit trail still need
/// every original notification.
// The lib facade compiles this module without the binary's `agent::finish`, so
// the only non-test caller is invisible from that target.
#[allow(dead_code)]
pub(crate) fn project_hook_notifications_for_display(
    events: &[GovernedEvent],
    i18n: &I18n,
) -> Vec<GovernedEvent> {
    let mut projected: Vec<GovernedEvent> = Vec::with_capacity(events.len());
    // Aggregation key -> (index into `projected`, hit count). Keyed on the
    // normalized message rather than a digit-generalized shape: numbers such as
    // the `[REDACTED_CARD:2603]` sample distinguish separate security hits and
    // must never be merged away.
    let mut collapsed: HashMap<(String, String), (usize, usize)> = HashMap::new();

    for event in events {
        let Some((hook_name, message)) = permissive_hook_notification_parts(event) else {
            projected.push(event.clone());
            continue;
        };

        let key = (
            hook_name.trim().to_string(),
            normalize_hook_message(hook_name, message),
        );
        if let Some((_, hits)) = collapsed.get_mut(&key) {
            *hits += 1;
            continue;
        }

        // First occurrence keeps its original position, so panel order still
        // matches the order the hooks actually fired in.
        let mut summary = event.clone();
        summary.display_text = hook_notification_summary_line(&key.0, &key.1, i18n);
        collapsed.insert(key, (projected.len(), 1));
        projected.push(summary);
    }

    for (index, hits) in collapsed.into_values() {
        if hits > 1 {
            projected[index].display_text.push_str(&format!(" ×{hits}"));
        }
    }

    projected
}

/// Returns the raw `(hook_name, message)` when this event is a hook
/// notification whose decision only confirms that the run continued.
fn permissive_hook_notification_parts(event: &GovernedEvent) -> Option<(&str, &str)> {
    let AgentEvent::HookNotification {
        hook_name,
        message,
        decision,
        ..
    } = &event.event
    else {
        return None;
    };

    let decision = decision.as_deref()?.trim();
    (decision.eq_ignore_ascii_case("allow") || decision.eq_ignore_ascii_case("approve"))
        .then_some((hook_name.as_str(), message.as_str()))
}

/// Normalizes a hook message for aggregation and single-line display: collapse
/// whitespace runs, then drop a leading `[hook_name]` that exactly repeats the
/// hook name the panel already prints.
fn normalize_hook_message(hook_name: &str, message: &str) -> String {
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let hook_name = hook_name.trim();
    if hook_name.is_empty() {
        return collapsed;
    }

    if let Some(rest) = collapsed.strip_prefix(&format!("[{hook_name}]")) {
        return rest.trim_start().to_string();
    }
    collapsed
}

/// Renders one collapsed permissive notice, reusing the localized fallbacks for
/// an empty hook name or message.
fn hook_notification_summary_line(hook_name: &str, message: &str, i18n: &I18n) -> String {
    let hook_name = if hook_name.is_empty() {
        i18n.t(MessageId::AgentGovernanceHookUnknown)
    } else {
        hook_name
    };
    let message = if message.is_empty() {
        i18n.t(MessageId::AgentGovernanceHookNoMessage)
    } else {
        message
    };

    format!("• {hook_name}: {message}")
}

fn render_recommended_commands(commands: &[String], i18n: &I18n) -> String {
    if commands.is_empty() {
        return String::new();
    }

    let rendered = commands
        .iter()
        .map(|command| format!("\n  - {command}"))
        .collect::<String>();
    format!(
        "\n{}{rendered}",
        i18n.t(MessageId::AgentRecommendedCommandsLabel)
    )
}

fn render_blocked_tool_request(name: &str, input: &str, i18n: &I18n) -> String {
    format!(
        "{}\n{}: {input}\n{}",
        i18n.format(
            MessageId::AgentGovernanceApprovalRequiredLine,
            &[("subject", &user_facing_tool_name(name, i18n))]
        ),
        i18n.t(MessageId::ApprovalCommandLabel),
        i18n.t(MessageId::AgentGovernanceBlockedUserApprovalLine)
    )
}

fn render_blocked_shell_command(command: &str, i18n: &I18n) -> String {
    format!(
        "{}\n{}: {command}\n{}",
        i18n.format(
            MessageId::AgentGovernanceApprovalRequiredLine,
            &[(
                "subject",
                i18n.t(MessageId::AgentGovernanceShellCommandSubject)
            )]
        ),
        i18n.t(MessageId::ApprovalCommandLabel),
        i18n.t(MessageId::AgentGovernanceBlockedUserApprovalLine)
    )
}

fn user_facing_tool_name(name: &str, i18n: &I18n) -> String {
    if is_shell_tool_name(name) {
        i18n.t(MessageId::AgentGovernanceBashCommandSubject)
            .to_string()
    } else {
        i18n.format(MessageId::AgentGovernanceToolSubject, &[("tool", name)])
    }
}

fn render_user_question(question: &str, options: &[String], i18n: &I18n) -> String {
    let question = display_question_text(question, i18n);
    if options.is_empty() {
        return i18n.format(
            MessageId::AgentGovernanceQuestionLine,
            &[("question", question.as_str())],
        );
    }

    let rendered = options
        .iter()
        .enumerate()
        .map(|(idx, option)| format!("\n  {}. {}", idx + 1, option))
        .collect::<String>();
    format!(
        "{}{rendered}",
        i18n.format(
            MessageId::AgentGovernanceQuestionLine,
            &[("question", question.as_str())]
        )
    )
}

fn display_question_text(question: &str, i18n: &I18n) -> String {
    let question = question.trim();
    if question.is_empty() {
        i18n.t(MessageId::QuestionDefaultPrompt).to_string()
    } else {
        question.to_string()
    }
}

fn agent_cancelled_reason(reason: &str, i18n: &I18n) -> String {
    if reason == "user requested cancellation" {
        return i18n
            .t(MessageId::AgentCancelledUserRequestedReason)
            .to_string();
    }
    reason.to_string()
}
