use std::time::Instant;

use crate::agent::continuation::{
    annotate_continuation_user_approval_mode, provider_mode_for_agent_run,
};
use crate::agent::poll::poll_active_agent_run;
use crate::agent::queue::enqueue;
use crate::agent::skill_context::finalize_agent_request_skill_context;
use crate::evidence::request::ParsedCoshRequest;
use crate::evidence::stream::{CoshRequestAuditRecord, CoshRequestStreamFilter};
use crate::recommendation::personal_integration::record_started_agent_request;
use crate::runtime::prelude::*;

// Queue admission types live in [`crate::agent::queue`]; re-exported here so
// the run-centric call sites keep one canonical import path.
pub(crate) use crate::agent::queue::{
    control_queue_has_capacity, PendingAgentRequest, PendingRequestClass,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AgentRunOrigin {
    #[default]
    Standard,
    InsightPrompt,
    AutoFailure,
}

impl AgentRunOrigin {
    pub(crate) fn is_insight_triggered(self) -> bool {
        matches!(self, Self::InsightPrompt | Self::AutoFailure)
    }
}

/// Whether an Agent run was asked for by the user or is internal best-effort.
///
/// This is deliberately independent of [`AgentRunOrigin`]: the same origin can
/// carry either intent (e.g. an `InsightPrompt` may be a user-chosen prompt
/// ghost *or* an internal continuation), so user intent must be stated
/// explicitly at each call site rather than inferred from the origin. The
/// compaction gate uses it to decide whether a request may be dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStartIntent {
    /// The user explicitly requested this run — typed input, chose a prompt
    /// ghost, answered a question, sent an evidence card, or resolved an
    /// approval. Must never be silently lost; it is queued and resumed in FIFO
    /// order across a background compaction.
    UserInitiated,
    /// A best-effort internal continuation (auto failure analysis, hook
    /// consultation, evidence auto-follow-up, recovery/handoff fallback). May
    /// be dropped while a compaction is running or imminent, because the
    /// pre-compaction context it captured is exactly what compaction rewrites.
    InternalBestEffort,
}

/// Outcome of an Agent-run start attempt.
///
/// The start path is not fire-and-forget: callers that record side effects
/// (e.g. marking a command block analyzed) before starting must inspect this
/// so a suppressed request is not mistaken for a handled one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStartDisposition {
    /// A model process was launched for this request.
    Started,
    /// Deferred behind the currently active run or a background compaction;
    /// starts (FIFO) when the blocker clears.
    Queued,
    /// Rejected because a background compaction is rewriting the transcript.
    /// The request did not run and was not queued.
    SuppressedByCompaction,
    /// Rejected because the pending-request queue is at capacity. The request
    /// did not run and was not queued; the caller must surface this to the
    /// user rather than dropping it silently.
    QueueFull,
}

pub(crate) struct ActiveAgentRun {
    pub(crate) request: AgentRequest,
    pub(crate) origin: AgentRunOrigin,
    pub(crate) handle: AgentRunHandle,
    pub(crate) provider_name: &'static str,
    pub(crate) language: Language,
    pub(crate) renderer: RatatuiInlineRenderer,
    pub(crate) status_animation: AgentStatusAnimation,
    pub(crate) markdown_stream: MarkdownStreamBlock,
    pub(crate) governed_events: Vec<GovernedEvent>,
    pub(crate) deferred_events: Vec<GovernedEvent>,
    pub(crate) held_events: Vec<GovernedEvent>,
    pub(crate) cosh_request_filter: CoshRequestStreamFilter,
    pub(crate) pending_cosh_requests: Vec<ParsedCoshRequest>,
    pub(crate) pending_cosh_request_audits: Vec<CoshRequestAuditRecord>,
    pub(crate) pending_hook_notifications: Vec<PendingHookNotification>,
    pub(crate) rendered_governed_event_count: usize,
    pub(crate) selectable_after_event_index: Option<usize>,
    pub(crate) started_at: Instant,
    pub(crate) last_activity_at: Instant,
    pub(crate) last_heartbeat_at: Instant,
    pub(crate) current_phase: String,
    pub(crate) current_message: String,
    pub(crate) has_visible_text_delta: bool,
    pub(crate) completed: bool,
    pub(crate) host_completed_tool_ids: Vec<String>,
}

#[cfg(test)]
pub(crate) mod test_support;

impl ActiveAgentRun {
    pub(crate) fn prepare_structured_surface<W: Write>(
        &mut self,
        output: &mut W,
    ) -> std::io::Result<bool> {
        self.status_animation.clear(output)?;
        let finished = self.markdown_stream.finish(output, None)?;
        if finished {
            self.has_visible_text_delta = false;
        }
        Ok(finished)
    }

    pub(crate) fn mark_host_completed_tool(&mut self, tool_id: &str) {
        if tool_id.trim().is_empty() {
            return;
        }
        if !self
            .host_completed_tool_ids
            .iter()
            .any(|existing| existing == tool_id)
        {
            self.host_completed_tool_ids.push(tool_id.to_string());
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingHookNotification {
    pub(crate) tool_use_id: Option<String>,
    pub(crate) hook_name: String,
    pub(crate) message: String,
    pub(crate) decision: Option<String>,
}

pub(crate) fn start_agent_run<W: Write>(
    request: &AgentRequest,
    intent: AgentStartIntent,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    selectable_after_event_index: Option<usize>,
) -> std::io::Result<()> {
    start_agent_run_with_origin(
        request,
        AgentRunOrigin::Standard,
        intent,
        adapter,
        state,
        output,
        selectable_after_event_index,
    )
}

pub(crate) fn start_agent_run_with_origin<W: Write>(
    request: &AgentRequest,
    origin: AgentRunOrigin,
    intent: AgentStartIntent,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    selectable_after_event_index: Option<usize>,
) -> std::io::Result<()> {
    let disposition = start_agent_run_with_origin_disposition(
        request,
        origin,
        intent,
        adapter,
        state,
        output,
        selectable_after_event_index,
    )?;
    // A rejected *user* request must never vanish silently. Even callers that
    // discard the disposition through this wrapper get a visible queue-full
    // notice so the user knows to retry; internal best-effort work is dropped
    // quietly. Callers that also consumed durable state (dedup markers, pending
    // cards) before starting must additionally roll it back — see
    // `start_agent_run_with_origin_disposition`.
    if intent == AgentStartIntent::UserInitiated && disposition == AgentStartDisposition::QueueFull
    {
        crate::slash::session::render_agent_queue_full_notice(state, output)?;
        crate::slash::prompt::write_shell_prompt(state, output)?;
        output.flush()?;
    }
    Ok(())
}

/// Starts an Agent run and reports whether it started, queued, was suppressed
/// by a background compaction, or was rejected because the queue is full.
/// Callers that mutate durable state before starting (analysis dedup markers,
/// pending cards, etc.) must use this and undo their bookkeeping on any
/// non-accepted disposition — both [`AgentStartDisposition::SuppressedByCompaction`]
/// and [`AgentStartDisposition::QueueFull`] mean the request did not run and
/// was not queued.
pub(crate) fn start_agent_run_with_origin_disposition<W: Write>(
    request: &AgentRequest,
    origin: AgentRunOrigin,
    intent: AgentStartIntent,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    selectable_after_event_index: Option<usize>,
) -> std::io::Result<AgentStartDisposition> {
    start_agent_run_with_queue_policy(
        request,
        origin,
        intent,
        PendingRequestClass::Normal,
        adapter,
        state,
        output,
        selectable_after_event_index,
        false,
    )
}

/// Starts a control-protocol response (a question answer or approval
/// resolution) whose pending card state has already been consumed.
///
/// Callers own the delivery plan: when it shows the response would actually
/// be *enqueued* (rather than delivered directly to the active provider
/// owner or started immediately), they must check
/// [`crate::agent::queue::control_queue_has_capacity`] BEFORE consuming the
/// card state — see the `*_needs_queue_slot` predicates in the
/// question/approval runtimes. On that contract the returned disposition
/// cannot be [`AgentStartDisposition::QueueFull`]; it is still returned
/// (rather than swallowed) so callers can assert it.
pub(crate) fn start_agent_run_control_response<W: Write>(
    request: &AgentRequest,
    origin: AgentRunOrigin,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    selectable_after_event_index: Option<usize>,
) -> std::io::Result<AgentStartDisposition> {
    start_agent_run_with_queue_policy(
        request,
        origin,
        AgentStartIntent::UserInitiated,
        PendingRequestClass::ControlResponse,
        adapter,
        state,
        output,
        selectable_after_event_index,
        false,
    )
}

/// Restarts a previously queued request, preserving its origin, intent,
/// admission class, and held-text ordering.
///
/// Every dequeue path (post-run FIFO, post-compaction resume) must use this
/// instead of the plain wrappers so a re-queue — e.g. when a new compaction
/// was recommended while the request waited — keeps a control response in
/// [`PendingRequestClass::ControlResponse`] rather than silently downgrading
/// it to a droppable normal request.
pub(crate) fn start_pending_agent_run<W: Write>(
    pending: PendingAgentRequest,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<AgentStartDisposition> {
    start_agent_run_with_queue_policy(
        &pending.request,
        pending.origin,
        pending.intent,
        pending.class,
        adapter,
        state,
        output,
        pending.selectable_after_event_index,
        pending.before_held_text,
    )
}

#[allow(clippy::too_many_arguments)]
fn start_agent_run_with_queue_policy<W: Write>(
    request: &AgentRequest,
    origin: AgentRunOrigin,
    intent: AgentStartIntent,
    class: PendingRequestClass,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    selectable_after_event_index: Option<usize>,
    before_held_text: bool,
) -> std::io::Result<AgentStartDisposition> {
    // Central compaction gate. Every start path funnels through here: the
    // natural-language intercept, auto failure analysis, hook consultation,
    // evidence continuations, recovery/handoff fallbacks, question answers,
    // approval resumptions, and queued requests. A background compaction —
    // running now, or recommended and about to start at the next idle
    // boundary — pauses the Agent conversation, so enforcing the pause at this
    // single boundary guarantees no model process is launched against a
    // transcript the compactor is about to rewrite, and (critically) that no
    // internal continuation keeps `agent_run.active` set and starves the
    // recommended compaction.
    //
    // The gate itself NEVER spawns the compactor; that stays with the
    // idle-boundary polling path. It only decides the disposition:
    //   - InternalBestEffort → dropped (its captured context is exactly what
    //     compaction discards; replaying it later would run against a
    //     different context).
    //   - UserInitiated → queued, so the user's request is never lost and
    //     resumes in FIFO order once compaction completes.
    if crate::slash::session::compaction_pending_or_active(state) {
        match intent {
            AgentStartIntent::InternalBestEffort => {
                tracing::debug!(
                    origin = ?origin,
                    "internal agent run suppressed: session compaction pending or active"
                );
                return Ok(AgentStartDisposition::SuppressedByCompaction);
            }
            AgentStartIntent::UserInitiated => {
                return Ok(enqueue(
                    state,
                    PendingAgentRequest {
                        request: request.clone(),
                        origin,
                        intent,
                        class,
                        selectable_after_event_index,
                        before_held_text,
                    },
                ));
            }
        }
    }

    if state.agent_run.active.is_some() {
        return Ok(enqueue(
            state,
            PendingAgentRequest {
                request: request.clone(),
                origin,
                intent,
                class,
                selectable_after_event_index,
                before_held_text,
            },
        ));
    }

    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let markdown_stream = renderer.stream_markdown_agent();
    let mut status_animation = renderer.status_animation();
    if status_animation.is_enabled() {
        status_animation.render(output, state.i18n().t(MessageId::AgentThinking))?;
    } else {
        renderer.write_loading_text(output, state.i18n().t(MessageId::AgentThinking))?;
    }
    output.flush()?;

    let mut request = request.clone();
    state.startup_health.poll_ready();
    attach_continuity_prompt_hint(&mut request, state);
    finalize_agent_request_skill_context(&mut request, state.startup_health.report.as_ref());
    enforce_insight_context_budget(&mut request);
    annotate_continuation_user_approval_mode(&mut request, state.approval_mode);
    let provider_mode = provider_mode_for_agent_run(&request, state.approval_mode);
    let handle = adapter.start_cancellable(request.clone(), provider_mode);
    record_started_agent_request(state, &request);
    let now = Instant::now();
    let i18n = state.i18n();
    state.agent_run.host_executed_shell_result_delivered = false;
    state.shell_evidence.clear_recent_shell_tool_outputs();
    state.agent_run.active = Some(ActiveAgentRun {
        request,
        origin,
        handle,
        provider_name: adapter.name(),
        language: state.language,
        renderer,
        status_animation,
        markdown_stream,
        governed_events: Vec::new(),
        deferred_events: Vec::new(),
        held_events: Vec::new(),
        cosh_request_filter: CoshRequestStreamFilter::default(),
        pending_cosh_requests: Vec::new(),
        pending_cosh_request_audits: Vec::new(),
        pending_hook_notifications: Vec::new(),
        rendered_governed_event_count: 0,
        selectable_after_event_index,
        started_at: now,
        last_activity_at: now,
        last_heartbeat_at: now,
        current_phase: i18n.t(MessageId::AgentStatusStarting).to_string(),
        current_message: i18n.t(MessageId::AgentStatusWaitingBackend).to_string(),
        has_visible_text_delta: false,
        completed: false,
        host_completed_tool_ids: Vec::new(),
    });
    poll_active_agent_run(state, output, adapter)?;
    Ok(AgentStartDisposition::Started)
}

fn attach_continuity_prompt_hint(request: &mut AgentRequest, state: &InlineState) {
    let Some(input) = request.user_input.as_deref() else {
        return;
    };
    let Some(hint) = continuity_prompt_hint(state, input) else {
        return;
    };
    if !request
        .context_hints
        .iter()
        .any(|existing| existing == &hint)
    {
        request.context_hints.push(hint);
    }
}

fn enforce_insight_context_budget(request: &mut AgentRequest) {
    if !request
        .context_hints
        .iter()
        .any(|hint| hint.starts_with("insight_evidence\n"))
    {
        return;
    }

    while serialized_context_hint_bytes(&request.context_hints)
        > crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES
    {
        if let Some(index) = request
            .context_hints
            .iter()
            .rposition(|hint| !hint.starts_with("insight_evidence\n"))
        {
            request.context_hints.remove(index);
            continue;
        }
        if request.context_hints.len() > 1 {
            request.context_hints.pop();
            continue;
        }
        let hint = &mut request.context_hints[0];
        let mut end = crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES.min(hint.len());
        while !hint.is_char_boundary(end) {
            end -= 1;
        }
        hint.truncate(end);
    }
}

fn serialized_context_hint_bytes(hints: &[String]) -> usize {
    hints.iter().map(String::len).sum::<usize>() + hints.len().saturating_sub(1)
}

pub(crate) fn stop_active_agent_run_without_rendering<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    state.agent_run.held_events.clear();
    let Some(mut active_run) = state.agent_run.active.take() else {
        return Ok(());
    };
    // Turn-scope batch consent never outlives its run (issue #1773).
    state.control.trust.clear_run_batch_consent();

    // #1940 run-terminal sweep before the handle is cancelled, while the
    // response channel is still usable.
    crate::approval::runtime::drain_unhomed_control_requests_with_handle(
        state,
        &active_run.request.id,
        &active_run.handle,
    );
    active_run.handle.cancel();
    active_run.status_animation.clear(output)?;
    // Surface unconsumed hook notifications before teardown; dropping them
    // with the run is how block decisions went missing (#2067).
    drain_pending_hook_notifications(&mut active_run, output)?;
    active_run.held_events.clear();
    active_run.deferred_events.clear();
    active_run.cosh_request_filter.clear();
    active_run.pending_cosh_requests.clear();
    active_run.pending_cosh_request_audits.clear();
    output.flush()?;
    Ok(())
}

/// Takes the run's unconsumed hook notifications as governable events.
pub(crate) fn take_pending_hook_notification_events(
    active_run: &mut ActiveAgentRun,
) -> Vec<AgentEvent> {
    let run_id = active_run.request.id.clone();
    active_run
        .pending_hook_notifications
        .drain(..)
        .map(|notification| AgentEvent::HookNotification {
            run_id: run_id.clone(),
            hook_name: notification.hook_name,
            message: notification.message,
            tool_use_id: notification.tool_use_id,
            decision: notification.decision,
        })
        .collect()
}

/// Drains unconsumed hook notifications through governance and renders them
/// with the guarded writer. Teardown paths must surface an orphan block/ask
/// decision instead of dropping it with the run (#2067).
pub(crate) fn drain_pending_hook_notifications<W: Write>(
    active_run: &mut ActiveAgentRun,
    output: &mut W,
) -> std::io::Result<()> {
    let events = take_pending_hook_notification_events(active_run);
    if events.is_empty() {
        return Ok(());
    }
    let governed =
        govern_agent_events_with_language(&events, &Policy::default(), active_run.language).events;
    active_run
        .renderer
        .write_governed_events(output, &governed)?;
    active_run.governed_events.extend(governed);
    Ok(())
}

pub(super) fn has_queued_run_before_held_text(state: &InlineState) -> bool {
    state
        .agent_run
        .queued_requests
        .iter()
        .any(|pending| pending.before_held_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skill_context::finalize_agent_request_skill_context;
    use crate::diagnostics::health::{
        HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue, HealthScanReport,
        HealthSeverity,
    };
    use crate::types::STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT;

    #[test]
    fn only_insight_origins_use_strict_result_presentation() {
        assert!(!AgentRunOrigin::Standard.is_insight_triggered());
        assert!(AgentRunOrigin::InsightPrompt.is_insight_triggered());
        assert!(AgentRunOrigin::AutoFailure.is_insight_triggered());
    }

    #[test]
    fn final_insight_context_never_exceeds_provider_budget() {
        let mut request = test_agent_request();
        request.context_hints = vec![
            "x".repeat(crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES),
            "insight_evidence\ntarget_facts:\ncommand_id=cmd-1".to_string(),
        ];

        finalize_agent_request_skill_context(&mut request, None);
        enforce_insight_context_budget(&mut request);

        assert!(
            serialized_context_hint_bytes(&request.context_hints)
                <= crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES
        );
        assert_eq!(request.context_hints.len(), 1);
        assert!(request.context_hints[0].starts_with("insight_evidence\n"));
    }

    #[test]
    fn malformed_oversized_insight_payload_is_utf8_safely_bounded() {
        let mut request = test_agent_request();
        request.context_hints = vec![format!(
            "insight_evidence\n{}",
            "界".repeat(crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES)
        )];

        enforce_insight_context_budget(&mut request);

        assert!(
            serialized_context_hint_bytes(&request.context_hints)
                <= crate::insight::evidence::PROVIDER_CONTEXT_MAX_BYTES
        );
        assert!(request.context_hints[0].is_char_boundary(request.context_hints[0].len()));
    }

    #[test]
    fn health_context_hint_is_not_attached_to_free_form_request() {
        let report = test_health_report();
        let mut request = test_agent_request();

        finalize_agent_request_skill_context(&mut request, Some(&report));

        assert!(!request
            .context_hints
            .iter()
            .any(|hint| hint.starts_with("health_scan ")));
    }

    #[test]
    fn health_context_hint_is_attached_to_startup_health_follow_up() {
        let report = test_health_report();
        let mut request = test_agent_request();
        request
            .context_hints
            .push(STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT.to_string());

        finalize_agent_request_skill_context(&mut request, Some(&report));

        let hint = request
            .context_hints
            .iter()
            .find(|hint| hint.starts_with("health_scan "))
            .expect("health context hint");
        assert!(hint.contains("scan_id=health-1"), "{hint}");
        assert!(hint.contains("overall_severity=warning"), "{hint}");
        assert!(hint.contains("bounded_facts_only=true"), "{hint}");
        assert!(hint.contains("no_collector_stdout=true"), "{hint}");
        assert!(!hint.contains("/tmp/cosh"), "{hint}");
    }

    #[test]
    fn health_context_hint_dedupes_existing_health_hint() {
        let report = test_health_report();
        let mut request = test_agent_request();
        request
            .context_hints
            .push(STARTUP_HEALTH_FOLLOW_UP_BINDING_HINT.to_string());
        request
            .context_hints
            .push("health_scan scan_id=existing".to_string());

        finalize_agent_request_skill_context(&mut request, Some(&report));

        assert_eq!(
            request
                .context_hints
                .iter()
                .filter(|hint| hint.starts_with("health_scan "))
                .count(),
            1
        );
    }

    fn test_health_report() -> HealthScanReport {
        let mut report = HealthScanReport::new("health-1", 0);
        report.overall_severity = HealthSeverity::Warning;
        report.facts.push(HealthFact {
            id: "memory.available_ratio".to_string(),
            category: HealthFactCategory::Memory,
            key: "memory.available_ratio".to_string(),
            value: HealthFactValue::Float(0.08),
            unit: None,
            source: HealthFactSource::Fixture,
            elapsed_ms: 0,
        });
        report
    }

    fn test_agent_request() -> AgentRequest {
        AgentRequest {
            id: "agent-request-health".to_string(),
            session_id: "session-1".to_string(),
            command_block: CommandBlock {
                id: "cmd-1".to_string(),
                session_id: "session-1".to_string(),
                command: "分析一下这台机器内存风险".to_string(),
                origin: CommandOrigin::UserInteractive,
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
            user_input: Some("分析一下这台机器内存风险".to_string()),
            findings: Vec::new(),
            mode: AgentMode::RecommendOnly,
            user_confirmed: true,
            hook_finding: None,
            recommended_skill: None,
        }
    }
}
