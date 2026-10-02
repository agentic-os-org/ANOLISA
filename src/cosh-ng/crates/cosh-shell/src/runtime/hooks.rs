use std::io::Write;

use super::prelude::{
    agent_request_after_confirmation, build_related_history_index, context_blocks_from_entries,
    findings_from_blocks, AdapterInstance, AgentContextBinding, AgentMode, CommandBlock,
    CommandOrigin, FindingSeverity, MessageId, NoticePanelModel, RatatuiInlineRenderer,
    RelatedHistoryConfig, ShellEvent, ShellEventKind,
};
#[cfg(test)]
use super::prelude::{
    default_builtin_hooks, AgentRequest, CommandStatus, FakeAgentAdapter, HookEngine, Language,
    OutputRefs,
};
#[cfg(test)]
use crate::adapter::prompt_from_request;
use crate::agent::{
    failed_command::{
        start_agent_for_block, FailedCommandAgentStartOptions, FailedCommandAnalysisTrigger,
    },
    run::{start_agent_run, AgentStartIntent},
};
use crate::hooks::aggregate::{
    apply_memory_pressure_severity_upgrade, combined_hook_finding,
    computed_suppression_key_with_origin, severity_rank, AggregatedHookFinding,
};
#[cfg(test)]
use crate::hooks::aggregate::{entity_key, suppression_key};
use crate::hooks::detector::{aggregate_hook_findings, refresh_aggregate_metadata};
#[cfg(test)]
use crate::hooks::feedback::{
    apply_session_interruption_policy, decide_session_interruption_policy,
    decide_session_interruption_policy_with_context,
};
use crate::hooks::feedback::{
    decide_session_interruption_policy_with_origin, display_for_aggregate,
};
use crate::hooks::interrupt::command_should_skip_failure_analysis;
#[cfg(test)]
use crate::hooks::policy::{classify_command_intent, CommandIntent};
use crate::hooks::presentation::render_consultation_details;
use crate::hooks::prompt::{
    finding_markdown_for_aggregate, format_runtime_hint, hook_analysis_user_input,
    prompt_hint_for_finding,
};
use crate::hooks::queue::{
    consultation_from_hint, record_hook_display_event_for_consultation,
    render_next_queued_consultation, render_or_queue_consultation, INTERRUPTION_BUDGET_WINDOW_MS,
};
#[cfg(test)]
use crate::hooks::queue::{
    queued_consultation_decision, topic_budget_key, QueuedConsultationDecision,
    PENDING_CONSULTATION_TTL_MS, SUCCESS_CONSULTATION_IDLE_GRACE,
};
#[cfg(test)]
use crate::hooks::state::InterruptionBudgetRecord;
#[cfg(test)]
use crate::hooks::state::{hook_feedback_group_key, HookFeedback};
#[cfg(test)]
use crate::hooks::state::{HookRuntimeState, HookSuppressionRecord};
use crate::hooks::state::{
    PendingConsultation, PendingConsultationState, RuntimeHookDisplay, RuntimeHookDisplayAction,
    RuntimeHookDisplayEvent, RuntimeHookFinding,
};
use crate::insight::memory::{
    adapt_memory_aggregate, claims_memory_aggregate, MemoryAggregateView, MemoryInsightOutcome,
};
use crate::insight::model::InterventionDecision;
use crate::insight::policy::{
    decide_candidate_intervention, AnalysisPolicyMode, InterventionGates,
};
use crate::runtime::controller::{pending_card_capture, shell_has_active_foreground_command};
use crate::runtime::state::{AnalysisMode, InlineState};
#[cfg(test)]
use crate::types::HookFinding;

const MAX_HOOK_FINDINGS: usize = 32;

#[cfg(test)]
pub(crate) fn record_command_hook_findings(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
    event_index_base: usize,
) {
    record_command_hook_findings_with_history_base(events, blocks, state, 0, event_index_base);
}

pub(crate) fn record_command_hook_findings_with_history_base(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
    history_index_base: usize,
    event_index_base: usize,
) {
    for block in blocks {
        if !state.hooks.handled_command_hooks.insert(block.id.clone()) {
            continue;
        }
        if command_should_skip_failure_analysis(events, block) {
            continue;
        }
        let origin = command_origin_for_block(events, block);

        let findings = state.hooks.engine.evaluate_with_disabled_and_origin(
            block,
            &state.hooks.disabled,
            origin,
        );
        let user_has_not_continued = !state.hooks.block_followed_by_user_input(&block.id);
        let gates = InterventionGates {
            same_dispatch_batch: command_end_event_index(events, block)
                .map(|index| history_index_base.saturating_add(index))
                .is_some_and(|idx| idx >= event_index_base),
            input_empty: user_has_not_continued,
            foreground_idle: !shell_has_active_foreground_command(events),
            active_runtime_idle: state.agent_run.active.is_none()
                && pending_card_capture(state).is_none(),
            user_has_not_continued,
            user_interactive_origin: origin == CommandOrigin::UserInteractive,
            budget_available: true,
        };
        for aggregate in aggregate_hook_findings(findings) {
            record_aggregated_hook_finding_with_origin(block, aggregate, origin, gates, state);
        }
    }

    if state.hooks.findings.len() > MAX_HOOK_FINDINGS {
        let drop_count = state.hooks.findings.len() - MAX_HOOK_FINDINGS;
        state.hooks.findings.drain(0..drop_count);
    }
}

pub(crate) fn command_origin_for_block(
    _events: &[ShellEvent],
    block: &CommandBlock,
) -> CommandOrigin {
    block.origin
}

pub(crate) fn record_blocks_followed_by_user_input(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
) {
    for block in blocks {
        let Some(end_index) = command_end_event_index(events, block) else {
            continue;
        };
        if events
            .iter()
            .skip(end_index + 1)
            .any(|event| is_followup_user_input_event(event, &block.id))
        {
            state
                .hooks
                .mark_block_followed_by_user_input(block.id.clone());
        }
    }
}

fn command_end_event_index(events: &[ShellEvent], block: &CommandBlock) -> Option<usize> {
    events.iter().position(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
        ) && event.command_id.as_deref() == Some(block.id.as_str())
    })
}

fn is_followup_user_input_event(event: &ShellEvent, block_id: &str) -> bool {
    match event.kind {
        ShellEventKind::CommandStarted => event.command_id.as_deref() != Some(block_id),
        ShellEventKind::UserInputIntercepted => true,
        _ => false,
    }
}

pub(crate) fn render_recorded_hook_findings<W: Write>(
    blocks: &[CommandBlock],
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let block_ids = blocks
        .iter()
        .map(|block| block.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let renderer = RatatuiInlineRenderer::for_terminal();

    let hints = state.hooks.findings.clone();
    for hint in hints {
        if !block_ids.contains(hint.command_block_id.as_str())
            || !state.hooks.rendered_findings.insert(hint.id.clone())
        {
            continue;
        }

        match hint.display {
            RuntimeHookDisplay::Silent => {}
            RuntimeHookDisplay::Hint => {
                if state.analysis_mode != AnalysisMode::Manual {
                    let Some(markdown) = hint.finding_markdown.as_deref() else {
                        continue;
                    };
                    let i18n = state.i18n();
                    let footer = i18n.format(
                        MessageId::HookFindingFooter,
                        &[("hint_id", hint.id.as_str())],
                    );
                    renderer.write_notice_panel(
                        output,
                        NoticePanelModel {
                            title: i18n.t(MessageId::HookFindingTitle),
                            body: renderer.markdown_text_lines(markdown),
                            footer: Some(&footer),
                        },
                    )?;
                    record_hook_display_event_for_hint(
                        &hint,
                        RuntimeHookDisplayAction::Shown,
                        state,
                    );
                }
            }
            RuntimeHookDisplay::Consultation => {
                if state.analysis_mode != AnalysisMode::Manual {
                    let active_agent_run = state.agent_run.active.is_some();
                    render_or_queue_consultation(
                        &hint,
                        &mut state.hooks,
                        active_agent_run,
                        output,
                    )?;
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn render_queued_hook_consultation<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let active_agent_run = state.agent_run.active.is_some();
    render_next_queued_consultation(&mut state.hooks, active_agent_run, state.language, output)
}

pub(crate) fn hook_routing_hints_for_block(
    state: &InlineState,
    block: &CommandBlock,
) -> Vec<String> {
    state
        .hooks
        .findings
        .iter()
        .filter(|hint| hint.command_block_id == block.id)
        .map(format_runtime_hint)
        .collect()
}

pub(crate) fn handle_consultation_events<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let consultation = match state.hooks.pending_consultation.take() {
        Some(c) => c,
        None => return Ok(()),
    };

    for event in events {
        if event.kind != ShellEventKind::UserInputIntercepted {
            continue;
        }
        if event.component.as_deref() != Some("card") {
            continue;
        }
        let event_id = event.input.as_deref().unwrap_or("");
        if !event_id.contains(&consultation.card_id) {
            continue;
        }
        let action = event.message.as_deref().unwrap_or("");
        if action == "approve" {
            let mut consultation = consultation;
            consultation.state = PendingConsultationState::Analyzed;
            let block = blocks.iter().find(|b| b.id == consultation.block_id);
            if let Some(block) = block {
                if consultation.hook_finding.is_some() {
                    record_hook_display_event_for_consultation(
                        &consultation,
                        RuntimeHookDisplayAction::Analyzed,
                        &mut state.hooks,
                    );
                    start_agent_for_hook_consultation(
                        block,
                        blocks,
                        &consultation,
                        adapter,
                        state,
                        output,
                    )?;
                } else {
                    let findings = findings_from_blocks(blocks);
                    start_agent_for_block(
                        block,
                        blocks,
                        &findings,
                        adapter,
                        state,
                        output,
                        FailedCommandAgentStartOptions {
                            selectable_after_event_index: None,
                            trigger: FailedCommandAnalysisTrigger::UserConfirmed,
                        },
                    )?;
                }
            }
            let active_agent_run = state.agent_run.active.is_some();
            render_next_queued_consultation(
                &mut state.hooks,
                active_agent_run,
                state.language,
                output,
            )?;
            return Ok(());
        } else if action == "details" {
            render_consultation_details(&consultation, state.i18n(), state.debug, output)?;
            state.hooks.pending_consultation = Some(consultation);
            return Ok(());
        } else if action == "cancel" || action == "deny" {
            let mut consultation = consultation;
            consultation.state = PendingConsultationState::Ignored;
            state
                .hooks
                .ignored_cards
                .insert(consultation.suppression_key.clone());
            record_hook_display_event_for_consultation(
                &consultation,
                RuntimeHookDisplayAction::Ignored,
                &mut state.hooks,
            );
            let active_agent_run = state.agent_run.active.is_some();
            render_next_queued_consultation(
                &mut state.hooks,
                active_agent_run,
                state.language,
                output,
            )?;
            return Ok(());
        }
    }

    state.hooks.pending_consultation = Some(consultation);
    Ok(())
}

#[cfg(test)]
fn record_aggregated_hook_finding(
    block: &CommandBlock,
    aggregate: AggregatedHookFinding,
    state: &mut InlineState,
) {
    record_aggregated_hook_finding_with_origin(
        block,
        aggregate,
        CommandOrigin::UserInteractive,
        InterventionGates::eligible(),
        state,
    );
}

fn record_aggregated_hook_finding_with_origin(
    block: &CommandBlock,
    mut aggregate: AggregatedHookFinding,
    origin: CommandOrigin,
    mut gates: InterventionGates,
    state: &mut InlineState,
) {
    if claims_memory_aggregate(&aggregate.provenance) {
        if origin != CommandOrigin::UserInteractive {
            return;
        }
        apply_memory_pressure_severity_upgrade(&mut aggregate);
        refresh_aggregate_metadata(block, &mut aggregate);
        let memory_view = MemoryAggregateView::new_with_facts(
            &aggregate.provenance,
            &aggregate.primary,
            &aggregate.related,
            &aggregate.builtin_facts,
        );
        match adapt_memory_aggregate(block, memory_view, &mut state.insight_correlation) {
            MemoryInsightOutcome::Claimed(candidate) => {
                if let Some(candidate) = candidate.map(|candidate| *candidate) {
                    gates.budget_available = !state.insight_budget.is_suppressed(
                        &candidate.suppression_key,
                        candidate.severity,
                        block.ended_at_ms,
                    );
                    if state.pending_command_insight.is_none()
                        && matches!(
                            decide_candidate_intervention(
                                &candidate,
                                analysis_policy_mode(state.analysis_mode),
                                gates,
                                false,
                            ),
                            InterventionDecision::Suggest { .. }
                        )
                    {
                        state.pending_command_insight = Some(candidate);
                    }
                }
                return;
            }
            MemoryInsightOutcome::ClaimedError(reason) => {
                tracing::debug!(
                    target: "cosh_insight",
                    command_block_id = %block.id,
                    reason,
                    "builtin memory insight adapter failed"
                );
                return;
            }
            MemoryInsightOutcome::NotClaimed => {
                tracing::debug!(
                    target: "cosh_insight",
                    command_block_id = %block.id,
                    reason = "claimed-provenance-rejected",
                    "builtin memory insight adapter failed"
                );
                return;
            }
        }
    }

    attach_recent_memory_pressure(block, &mut aggregate, state);
    apply_memory_pressure_severity_upgrade(&mut aggregate);
    refresh_aggregate_metadata(block, &mut aggregate);
    aggregate.suppression_key = computed_suppression_key_with_origin(block, &aggregate, origin);
    let base_display = display_for_aggregate(
        block,
        &aggregate,
        state.analysis_mode == AnalysisMode::Manual,
    );
    let decision = decide_session_interruption_policy_with_origin(
        block,
        &aggregate,
        base_display,
        &aggregate.suppression_key,
        origin,
        &state.hooks,
        state.agent_run.active.is_some(),
    );
    let recommended_skill = aggregate.recommended_skill.clone();
    let prompt_hint = prompt_hint_for_finding(block, &aggregate, recommended_skill.as_deref());
    let finding_markdown = finding_markdown_for_aggregate(block, &aggregate, state.i18n());
    let hook_id = aggregate.primary.hook_id.clone();
    let related_hook_ids = aggregate
        .related
        .iter()
        .map(|finding| finding.hook_id.clone())
        .collect::<Vec<_>>();
    state.hooks.findings.push(RuntimeHookFinding {
        id: format!("hook-{}-{hook_id}", block.id),
        command_block_id: block.id.clone(),
        command: block.command.clone(),
        output_ref: block.output.terminal_output_ref.clone(),
        ended_at_ms: block.ended_at_ms,
        prompt_hint,
        finding_markdown: Some(finding_markdown),
        hook_finding: Some(combined_hook_finding(
            aggregate.primary.clone(),
            &aggregate.related,
        )),
        recommended_skill,
        display: decision.display,
        display_reason: decision.reason.to_string(),
        related_hook_ids,
        topic: aggregate.topic,
        entity_key: aggregate.entity_key,
        effective_severity: aggregate.effective_severity,
        confidence: aggregate.confidence,
        suppression_key: aggregate.suppression_key,
    });
    if decision.reason == "muted" {
        if let Some(hint) = state.hooks.findings.last().cloned() {
            record_hook_display_event_for_hint(&hint, RuntimeHookDisplayAction::Muted, state);
        }
    }
}

fn analysis_policy_mode(mode: AnalysisMode) -> AnalysisPolicyMode {
    match mode {
        AnalysisMode::Smart => AnalysisPolicyMode::Smart,
        AnalysisMode::Auto => AnalysisPolicyMode::Auto,
        AnalysisMode::Manual => AnalysisPolicyMode::Manual,
    }
}

fn record_hook_display_event_for_hint(
    hint: &RuntimeHookFinding,
    action: RuntimeHookDisplayAction,
    state: &mut InlineState,
) {
    let hook_id = hint
        .hook_finding
        .as_ref()
        .map(|finding| finding.hook_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    record_hook_display_event(
        RuntimeHookDisplayEvent {
            action,
            finding_id: hint.id.clone(),
            command_block_id: hint.command_block_id.clone(),
            hook_id,
            topic: hint.topic.clone(),
            entity_key: hint.entity_key.clone(),
            suppression_key: hint.suppression_key.clone(),
            display: hint.display,
            display_reason: hint.display_reason.clone(),
            confidence: hint.confidence.clone(),
            ended_at_ms: hint.ended_at_ms,
        },
        state,
    );
}

fn record_hook_display_event(event: RuntimeHookDisplayEvent, state: &mut InlineState) {
    state.hooks.record_display_event(event);
}

pub(crate) fn handle_command_hook_hint_action<W: Write>(
    action: &str,
    hint_id: &str,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let Some(hint) = state
        .hooks
        .findings
        .iter()
        .find(|hint| hint.id == hint_id)
        .cloned()
    else {
        return RatatuiInlineRenderer::for_terminal().write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::HookHintTitle),
                body: vec![i18n.format(MessageId::HookHintNotFoundBody, &[("hint_id", hint_id)])],
                footer: Some(i18n.t(MessageId::HookHintNotFoundFooter)),
            },
        );
    };
    let Some(consultation) = consultation_from_hint(&hint) else {
        return RatatuiInlineRenderer::for_terminal().write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::HookHintTitle),
                body: vec![i18n.format(MessageId::HookHintNoFindingBody, &[("hint_id", hint_id)])],
                footer: None,
            },
        );
    };

    match action {
        "analyze" => {
            let Some(block) = blocks
                .iter()
                .find(|block| block.id == consultation.block_id)
            else {
                return RatatuiInlineRenderer::for_terminal().write_notice_panel(
                    output,
                    NoticePanelModel {
                        title: i18n.t(MessageId::HookHintTitle),
                        body: vec![i18n.format(
                            MessageId::HookHintBlockUnavailableBody,
                            &[("block_id", consultation.block_id.as_str())],
                        )],
                        footer: None,
                    },
                );
            };
            record_hook_display_event_for_consultation(
                &consultation,
                RuntimeHookDisplayAction::Analyzed,
                &mut state.hooks,
            );
            start_agent_for_hook_consultation(block, blocks, &consultation, adapter, state, output)
        }
        "details" => render_consultation_details(&consultation, state.i18n(), state.debug, output),
        "ignore" => {
            state
                .hooks
                .ignored_cards
                .insert(consultation.suppression_key.clone());
            record_hook_display_event_for_consultation(
                &consultation,
                RuntimeHookDisplayAction::Ignored,
                &mut state.hooks,
            );
            RatatuiInlineRenderer::for_terminal().write_notice_panel(
                output,
                NoticePanelModel {
                    title: i18n.t(MessageId::HookHintIgnoredTitle),
                    body: vec![i18n.format(
                        MessageId::HookHintIgnoredBody,
                        &[("hint_id", hint_id)],
                    )],
                    footer: Some(i18n.t(MessageId::HookHintIgnoredFooter)),
                },
            )
        }
        _ => RatatuiInlineRenderer::for_terminal().write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::HookHintUsageTitle),
                body: vec![i18n.t(MessageId::HookHintUsageBody).to_string()],
                footer: None,
            },
        ),
    }
}

pub(crate) fn start_agent_for_hook_consultation<W: Write>(
    block: &CommandBlock,
    blocks: &[CommandBlock],
    consultation: &PendingConsultation,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let findings = findings_from_blocks(blocks);
    let Some(mut request) =
        agent_request_after_confirmation(&block.session_id, block, &findings, true)
    else {
        return Ok(());
    };

    let ctx_config = RelatedHistoryConfig {
        related_command_ids: related_command_ids_for_consultation(state, consultation),
        ..Default::default()
    };
    let ctx_entries = build_related_history_index(blocks, block, &ctx_config);
    request.id = format!("agent-request-{}", consultation.card_id);
    request.context_blocks = context_blocks_from_entries(&ctx_entries);
    request.context_hints = if consultation.context_hints.is_empty() {
        hook_routing_hints_for_block(state, block)
    } else {
        consultation.context_hints.clone()
    };
    request.user_input = Some(hook_analysis_user_input(block, consultation));
    request.mode = AgentMode::RecommendOnly;
    request.user_confirmed = true;
    crate::types::set_request_context_binding(&mut request, AgentContextBinding::HookConsultation);
    request.hook_finding = consultation.hook_finding.clone();
    request.recommended_skill = consultation.recommended_skill.clone();
    state.agent_run.needs_prompt_after_run = true;
    // Hook consultation is an internal best-effort continuation.
    start_agent_run(
        &request,
        AgentStartIntent::InternalBestEffort,
        adapter,
        state,
        output,
        None,
    )
}

fn related_command_ids_for_consultation(
    state: &InlineState,
    consultation: &PendingConsultation,
) -> Vec<String> {
    let Some(hint) = state
        .hooks
        .findings
        .iter()
        .find(|hint| hint.id == consultation.finding_id)
    else {
        return Vec::new();
    };

    hint.related_hook_ids
        .iter()
        .filter_map(|related_hook_id| {
            state
                .hooks
                .findings
                .iter()
                .rev()
                .find(|candidate| {
                    candidate.command_block_id != consultation.block_id
                        && candidate.ended_at_ms <= consultation.ended_at_ms
                        && candidate
                            .hook_finding
                            .as_ref()
                            .map(|finding| finding.hook_id == *related_hook_id)
                            .unwrap_or(false)
                })
                .map(|candidate| candidate.command_block_id.clone())
        })
        .collect()
}

fn attach_recent_memory_pressure(
    block: &CommandBlock,
    aggregate: &mut AggregatedHookFinding,
    state: &InlineState,
) {
    if aggregate.primary.hook_id != "high-memory-process"
        || aggregate
            .related
            .iter()
            .any(|finding| finding.hook_id == "memory-pressure")
    {
        return;
    }
    let Some(pressure) = state.hooks.findings.iter().rev().find(|hint| {
        hint.command_block_id != block.id
            && hint.ended_at_ms <= block.ended_at_ms
            && block.ended_at_ms.saturating_sub(hint.ended_at_ms) <= INTERRUPTION_BUDGET_WINDOW_MS
            && hint.display != RuntimeHookDisplay::Silent
            && severity_rank(hint.effective_severity) >= severity_rank(FindingSeverity::Warning)
            && hint
                .hook_finding
                .as_ref()
                .map(|finding| finding.hook_id == "memory-pressure")
                .unwrap_or(false)
    }) else {
        return;
    };
    if let Some(finding) = pressure.hook_finding.clone() {
        aggregate.related.push(finding);
    }
}

#[cfg(test)]
#[path = "../hooks/runtime_tests.rs"]
mod tests;
