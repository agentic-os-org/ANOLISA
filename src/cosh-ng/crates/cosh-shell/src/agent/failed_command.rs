// Owner: agent. Candidate construction is split under failed_command/.
mod candidate;
mod routing;

use crate::hooks::interrupt::command_should_skip_failure_analysis;
use crate::runtime::prelude::*;
use candidate::command_not_found_agent_candidate;
use routing::has_proven_top_level_missing;

use crate::command::{classify_failure, FailureClass, FailureConfidence, FailureReason};
use crate::evidence::model::{EvidenceExcerpt, OutputExcerptDirection};
use crate::evidence::output_policy::{
    bounded_output_excerpt_for_block, bounded_output_head_tail_excerpt_for_block,
};
use crate::insight::evidence::{
    build_provider_evidence_payload, provider_target_facts, take_bound_insight_metadata,
    trim_optional_context_hints, EvidenceBundleInput,
};
use crate::insight::failed_command::{
    decide_failure_intervention, map_failure_semantics, FailureInsightKind,
};
use crate::insight::model::{
    InsightBinding, InsightCandidate, InsightConfidence, InsightEvidence, InsightSeverity,
    InsightSource, InsightTarget, InterventionDecision, OutputExcerptStatus, PromptSuggestion,
    SuppressionTopic,
};
use crate::insight::policy::{failure_suppression_key, AnalysisPolicyMode, InterventionGates};
use crate::insight::scope::resolve_execution_scope;
use crate::runtime::controller::{pending_card_capture, shell_has_active_foreground_command};

const FAILURE_OUTPUT_EXCERPT_MAX_BYTES: usize = 8192;
const FAILURE_OUTPUT_EXCERPT_MAX_LINES: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailedCommandAnalysisTrigger {
    Auto,
    UserConfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureAnalysisDisposition {
    SilentRecord,
    ActionCard,
    AutoAnalyze,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailedCommandAgentStartOptions {
    pub(crate) selectable_after_event_index: Option<usize>,
    pub(crate) trigger: FailedCommandAnalysisTrigger,
}

#[cfg(test)]
pub(crate) fn collect_failed_command_insights<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    collect_failed_command_insights_with_history_base(
        events,
        blocks,
        state,
        output,
        0,
        event_index_base,
    )
}

pub(crate) fn collect_failed_command_insights_with_history_base<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    state: &mut InlineState,
    _output: &mut W,
    history_index_base: usize,
    event_index_base: usize,
) -> std::io::Result<()> {
    for block in blocks {
        if state.analyzed_blocks.contains(&block.id) || state.canceled_blocks.contains(&block.id) {
            continue;
        }
        if state.evaluated_failed_command_insights.contains(&block.id) {
            continue;
        }
        let end_event_index = block_end_event_index(events, block)
            .map(|index| history_index_base.saturating_add(index));
        if end_event_index.is_none_or(|idx| idx < event_index_base) {
            continue;
        }
        state
            .evaluated_failed_command_insights
            .insert(block.id.clone());

        let excerpt = failure_output_evidence(block);
        let semantics = classify_failure(block, events, excerpt.text.as_deref());
        let proven_missing = block.exit_code == 127 && has_proven_top_level_missing(events, block);
        let rewrite = if state.analysis_mode != AnalysisMode::Manual && proven_missing {
            let diagnostic_tail = command_not_found_diagnostic_tail(block);
            state
                .shell_rewrite
                .resolve_for_block(block, diagnostic_tail.as_deref())
        } else {
            None
        };
        let candidate = rewrite
            .map(|text| shell_rewrite_candidate(block, text))
            .or_else(|| {
                (state.analysis_mode != AnalysisMode::Manual && proven_missing)
                    .then(|| command_not_found_agent_candidate(block, &excerpt))
            })
            .or_else(|| failed_command_candidate(events, block));
        let Some(candidate) = candidate else {
            continue;
        };
        let user_has_not_continued = !state.hooks.block_followed_by_user_input(&block.id);
        let gates = InterventionGates {
            same_dispatch_batch: end_event_index.is_some_and(|idx| idx >= event_index_base),
            input_empty: user_has_not_continued,
            foreground_idle: !shell_has_active_foreground_command(events),
            active_runtime_idle: state.agent_run.active.is_none()
                && pending_card_capture(state).is_none(),
            user_has_not_continued,
            user_interactive_origin: block.origin == CommandOrigin::UserInteractive,
            budget_available: !state.insight_budget.is_suppressed(
                &candidate.suppression_key,
                candidate.severity,
                block.ended_at_ms,
            ),
        };
        let Some(kind) = map_failure_semantics(&semantics) else {
            continue;
        };
        if !matches!(
            decide_failure_intervention(
                kind,
                semantics.confidence,
                semantics.auto_eligibility,
                failure_output_status(block, &excerpt).is_usable(excerpt.text.as_deref()),
                &candidate,
                analysis_policy_mode(state.analysis_mode),
                gates,
            ),
            InterventionDecision::Suggest { .. }
        ) {
            continue;
        }
        let replace = state
            .pending_command_insight
            .as_ref()
            .is_none_or(|pending| candidate.severity > pending.severity);
        if replace {
            state.pending_command_insight = Some(candidate);
        }
    }

    Ok(())
}

fn analysis_policy_mode(mode: AnalysisMode) -> AnalysisPolicyMode {
    match mode {
        AnalysisMode::Smart => AnalysisPolicyMode::Smart,
        AnalysisMode::Auto => AnalysisPolicyMode::Auto,
        AnalysisMode::Manual => AnalysisPolicyMode::Manual,
    }
}

pub(crate) fn failed_command_intervention(
    events: &[ShellEvent],
    block: &CommandBlock,
    candidate: &InsightCandidate,
    mode: AnalysisMode,
    gates: InterventionGates,
) -> InterventionDecision {
    let excerpt = failure_output_evidence(block);
    let semantics = classify_failure(block, events, excerpt.text.as_deref());
    let Some(kind) = map_failure_semantics(&semantics) else {
        return InterventionDecision::Silent;
    };
    decide_failure_intervention(
        kind,
        semantics.confidence,
        semantics.auto_eligibility,
        failure_output_status(block, &excerpt).is_usable(excerpt.text.as_deref()),
        candidate,
        analysis_policy_mode(mode),
        gates,
    )
}

fn shell_rewrite_candidate(block: &CommandBlock, text: String) -> InsightCandidate {
    let scope = resolve_execution_scope(&block.session_id, &block.command);
    let suppression_key = failure_suppression_key(
        SuppressionTopic::CommandNotFound,
        &block.command,
        scope.clone(),
    );
    InsightCandidate {
        source: InsightSource::FailedCommand,
        topic: SuppressionTopic::CommandNotFound,
        entity: suppression_key.entity.clone(),
        severity: InsightSeverity::Warning,
        confidence: InsightConfidence::High,
        evidence: Vec::new(),
        suggestion: Some(PromptSuggestion::ShellRewrite { text }),
        scope,
        suppression_key,
    }
}

pub(crate) fn failed_command_candidate(
    events: &[ShellEvent],
    block: &CommandBlock,
) -> Option<InsightCandidate> {
    let excerpt = failure_output_evidence(block);
    let semantics = classify_failure(block, events, excerpt.text.as_deref());
    let kind = map_failure_semantics(&semantics)?;
    if kind == FailureInsightKind::CommandNotFound {
        return None;
    }
    let scope = resolve_execution_scope(&block.session_id, &block.command);
    let topic = match kind {
        FailureInsightKind::PermissionDenied => SuppressionTopic::PermissionDenied,
        FailureInsightKind::BuildOrTestFailure => SuppressionTopic::BuildOrTestFailure,
        FailureInsightKind::RuntimeException => SuppressionTopic::RuntimeException,
        FailureInsightKind::AbnormalSignal => SuppressionTopic::AbnormalSignal,
        FailureInsightKind::CommandNotFound => unreachable!(),
    };
    let evidence_status = failure_output_status(block, &excerpt);
    let severity = if kind == FailureInsightKind::AbnormalSignal {
        InsightSeverity::Critical
    } else {
        InsightSeverity::Warning
    };
    let confidence = match semantics.confidence {
        FailureConfidence::High => InsightConfidence::High,
        FailureConfidence::Medium => InsightConfidence::Medium,
        FailureConfidence::Low => InsightConfidence::Low,
    };
    let mut evidence = vec![
        InsightEvidence {
            key: "failure_class".to_string(),
            value: format!("{:?}", semantics.class),
        },
        InsightEvidence {
            key: "failure_auto_eligibility".to_string(),
            value: format!("{:?}", semantics.auto_eligibility),
        },
    ];
    evidence.extend(
        semantics
            .reasons
            .iter()
            .filter(|reason| {
                matches!(
                    reason,
                    FailureReason::ExitCode(_)
                        | FailureReason::CommandFamily(_)
                        | FailureReason::TerminalSignature(_)
                        | FailureReason::ExcerptDirection(_)
                )
            })
            .enumerate()
            .map(|(index, reason)| InsightEvidence {
                key: format!("failure_reason_{index}"),
                value: format!("{reason:?}"),
            }),
    );
    let target = InsightTarget {
        insight_id: format!("failure-{}", block.id),
        source_session_id: block.session_id.clone(),
        source_command_block_id: block.id.clone(),
        scope: scope.clone(),
        evidence_handle: Some(crate::evidence::terminal_output_id(
            &block.session_id,
            &block.id,
        )),
        evidence_status,
        severity,
        confidence,
        evidence: evidence.clone(),
        created_at_ms: block.ended_at_ms,
    };
    let suppression_key = failure_suppression_key(topic.clone(), &block.command, scope.clone());
    Some(InsightCandidate {
        source: InsightSource::FailedCommand,
        topic,
        entity: suppression_key.entity.clone(),
        severity,
        confidence,
        evidence,
        suggestion: Some(PromptSuggestion::AgentPrompt {
            binding: Box::new(InsightBinding {
                suggestion_id: format!("failure-suggestion-{}", block.id),
                target,
            }),
        }),
        scope,
        suppression_key,
    })
}

fn failure_output_status(_block: &CommandBlock, excerpt: &EvidenceExcerpt) -> OutputExcerptStatus {
    match excerpt.capture_status {
        crate::evidence::EvidenceCaptureStatus::Expired => OutputExcerptStatus::Expired,
        crate::evidence::EvidenceCaptureStatus::Unavailable => OutputExcerptStatus::Unavailable,
        crate::evidence::EvidenceCaptureStatus::ReadFailed => OutputExcerptStatus::ReadFailed,
        crate::evidence::EvidenceCaptureStatus::Truncated => OutputExcerptStatus::Truncated,
        crate::evidence::EvidenceCaptureStatus::Available if excerpt.text.is_none() => {
            OutputExcerptStatus::ReadFailed
        }
        _ if excerpt
            .text
            .as_deref()
            .is_none_or(|text| text.trim().is_empty()) =>
        {
            OutputExcerptStatus::Empty
        }
        _ if excerpt.truncated => OutputExcerptStatus::Truncated,
        _ => OutputExcerptStatus::Available,
    }
}

pub(crate) fn render_post_failure_actions<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    findings: &[Finding],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let key = format!("cancel-{event_index}");
        if event_cancels_failed_command_analysis(event)
            && !state.handled_cancellations.contains(&key)
        {
            let Some(block) = pending_failed_block_for_event(blocks, state, event) else {
                continue;
            };

            state.handled_cancellations.insert(key);
            state.canceled_blocks.insert(block.id.clone());
            RatatuiInlineRenderer::for_terminal().write_notice_panel(
                output,
                NoticePanelModel {
                    title: state.i18n().t(MessageId::FailedAnalysisCancelledTitle),
                    body: vec![state.i18n().format(
                        MessageId::FailedAnalysisCancelledBody,
                        &[("command", block.command.as_str())],
                    )],
                    footer: Some(state.i18n().t(MessageId::FailedAnalysisCancelledFooter)),
                },
            )?;
            output.flush()?;
            continue;
        }

        let key = format!("confirm-{event_index}");
        if !event_confirms_failed_command_analysis(event)
            || state.handled_confirmations.contains(&key)
        {
            continue;
        }

        let Some(block) = pending_failed_block_for_event(blocks, state, event) else {
            continue;
        };

        state.handled_confirmations.insert(key);
        start_agent_for_block(
            block,
            blocks,
            findings,
            adapter,
            state,
            output,
            FailedCommandAgentStartOptions {
                selectable_after_event_index: Some(event_index),
                trigger: FailedCommandAnalysisTrigger::UserConfirmed,
            },
        )?;
        output.flush()?;
    }

    Ok(())
}

pub(crate) fn latest_pending_failed_block_before_event<'a>(
    blocks: &'a [CommandBlock],
    state: &InlineState,
    event: &ShellEvent,
) -> Option<&'a CommandBlock> {
    blocks.iter().rev().find(|block| {
        can_user_confirm_failure_analysis(block)
            && !state.analyzed_blocks.contains(&block.id)
            && !state.canceled_blocks.contains(&block.id)
            && event_happened_after_block_end(event, block)
    })
}

fn pending_failed_block_for_event<'a>(
    blocks: &'a [CommandBlock],
    state: &InlineState,
    event: &ShellEvent,
) -> Option<&'a CommandBlock> {
    latest_pending_failed_block_before_event(blocks, state, event)
}

#[allow(dead_code)]
pub(crate) fn should_analyze_failed_block(block: &CommandBlock, mode: AnalysisMode) -> bool {
    should_auto_analyze_failed_block(&[], block, mode)
}

pub(crate) fn should_auto_analyze_failed_block(
    events: &[ShellEvent],
    block: &CommandBlock,
    mode: AnalysisMode,
) -> bool {
    failure_analysis_disposition_for_block(events, block, mode)
        == FailureAnalysisDisposition::AutoAnalyze
}

pub(crate) fn failure_analysis_disposition_for_block(
    events: &[ShellEvent],
    block: &CommandBlock,
    mode: AnalysisMode,
) -> FailureAnalysisDisposition {
    let excerpt = failure_output_excerpt(block);
    failure_analysis_disposition(events, block, mode, excerpt.as_deref())
}

fn failure_analysis_disposition(
    events: &[ShellEvent],
    block: &CommandBlock,
    mode: AnalysisMode,
    output_excerpt: Option<&str>,
) -> FailureAnalysisDisposition {
    if block.command.trim().is_empty() || command_should_skip_failure_analysis(events, block) {
        return FailureAnalysisDisposition::SilentRecord;
    }

    let semantics = classify_failure(block, events, output_excerpt);
    match semantics.class {
        FailureClass::Success
        | FailureClass::ExpectedNoResult
        | FailureClass::UsageOrHelp
        | FailureClass::InteractiveCancel
        | FailureClass::UserInterrupt
        | FailureClass::PipelineNormal
        | FailureClass::CommandNotFound
        | FailureClass::GenericRuntimeFailure
        | FailureClass::ProviderOrInternalArtifact
        | FailureClass::UnknownFailure => FailureAnalysisDisposition::SilentRecord,
        FailureClass::PermissionDenied if semantics.confidence == FailureConfidence::High => {
            match mode {
                AnalysisMode::Auto
                    if semantics.auto_eligibility
                        == crate::command::FailureAutoEligibility::LegacyAllowlisted =>
                {
                    FailureAnalysisDisposition::AutoAnalyze
                }
                AnalysisMode::Auto => FailureAnalysisDisposition::ActionCard,
                AnalysisMode::Smart => FailureAnalysisDisposition::ActionCard,
                AnalysisMode::Manual => FailureAnalysisDisposition::SilentRecord,
            }
        }
        FailureClass::AbnormalSignal
        | FailureClass::BuildOrTestFailure
        | FailureClass::RuntimeException
            if semantics.confidence == FailureConfidence::High =>
        {
            match mode {
                AnalysisMode::Auto
                    if semantics.auto_eligibility
                        == crate::command::FailureAutoEligibility::LegacyAllowlisted
                        && output_excerpt.is_some_and(usable_failure_excerpt) =>
                {
                    FailureAnalysisDisposition::AutoAnalyze
                }
                AnalysisMode::Auto | AnalysisMode::Smart => FailureAnalysisDisposition::ActionCard,
                AnalysisMode::Manual => FailureAnalysisDisposition::SilentRecord,
            }
        }
        _ => FailureAnalysisDisposition::SilentRecord,
    }
}

fn usable_failure_excerpt(excerpt: &str) -> bool {
    let text = excerpt.trim();
    !text.is_empty() && text != "... <truncated>"
}

fn can_user_confirm_failure_analysis(block: &CommandBlock) -> bool {
    block.exit_code != 0
        && !block.command.trim().is_empty()
        && !matches!(
            block.origin,
            CommandOrigin::ProviderTool | CommandOrigin::ShellInternal
        )
}

fn failure_output_excerpt(block: &CommandBlock) -> Option<String> {
    failure_output_evidence(block).text
}

fn failure_output_evidence(block: &CommandBlock) -> EvidenceExcerpt {
    bounded_output_head_tail_excerpt_for_block(
        block,
        FAILURE_OUTPUT_EXCERPT_MAX_LINES,
        FAILURE_OUTPUT_EXCERPT_MAX_BYTES,
    )
}

fn command_not_found_diagnostic_tail(block: &CommandBlock) -> Option<String> {
    bounded_output_excerpt_for_block(
        block,
        OutputExcerptDirection::Tail,
        FAILURE_OUTPUT_EXCERPT_MAX_LINES,
        FAILURE_OUTPUT_EXCERPT_MAX_BYTES,
    )
    .text
}

fn event_happened_after_block_end(event: &ShellEvent, block: &CommandBlock) -> bool {
    event
        .started_at_ms
        .map(|timestamp| timestamp >= block.ended_at_ms)
        .unwrap_or(true)
}

pub(crate) fn block_end_event_index(events: &[ShellEvent], block: &CommandBlock) -> Option<usize> {
    events.iter().enumerate().find_map(|(idx, event)| {
        if event.command_id.as_deref() == Some(block.id.as_str())
            && matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            )
        {
            Some(idx)
        } else {
            None
        }
    })
}

pub(crate) fn start_agent_for_block<W: Write>(
    block: &CommandBlock,
    blocks: &[CommandBlock],
    findings: &[Finding],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    options: FailedCommandAgentStartOptions,
) -> std::io::Result<()> {
    let should_start = match options.trigger {
        FailedCommandAnalysisTrigger::Auto => true,
        FailedCommandAnalysisTrigger::UserConfirmed => can_user_confirm_failure_analysis(block),
    };
    if !should_start {
        return Ok(());
    }

    if state.canceled_blocks.contains(&block.id) {
        return Ok(());
    }

    if !state.analyzed_blocks.insert(block.id.clone()) {
        return Ok(());
    }

    if state.analysis_throttle.should_throttle(&block.command) {
        let throttle_key = format!("throttle:{}", first_program_token(&block.command));
        if state.queued_analysis_notices.insert(throttle_key) {
            RatatuiInlineRenderer::for_terminal().write_notice_panel(
                output,
                NoticePanelModel {
                    title: state.i18n().t(MessageId::AnalysisSkippedTitle),
                    body: vec![state.i18n().format(
                        MessageId::AnalysisSkippedBody,
                        &[("command", block.command.as_str())],
                    )],
                    footer: Some(state.i18n().t(MessageId::AnalysisSkippedFooter)),
                },
            )?;
            output.flush()?;
        }
        return Ok(());
    }

    let request = match options.trigger {
        FailedCommandAnalysisTrigger::Auto => Some(agent_request_for_auto_failure(
            &block.session_id,
            block,
            findings,
        )),
        FailedCommandAnalysisTrigger::UserConfirmed => {
            agent_request_after_confirmation(&block.session_id, block, findings, true)
        }
    };
    match request {
        Some(mut request) => {
            let ctx_config = RelatedHistoryConfig::default();
            let ctx_entries = build_related_history_index(blocks, block, &ctx_config);
            let target_scope = resolve_execution_scope(&block.session_id, &block.command);
            request.context_blocks = if target_scope.allows_correlation() {
                context_blocks_from_entries(&ctx_entries)
                    .into_iter()
                    .filter(|related| {
                        resolve_execution_scope(&related.session_id, &related.command)
                            == target_scope
                    })
                    .collect()
            } else {
                Vec::new()
            };
            request
                .context_hints
                .extend(hook_routing_hints_for_block(state, block));
            attach_failure_evidence_bundle(&mut request);
            if options.trigger == FailedCommandAnalysisTrigger::Auto
                && !request.context_hints.is_empty()
                && state.agent_run.active.is_none()
                && !crate::slash::session::compaction_active(state)
            {
                writeln!(
                    output,
                    "{} {}",
                    state.i18n().format(
                        MessageId::HookAutoAnalyzedBody,
                        &[
                            ("command", block.command.as_str()),
                            ("exit_code", &block.exit_code.to_string()),
                        ],
                    ),
                    state.i18n().t(MessageId::HookAutoAnalyzedFooter),
                )?;
            }
            if state.agent_run.active.is_some()
                && state.queued_analysis_notices.insert(block.id.clone())
            {
                RatatuiInlineRenderer::for_terminal().write_notice_panel(
                    output,
                    NoticePanelModel {
                        title: state.i18n().t(MessageId::AgentQueuedTitle),
                        body: vec![
                            state.i18n().format(
                                MessageId::AgentQueuedBodyCommand,
                                &[("command", block.command.as_str())],
                            ),
                            state.i18n().t(MessageId::AgentQueuedBodyActive).to_string(),
                        ],
                        footer: Some(state.i18n().t(MessageId::AgentQueuedFooter)),
                    },
                )?;
            }
            state.agent_run.needs_prompt_after_run = true;
            let (origin, intent) = match options.trigger {
                // Automatic analysis is internal best-effort; a user-confirmed
                // action is an explicit user request.
                FailedCommandAnalysisTrigger::Auto => (
                    AgentRunOrigin::AutoFailure,
                    AgentStartIntent::InternalBestEffort,
                ),
                FailedCommandAnalysisTrigger::UserConfirmed => {
                    (AgentRunOrigin::Standard, AgentStartIntent::UserInitiated)
                }
            };
            let disposition = start_agent_run_with_origin_disposition(
                &request,
                origin,
                intent,
                adapter,
                state,
                output,
                options.selectable_after_event_index,
            )?;
            if matches!(
                disposition,
                AgentStartDisposition::SuppressedByCompaction | AgentStartDisposition::QueueFull
            ) {
                // The analysis did not run and was not queued (a background
                // compaction rejected it, or the pending queue was full). Undo
                // the dedup mark so the block is not permanently recorded as
                // analyzed when it never ran.
                state.analyzed_blocks.remove(&block.id);
                state.queued_analysis_notices.remove(&block.id);
            }
            // A queue-full rejection of an explicit user-confirmed analysis must
            // be surfaced, not dropped silently. (A compaction rejection of the
            // best-effort auto path is intentionally quiet.)
            if disposition == AgentStartDisposition::QueueFull
                && intent == AgentStartIntent::UserInitiated
            {
                crate::slash::session::render_agent_queue_full_notice(state, output)?;
            }
            Ok(())
        }
        None => Ok(()),
    }
}

pub(crate) fn attach_failure_evidence_bundle(request: &mut AgentRequest) {
    let classifier_excerpt = bounded_output_head_tail_excerpt_for_block(
        &request.command_block,
        FAILURE_OUTPUT_EXCERPT_MAX_LINES,
        FAILURE_OUTPUT_EXCERPT_MAX_BYTES,
    );
    let semantics = classify_failure(
        &request.command_block,
        &[],
        classifier_excerpt.text.as_deref(),
    );
    let target_excerpt = match semantics.class {
        FailureClass::BuildOrTestFailure | FailureClass::RuntimeException => {
            bounded_output_head_tail_excerpt_for_block(&request.command_block, 120, 12 * 1024)
        }
        FailureClass::PermissionDenied | FailureClass::AbnormalSignal => {
            bounded_output_excerpt_for_block(
                &request.command_block,
                OutputExcerptDirection::Tail,
                120,
                12 * 1024,
            )
        }
        _ => classifier_excerpt,
    };
    let related_facts = request
        .context_blocks
        .iter()
        .map(crate::evidence::provider_safe_command_fact_line)
        .collect::<Vec<_>>();
    let severity = if semantics.class == FailureClass::AbnormalSignal {
        "Critical"
    } else {
        "Warning"
    };
    let metadata = take_bound_insight_metadata(
        &mut request.context_hints,
        severity,
        &format!("{:?}", semantics.confidence),
        failure_structured_evidence(&semantics),
    );
    let evidence_status = target_excerpt.evidence_status();
    let target_facts = provider_target_facts(
        &request.command_block,
        &format!(
            "{:?}",
            resolve_execution_scope(
                &request.command_block.session_id,
                &request.command_block.command
            )
        ),
        &format!("{:?}", request.command_block.origin),
        evidence_status,
        target_excerpt.redaction_status,
        target_excerpt.truncation_status(),
        &metadata,
    );
    trim_optional_context_hints(&mut request.context_hints);
    request.context_blocks.clear();
    let other_context_bytes = request
        .context_hints
        .iter()
        .map(|hint| hint.len() + 1)
        .sum();
    request.context_hints.push(build_provider_evidence_payload(
        EvidenceBundleInput {
            target_facts,
            target_excerpt: target_excerpt.text.unwrap_or_default(),
            related_facts,
        },
        other_context_bytes,
    ));
}

fn failure_structured_evidence(semantics: &crate::command::FailureSemantics) -> Vec<String> {
    let mut evidence = vec![
        format!("failure_class={:?}", semantics.class),
        format!("failure_auto_eligibility={:?}", semantics.auto_eligibility),
    ];
    let profile = match semantics.class {
        FailureClass::PermissionDenied => Some("permission"),
        FailureClass::BuildOrTestFailure => Some("build_or_test"),
        FailureClass::RuntimeException => Some("runtime_exception"),
        FailureClass::AbnormalSignal => Some("abnormal_signal"),
        _ => None,
    };
    if let Some(profile) = profile {
        evidence.push(format!("failure_profile={profile}"));
    }
    if semantics.class == FailureClass::RuntimeException {
        evidence.push(
            "failure_objectives=first_failing_frame,direct_cause,minimal_reproduction,smallest_safe_fix"
                .to_string(),
        );
    }
    evidence.extend(
        semantics
            .reasons
            .iter()
            .filter(|reason| {
                matches!(
                    reason,
                    FailureReason::ExitCode(_)
                        | FailureReason::CommandFamily(_)
                        | FailureReason::TerminalSignature(_)
                        | FailureReason::ExcerptDirection(_)
                )
            })
            .enumerate()
            .map(|(index, reason)| format!("failure_reason_{index}={reason:?}")),
    );
    evidence
}

#[cfg(test)]
#[path = "failed_command_tests.rs"]
mod tests;
