use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::recommendation::personal_context::discover_repo_context;
use crate::recommendation::personal_model::{ActivityContext, ActivitySource};
use crate::recommendation::personal_record::{
    agent_request_record, agent_run_record, shell_command_record,
};
use crate::recommendation::personal_runtime::{EnqueueOutcome, PersonalRuntimeWriter};
use crate::runtime::state::InlineState;
use crate::types::{
    request_context_binding, AgentContextBinding, AgentEvent, AgentRequest, CommandBlock,
    CommandOrigin, GovernedEvent,
};

#[derive(Default)]
pub(crate) struct PersonalSignalState {
    claimed_blocks: HashSet<String>,
    claimed_requests: HashSet<String>,
    claimed_runs: HashSet<String>,
    block_activity_ids: HashMap<String, String>,
    request_activity_ids: HashMap<String, String>,
    request_contexts: HashMap<String, ActivityContext>,
    pending_intent_lifecycle_id: Option<String>,
}

impl PersonalSignalState {
    fn claim_block(&mut self, id: &str) -> bool {
        self.claimed_blocks.insert(id.to_string())
    }

    fn claim_request(&mut self, id: &str) -> bool {
        self.claimed_requests.insert(id.to_string())
    }

    fn claim_run(&mut self, id: &str) -> bool {
        self.claimed_runs.insert(id.to_string())
    }

    pub(crate) fn set_pending_intent_lifecycle(&mut self, intent_lifecycle_id: String) {
        self.pending_intent_lifecycle_id = Some(intent_lifecycle_id);
    }

    fn take_pending_intent_lifecycle(&mut self) -> Option<String> {
        self.pending_intent_lifecycle_id.take()
    }
}

pub(crate) fn record_completed_command_blocks(state: &mut InlineState, blocks: &[CommandBlock]) {
    if state.analysis_mode == crate::runtime::state::AnalysisMode::Manual {
        return;
    }
    let Some(writer) = state.personalization.writer.as_mut() else {
        return;
    };
    for block in blocks {
        if !matches!(
            block.origin,
            CommandOrigin::UserInteractive
                | CommandOrigin::UserSendToShell
                | CommandOrigin::UserAnalysisAction
        ) || !state.personalization.signals.claim_block(&block.id)
        {
            continue;
        }
        let Some(identity) = writer
            .activity_identity(
                ActivitySource::ShellCommand,
                block_identity(block).as_bytes(),
            )
            .ok()
            .flatten()
        else {
            continue;
        };
        let Some(session_scope_id) = writer.session_scope_id() else {
            continue;
        };
        let Some(context) = activity_context(writer, block_cwd(block)) else {
            continue;
        };
        let Some(record) = shell_command_record(
            block,
            &identity.activity_id,
            &session_scope_id,
            &identity.source_fingerprint,
            context,
            None,
        ) else {
            continue;
        };
        if writer.try_enqueue_for_epoch(record, &identity.store_epoch) == EnqueueOutcome::Accepted {
            state
                .personalization
                .signals
                .block_activity_ids
                .insert(block.id.clone(), identity.activity_id);
        }
    }
}

pub(crate) fn record_started_agent_request(state: &mut InlineState, request: &AgentRequest) {
    if state.analysis_mode == crate::runtime::state::AnalysisMode::Manual {
        return;
    }
    let binding = request_context_binding(request);
    if matches!(
        binding,
        AgentContextBinding::ControlProtocolEvidence
            | AgentContextBinding::ShellHandoffContinuation
    ) {
        return;
    }
    let Some(writer) = state.personalization.writer.as_mut() else {
        return;
    };
    if !state.personalization.signals.claim_request(&request.id) {
        return;
    }
    let Some(identity) = writer
        .activity_identity(ActivitySource::AgentRequest, request.id.as_bytes())
        .ok()
        .flatten()
    else {
        return;
    };
    let Some(session_scope_id) = writer.session_scope_id() else {
        return;
    };
    let cwd = request_cwd(request);
    let Some(context) = activity_context(writer, cwd) else {
        return;
    };
    let context_command_activity_id = state
        .personalization
        .signals
        .block_activity_ids
        .get(&request.command_block.id)
        .cloned();
    let intent_lifecycle_id = state
        .personalization
        .signals
        .take_pending_intent_lifecycle()
        .unwrap_or_else(|| identity.activity_id.clone());
    let Some(record) = agent_request_record(
        request,
        binding,
        &identity.activity_id,
        &session_scope_id,
        &identity.source_fingerprint,
        &intent_lifecycle_id,
        context.clone(),
        context_command_activity_id,
    ) else {
        return;
    };
    if writer.try_enqueue_for_epoch(record, &identity.store_epoch) == EnqueueOutcome::Accepted {
        state
            .personalization
            .signals
            .request_activity_ids
            .insert(request.id.clone(), identity.activity_id);
        state
            .personalization
            .signals
            .request_contexts
            .insert(request.id.clone(), context);
    }
}

pub(crate) fn record_finished_agent_run(
    state: &mut InlineState,
    request: &AgentRequest,
    governed_events: &[GovernedEvent],
) {
    if state.analysis_mode == crate::runtime::state::AnalysisMode::Manual {
        return;
    }
    let Some(request_activity_id) = state
        .personalization
        .signals
        .request_activity_ids
        .get(&request.id)
        .cloned()
    else {
        return;
    };
    let Some(writer) = state.personalization.writer.as_mut() else {
        return;
    };
    if !state.personalization.signals.claim_run(&request.id) {
        return;
    }
    let identity_material = format!("{}\0run", request.id);
    let Some(identity) = writer
        .activity_identity(ActivitySource::AgentRun, identity_material.as_bytes())
        .ok()
        .flatten()
    else {
        return;
    };
    let Some(session_scope_id) = writer.session_scope_id() else {
        return;
    };
    let context = state
        .personalization
        .signals
        .request_contexts
        .get(&request.id)
        .cloned()
        .or_else(|| activity_context(writer, request_cwd(request)));
    let Some(context) = context else {
        return;
    };
    let events = terminal_events(governed_events);
    let Some(record) = agent_run_record(
        &request_activity_id,
        &events,
        &identity.activity_id,
        &session_scope_id,
        &identity.source_fingerprint,
        context,
    ) else {
        return;
    };
    let _ = writer.try_enqueue_for_epoch(record, &identity.store_epoch);
}

fn request_cwd(request: &AgentRequest) -> &Path {
    block_cwd(&request.command_block)
}

fn block_cwd(block: &CommandBlock) -> &Path {
    Path::new(if block.end_cwd.is_empty() {
        &block.cwd
    } else {
        &block.end_cwd
    })
}

fn block_identity(block: &CommandBlock) -> String {
    format!("{}\0{}", block.session_id, block.id)
}

pub(crate) fn activity_context(
    writer: &PersonalRuntimeWriter,
    cwd: &Path,
) -> Option<ActivityContext> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let host_identity = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default();
    let repo = discover_repo_context(cwd);
    writer.build_context(
        &host_identity,
        cwd,
        repo.as_ref().map(|repo| repo.root.as_path()),
        repo.as_ref()
            .and_then(|repo| repo.normalized_identity.as_deref()),
        &home,
    )
}

fn terminal_events(events: &[GovernedEvent]) -> Vec<AgentEvent> {
    events
        .iter()
        .filter_map(|event| match &event.event {
            AgentEvent::ToolCall { name, .. } => Some(AgentEvent::ToolCall {
                run_id: String::new(),
                tool_id: None,
                name: name.clone(),
                input: String::new(),
            }),
            AgentEvent::ToolPermissionRequest { tool_name, .. } => {
                Some(AgentEvent::ToolPermissionRequest {
                    run_id: String::new(),
                    request_id: String::new(),
                    tool_name: tool_name.clone(),
                    tool_input: serde_json::Value::Null,
                    tool_use_id: String::new(),
                    hook_requires_approval: false,
                    audit_ref: None,
                })
            }
            AgentEvent::AgentCompleted { .. } => Some(AgentEvent::AgentCompleted {
                run_id: String::new(),
                summary: String::new(),
            }),
            AgentEvent::AgentFailed { .. } => Some(AgentEvent::AgentFailed {
                run_id: String::new(),
                error: String::new(),
                error_code: None,
                max_turns: None,
            }),
            AgentEvent::AgentCancelled { .. } => Some(AgentEvent::AgentCancelled {
                run_id: String::new(),
                reason: String::new(),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::types::{AgentEvent, GovernanceDecision, GovernancePolicyDecision, GovernedEvent};

    use super::{terminal_events, PersonalSignalState};

    #[test]
    fn signal_state_claims_completed_blocks_and_started_requests_once() {
        let mut signals = PersonalSignalState::default();

        assert!(signals.claim_block("block-1"));
        assert!(!signals.claim_block("block-1"));
        assert!(signals.claim_request("request-1"));
        assert!(!signals.claim_request("request-1"));
    }

    #[test]
    fn pending_intent_lifecycle_is_consumed_once() {
        let mut signals = PersonalSignalState::default();
        signals.set_pending_intent_lifecycle("intent-1".into());

        assert_eq!(
            signals.take_pending_intent_lifecycle().as_deref(),
            Some("intent-1")
        );
        assert!(signals.take_pending_intent_lifecycle().is_none());
    }

    #[test]
    fn terminal_events_keep_only_tool_names_and_terminal_outcome() {
        let events = vec![
            governed(AgentEvent::TextDelta {
                run_id: "run-1".into(),
                text: "private response".into(),
            }),
            governed(AgentEvent::ToolCall {
                run_id: "run-1".into(),
                tool_id: Some("tool-1".into()),
                name: "read_file".into(),
                input: "secret input".into(),
            }),
            governed(AgentEvent::AgentFailed {
                run_id: "run-1".into(),
                error: "secret failure body".into(),
                error_code: None,
                max_turns: None,
            }),
        ];

        let retained = terminal_events(&events);

        assert_eq!(retained.len(), 2);
        assert!(matches!(
            &retained[0],
            AgentEvent::ToolCall { name, input, .. }
                if name == "read_file" && input.is_empty()
        ));
        assert!(matches!(
            &retained[1],
            AgentEvent::AgentFailed { error, .. } if error.is_empty()
        ));
    }

    fn governed(event: AgentEvent) -> GovernedEvent {
        GovernedEvent {
            event,
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            reason: String::new(),
            display_text: String::new(),
            auto_execute: false,
        }
    }
}
