//! Pending Agent-request queue: admission classes, capacity rules, and the
//! single enqueue gate shared by every start path.

use crate::agent::run::{AgentRunOrigin, AgentStartDisposition, AgentStartIntent};
use crate::runtime::state::{AgentRunState, InlineState};
use crate::types::AgentRequest;

/// Maximum number of *normal* (cappable) requests held in the pending queue.
///
/// Bounds unbounded growth when the user keeps submitting requests while the
/// Agent is paused (e.g. a long background compaction). Beyond this, new
/// [`PendingRequestClass::Normal`] requests are rejected with a visible
/// notice instead of accumulating. Normal requests can never spill into the
/// control-response reserve.
pub(crate) const MAX_QUEUED_AGENT_REQUESTS: usize = 32;

/// Reserved queue slots for control-protocol responses beyond the normal
/// capacity, so a full user queue can never starve a question answer or
/// approval resolution.
pub(crate) const CONTROL_RESPONSE_RESERVED_SLOTS: usize = 8;

/// Absolute hard cap on the pending queue across *all* request classes.
///
/// Control responses may also use unoccupied normal capacity, but nothing —
/// including a burst of multi-tool approval resolutions — may grow the queue
/// past this bound.
pub(crate) const MAX_TOTAL_QUEUED_AGENT_REQUESTS: usize =
    MAX_QUEUED_AGENT_REQUESTS + CONTROL_RESPONSE_RESERVED_SLOTS;

/// Admission class of a queued request; persisted on the queue entry so a
/// dequeue/re-queue cycle (compaction pause, provider-timeout resume) can
/// never silently downgrade a control response to a droppable request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingRequestClass {
    /// Fresh user input or internal work. Subject to
    /// [`MAX_QUEUED_AGENT_REQUESTS`]; rejected with
    /// [`AgentStartDisposition::QueueFull`] at capacity and droppable by
    /// provider-timeout trimming.
    Normal,
    /// A control-protocol response (question answer, approval resolution)
    /// whose pending card state has already been consumed. Never trimmed;
    /// admitted into the reserved slots up to the total hard cap.
    ControlResponse,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingAgentRequest {
    pub(crate) request: AgentRequest,
    pub(crate) origin: AgentRunOrigin,
    pub(crate) intent: AgentStartIntent,
    pub(crate) class: PendingRequestClass,
    pub(crate) selectable_after_event_index: Option<usize>,
    pub(crate) before_held_text: bool,
}

/// Whether the pending queue can currently admit one control-protocol
/// response.
///
/// This is a pure capacity check. Callers must combine it with their own
/// delivery plan and only gate on it when the response would actually be
/// *enqueued* as a fallback Agent continuation: a response delivered directly
/// to the active provider owner, resolved into a foreground shell handoff, or
/// started immediately because the stopped/absent run leaves nothing to queue
/// behind consumes no queue slot and must never be blocked by a full queue —
/// blocking it would deadlock the provider that is waiting for exactly this
/// response while the queue cannot drain. See the question/approval runtimes
/// for the ownership-aware `*_needs_queue_slot` predicates.
///
/// When a caller does need a slot, it must check this *before* consuming
/// question/approval card state, so a full control queue is discovered while
/// the card is still pending and the user can simply retry. The check and
/// the subsequent enqueue run inside one synchronous dispatch, so the queue
/// cannot change in between.
pub(crate) fn control_queue_has_capacity(state: &InlineState) -> bool {
    admission_allows(&state.agent_run, PendingRequestClass::ControlResponse)
}

/// Single admission rule for the pending queue.
///
/// - Normal requests: bounded by [`MAX_QUEUED_AGENT_REQUESTS`] *and* the
///   total hard cap, so user input can never occupy the control reserve.
/// - Control responses: bounded only by [`MAX_TOTAL_QUEUED_AGENT_REQUESTS`],
///   so they may use free normal capacity plus the reserved slots but can
///   never grow the queue without bound.
fn admission_allows(agent_run: &AgentRunState, class: PendingRequestClass) -> bool {
    let total = agent_run.queued_requests.len();
    if total >= MAX_TOTAL_QUEUED_AGENT_REQUESTS {
        return false;
    }
    match class {
        PendingRequestClass::Normal => {
            let normal = agent_run
                .queued_requests
                .iter()
                .filter(|pending| pending.class == PendingRequestClass::Normal)
                .count();
            normal < MAX_QUEUED_AGENT_REQUESTS
        }
        PendingRequestClass::ControlResponse => true,
    }
}

/// Queues a pending request through the central admission rule.
///
/// Returns [`AgentStartDisposition::Queued`] when enqueued, or
/// [`AgentStartDisposition::QueueFull`] when admission rejected it (the
/// caller must surface that, not drop it silently).
pub(super) fn enqueue(
    state: &mut InlineState,
    pending: PendingAgentRequest,
) -> AgentStartDisposition {
    if !admission_allows(&state.agent_run, pending.class) {
        return AgentStartDisposition::QueueFull;
    }
    state.agent_run.queue_request(pending);
    AgentStartDisposition::Queued
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentMode, CommandBlock, CommandStatus, OutputRefs};

    fn pending_with_class(id: &str, class: PendingRequestClass) -> PendingAgentRequest {
        PendingAgentRequest {
            request: AgentRequest {
                id: id.to_string(),
                session_id: "session-1".to_string(),
                command_block: CommandBlock {
                    id: format!("cmd-{id}"),
                    session_id: "session-1".to_string(),
                    command: "echo hi".to_string(),
                    origin: Default::default(),
                    cwd: "/repo".to_string(),
                    end_cwd: "/repo".to_string(),
                    started_at_ms: 1,
                    ended_at_ms: 2,
                    duration_ms: 1,
                    exit_code: 0,
                    status: CommandStatus::Completed,
                    output: OutputRefs {
                        terminal_output_ref: None,
                        terminal_output_bytes: 0,
                    },
                    shell_environment_generation: None,
                    audit_identity: None,
                },
                context_blocks: Vec::new(),
                context_hints: Vec::new(),
                user_input: Some("queued".to_string()),
                findings: Vec::new(),
                mode: AgentMode::RecommendOnly,
                user_confirmed: true,
                hook_finding: None,
                recommended_skill: None,
            },
            origin: AgentRunOrigin::Standard,
            intent: AgentStartIntent::UserInitiated,
            class,
            selectable_after_event_index: None,
            before_held_text: false,
        }
    }

    #[test]
    fn admission_reserves_control_slots_and_enforces_the_total_hard_cap() {
        let mut state = InlineState::default();
        // Fill the normal capacity: further normal requests are rejected but
        // control responses still fit in the reserved slots.
        for index in 0..MAX_QUEUED_AGENT_REQUESTS {
            state
                .agent_run
                .queued_requests
                .push_back(pending_with_class(
                    &format!("normal-{index}"),
                    PendingRequestClass::Normal,
                ));
        }
        assert!(!admission_allows(
            &state.agent_run,
            PendingRequestClass::Normal
        ));
        assert!(admission_allows(
            &state.agent_run,
            PendingRequestClass::ControlResponse
        ));

        // Exhaust the reserve: at the total hard cap nothing is admitted,
        // so even a burst of multi-tool approval resolutions cannot grow the
        // queue without bound.
        for index in 0..CONTROL_RESPONSE_RESERVED_SLOTS {
            state
                .agent_run
                .queued_requests
                .push_back(pending_with_class(
                    &format!("control-{index}"),
                    PendingRequestClass::ControlResponse,
                ));
        }
        assert_eq!(
            state.agent_run.queued_requests.len(),
            MAX_TOTAL_QUEUED_AGENT_REQUESTS
        );
        assert!(!admission_allows(
            &state.agent_run,
            PendingRequestClass::ControlResponse
        ));
        assert!(!admission_allows(
            &state.agent_run,
            PendingRequestClass::Normal
        ));
    }

    #[test]
    fn normal_requests_never_occupy_the_control_reserve() {
        let mut state = InlineState::default();
        // One control response in the queue must not shrink normal capacity
        // below its own bound, and normals stop exactly at their capacity
        // even though total slots remain.
        state
            .agent_run
            .queued_requests
            .push_back(pending_with_class(
                "control-0",
                PendingRequestClass::ControlResponse,
            ));
        for index in 0..MAX_QUEUED_AGENT_REQUESTS {
            assert!(admission_allows(
                &state.agent_run,
                PendingRequestClass::Normal
            ));
            state
                .agent_run
                .queued_requests
                .push_back(pending_with_class(
                    &format!("normal-{index}"),
                    PendingRequestClass::Normal,
                ));
        }
        assert!(!admission_allows(
            &state.agent_run,
            PendingRequestClass::Normal
        ));
        assert!(
            state.agent_run.queued_requests.len() < MAX_TOTAL_QUEUED_AGENT_REQUESTS,
            "control reserve must remain available"
        );
    }

    #[test]
    fn control_queue_capacity_is_a_pure_queue_length_check() {
        let mut state = InlineState::default();
        assert!(control_queue_has_capacity(&state));

        // Capacity ignores what else is happening (active runs, compaction):
        // callers decide whether a slot is even needed. Only the hard cap
        // flips the answer.
        crate::slash::session::note_compaction_recommendation(
            &mut state,
            "00000000-0000-4000-8000-000000000000:1:0:200000:100000",
        );
        assert!(control_queue_has_capacity(&state));

        for index in 0..MAX_TOTAL_QUEUED_AGENT_REQUESTS {
            state
                .agent_run
                .queued_requests
                .push_back(pending_with_class(
                    &format!("control-{index}"),
                    PendingRequestClass::ControlResponse,
                ));
        }
        assert!(!control_queue_has_capacity(&state));
    }
}
