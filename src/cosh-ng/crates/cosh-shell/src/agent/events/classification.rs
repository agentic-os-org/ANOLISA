//! Classification helpers for structured Agent event rendering.

use crate::runtime::prelude::*;
use crate::types::TOOL_ARGUMENTS_STATUS_PHASE;

pub(super) fn is_interaction_governed_event(event: &GovernedEvent) -> bool {
    matches!(
        event.event,
        AgentEvent::UserQuestion { .. }
            | AgentEvent::AuthRequired { .. }
            | AgentEvent::Action { .. }
            | AgentEvent::ToolPermissionRequest { .. }
    )
}

pub(super) fn event_may_render_structured_surface(event: &GovernedEvent) -> bool {
    is_interaction_governed_event(event)
        || should_render_governance_block(event)
        || matches!(
            event.event,
            AgentEvent::ToolCompleted { .. } | AgentEvent::ShellEvidenceRequest { .. }
        )
}

pub(super) fn event_updates_pending_tool_status(event: &GovernedEvent) -> bool {
    // Argument generation is included even though no tool call exists yet: it is
    // the window the status was added for, and rendering it here is what ends a
    // streamed-text surface so the status is not swallowed by it.
    matches!(event.event, AgentEvent::ToolCall { .. })
        || matches!(
            &event.event,
            AgentEvent::StatusChanged { phase, .. } if phase == TOOL_ARGUMENTS_STATUS_PHASE
        )
}

pub(super) fn should_render_governance_block(event: &GovernedEvent) -> bool {
    match &event.event {
        AgentEvent::StatusChanged { .. } | AgentEvent::Recommendation { .. } => false,
        AgentEvent::ToolCall { .. }
        | AgentEvent::UserQuestion { .. }
        | AgentEvent::Action { .. }
        | AgentEvent::ToolPermissionRequest { .. } => false,
        AgentEvent::AgentFailed { .. }
        | AgentEvent::AgentCancelled { .. }
        | AgentEvent::HookNotification { .. } => true,
        AgentEvent::ToolOutputDelta { .. }
        | AgentEvent::ToolCompleted { .. }
        | AgentEvent::ToolHookVerdict { .. }
        | AgentEvent::TextDelta { .. }
        | AgentEvent::AgentCompleted { .. }
        | AgentEvent::AuthRequired { .. }
        | AgentEvent::ShellEvidenceRequest { .. } => false,
    }
}
