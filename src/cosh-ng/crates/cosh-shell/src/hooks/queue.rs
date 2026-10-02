use std::collections::HashMap;
use std::io::Write;

use super::aggregate::{entity_key, finding_topic, severity_rank, AggregatedHookFinding};
use super::feedback::decide_session_interruption_policy;
use super::prelude::{
    CommandBlock, CommandStatus, FindingSeverity, HookFinding, Language, OutputRefs,
};
use super::presentation::render_consultation_card;
use super::prompt::format_runtime_hint;
use super::state::{
    HookRuntimeState, HookSuppressionRecord, InterruptionBudgetRecord, PendingConsultation,
    PendingConsultationState, RuntimeHookDisplay, RuntimeHookDisplayAction,
    RuntimeHookDisplayEvent, RuntimeHookFinding,
};
use crate::types::HookProvenance;

pub(crate) const INTERRUPTION_BUDGET_WINDOW_MS: u64 = 10 * 60 * 1000;
pub(crate) const PENDING_CONSULTATION_TTL_MS: u64 = INTERRUPTION_BUDGET_WINDOW_MS;
pub(crate) const SUCCESS_CONSULTATION_IDLE_GRACE: std::time::Duration =
    std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueuedConsultationDecision {
    Render,
    KeepQueued,
    Drop,
}

pub(crate) fn interruption_budget_exhausted_for_budget(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    budget: &HashMap<String, InterruptionBudgetRecord>,
) -> bool {
    let topic = finding_topic(aggregate);
    let entity = entity_key(block, aggregate);
    let severity = aggregate.primary.severity;
    [topic_budget_key(topic), entity_budget_key(topic, &entity)]
        .iter()
        .filter_map(|key| budget.get(key))
        .any(|record| suppress_by_budget(record, block.ended_at_ms, severity))
}

pub(crate) fn record_interruption_budget(
    consultation: &PendingConsultation,
    hooks: &mut HookRuntimeState,
) {
    let Some(finding) = consultation.hook_finding.as_ref() else {
        return;
    };
    let record = InterruptionBudgetRecord {
        last_rendered_at_ms: consultation.ended_at_ms,
        severity: finding.severity,
    };
    hooks
        .interruption_budget
        .insert(topic_budget_key(&consultation.topic), record);
    hooks.interruption_budget.insert(
        entity_budget_key(&consultation.topic, &consultation.entity_key),
        record,
    );
}

pub(crate) fn consultation_from_hint(hint: &RuntimeHookFinding) -> Option<PendingConsultation> {
    let finding = hint.hook_finding.clone()?;
    Some(PendingConsultation {
        finding_id: hint.id.clone(),
        card_id: format!("consultation-{}", hint.id),
        block_id: hint.command_block_id.clone(),
        command: hint.command.clone(),
        output_ref: hint.output_ref.clone(),
        state: PendingConsultationState::Queued,
        created_at_ms: hint.ended_at_ms,
        expires_at_ms: hint.ended_at_ms.saturating_add(PENDING_CONSULTATION_TTL_MS),
        ended_at_ms: hint.ended_at_ms,
        queued_at: std::time::Instant::now(),
        prompt_hint: hint.prompt_hint.clone(),
        hook_finding: Some(finding),
        recommended_skill: hint.recommended_skill.clone(),
        context_hints: vec![format_runtime_hint(hint)],
        suppression_key: hint.suppression_key.clone(),
        topic: hint.topic.clone(),
        entity_key: hint.entity_key.clone(),
        confidence: hint.confidence.clone(),
        display_reason: hint.display_reason.clone(),
    })
}

fn suppress_by_budget(
    record: &InterruptionBudgetRecord,
    ended_at_ms: u64,
    severity: FindingSeverity,
) -> bool {
    ended_at_ms.saturating_sub(record.last_rendered_at_ms) < INTERRUPTION_BUDGET_WINDOW_MS
        && severity_rank(record.severity) >= severity_rank(severity)
}

pub(crate) fn topic_budget_key(topic: &str) -> String {
    format!("topic:{topic}")
}

fn entity_budget_key(topic: &str, entity_key: &str) -> String {
    format!("entity:{topic}:{entity_key}")
}

pub(crate) fn render_or_queue_consultation<W: Write>(
    hint: &RuntimeHookFinding,
    hooks: &mut HookRuntimeState,
    active_agent_run: bool,
    _output: &mut W,
) -> std::io::Result<()> {
    let Some(mut consultation) = consultation_from_hint(hint) else {
        return Ok(());
    };
    if active_agent_run {
        consultation.state = PendingConsultationState::Deferred;
        consultation.display_reason = "active-agent-run-deferred".to_string();
        record_hook_display_event_for_consultation(
            &consultation,
            RuntimeHookDisplayAction::Deferred,
            hooks,
        );
        hooks.pending_consultation_queue.push_back(consultation);
        return Ok(());
    }
    if hooks.pending_consultation.is_some() {
        consultation.state = PendingConsultationState::Queued;
        hooks.pending_consultation_queue.push_back(consultation);
        return Ok(());
    }
    consultation.state = PendingConsultationState::Queued;
    hooks.pending_consultation_queue.push_back(consultation);
    Ok(())
}

pub(crate) fn render_next_queued_consultation<W: Write>(
    hooks: &mut HookRuntimeState,
    active_agent_run: bool,
    language: Language,
    output: &mut W,
) -> std::io::Result<()> {
    if active_agent_run || hooks.pending_consultation.is_some() {
        return Ok(());
    }
    let Some(mut consultation) = next_renderable_queued_consultation(hooks, active_agent_run)
    else {
        return Ok(());
    };
    consultation.state = PendingConsultationState::Displayed;
    mark_consultation_rendered(&consultation, hooks);
    render_consultation_card(&consultation, language, output)?;
    hooks.pending_consultation = Some(consultation);
    Ok(())
}

fn next_renderable_queued_consultation(
    hooks: &mut HookRuntimeState,
    active_agent_run: bool,
) -> Option<PendingConsultation> {
    let now_ms = pending_consultation_now_ms(hooks);
    while let Some(mut consultation) = hooks.pending_consultation_queue.pop_front() {
        match queued_consultation_decision(&mut consultation, hooks, active_agent_run, now_ms) {
            QueuedConsultationDecision::Render => return Some(consultation),
            QueuedConsultationDecision::KeepQueued => {
                hooks.pending_consultation_queue.push_front(consultation);
                return None;
            }
            QueuedConsultationDecision::Drop => {}
        }
    }
    None
}

pub(crate) fn queued_consultation_decision(
    consultation: &mut PendingConsultation,
    hooks: &mut HookRuntimeState,
    active_agent_run: bool,
    now_ms: u64,
) -> QueuedConsultationDecision {
    if now_ms > consultation.expires_at_ms {
        consultation.state = PendingConsultationState::Expired;
        record_hook_display_event_for_consultation(
            consultation,
            RuntimeHookDisplayAction::Expired,
            hooks,
        );
        return QueuedConsultationDecision::Drop;
    }
    if consultation.queued_at.elapsed() < SUCCESS_CONSULTATION_IDLE_GRACE {
        return QueuedConsultationDecision::KeepQueued;
    }
    let Some(finding) = consultation.hook_finding.as_ref() else {
        consultation.state = PendingConsultationState::Expired;
        record_hook_display_event_for_consultation(
            consultation,
            RuntimeHookDisplayAction::Expired,
            hooks,
        );
        return QueuedConsultationDecision::Drop;
    };

    let block = block_for_pending_consultation(consultation);
    let aggregate = aggregate_for_pending_consultation(consultation, finding.clone());
    let decision = decide_session_interruption_policy(
        &block,
        &aggregate,
        RuntimeHookDisplay::Consultation,
        &consultation.suppression_key,
        hooks,
        active_agent_run,
    );
    if decision.display != RuntimeHookDisplay::Consultation {
        consultation.display_reason = decision.reason.to_string();
        consultation.state = if decision.reason == "ignored-same-finding" {
            PendingConsultationState::Ignored
        } else {
            PendingConsultationState::Deferred
        };
        let action = if consultation.state == PendingConsultationState::Ignored {
            RuntimeHookDisplayAction::Ignored
        } else {
            RuntimeHookDisplayAction::Deferred
        };
        record_hook_display_event_for_consultation(consultation, action, hooks);
        return QueuedConsultationDecision::Drop;
    }
    consultation.display_reason = decision.reason.to_string();
    QueuedConsultationDecision::Render
}

fn block_for_pending_consultation(consultation: &PendingConsultation) -> CommandBlock {
    CommandBlock {
        id: consultation.block_id.clone(),
        session_id: "session".to_string(),
        command: consultation.command.clone(),
        origin: Default::default(),
        cwd: String::new(),
        end_cwd: String::new(),
        started_at_ms: consultation.created_at_ms,
        ended_at_ms: consultation.ended_at_ms,
        duration_ms: consultation
            .ended_at_ms
            .saturating_sub(consultation.created_at_ms),
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: consultation.output_ref.clone(),
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn aggregate_for_pending_consultation(
    consultation: &PendingConsultation,
    finding: HookFinding,
) -> AggregatedHookFinding {
    let effective_severity = finding.severity;
    AggregatedHookFinding {
        provenance: HookProvenance::External {
            registration_key: "legacy-pending-consultation".to_string(),
        },
        builtin_facts: Vec::new(),
        primary: finding,
        related: Vec::new(),
        recommended_skill: consultation.recommended_skill.clone(),
        topic: consultation.topic.clone(),
        entity_key: consultation.entity_key.clone(),
        effective_severity,
        confidence: consultation.confidence.clone(),
        suppression_key: consultation.suppression_key.clone(),
    }
}

fn pending_consultation_now_ms(hooks: &HookRuntimeState) -> u64 {
    hooks
        .findings
        .iter()
        .map(|hint| hint.ended_at_ms)
        .chain(
            hooks
                .pending_consultation
                .iter()
                .map(|consultation| consultation.ended_at_ms),
        )
        .chain(
            hooks
                .pending_consultation_queue
                .iter()
                .map(|consultation| consultation.ended_at_ms),
        )
        .max()
        .unwrap_or(0)
}

fn mark_consultation_rendered(consultation: &PendingConsultation, hooks: &mut HookRuntimeState) {
    let Some(finding) = consultation.hook_finding.as_ref() else {
        return;
    };
    hooks.rendered_cards.insert(
        consultation.suppression_key.clone(),
        HookSuppressionRecord {
            severity: finding.severity,
        },
    );
    record_interruption_budget(consultation, hooks);
    record_hook_display_event_for_consultation(
        consultation,
        RuntimeHookDisplayAction::Shown,
        hooks,
    );
}

pub(crate) fn record_hook_display_event_for_consultation(
    consultation: &PendingConsultation,
    action: RuntimeHookDisplayAction,
    hooks: &mut HookRuntimeState,
) {
    let hook_id = consultation
        .hook_finding
        .as_ref()
        .map(|finding| finding.hook_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    hooks.record_display_event(RuntimeHookDisplayEvent {
        action,
        finding_id: consultation.finding_id.clone(),
        command_block_id: consultation.block_id.clone(),
        hook_id,
        topic: consultation.topic.clone(),
        entity_key: consultation.entity_key.clone(),
        suppression_key: consultation.suppression_key.clone(),
        display: RuntimeHookDisplay::Consultation,
        display_reason: consultation.display_reason.clone(),
        confidence: consultation.confidence.clone(),
        ended_at_ms: consultation.ended_at_ms,
    });
}
