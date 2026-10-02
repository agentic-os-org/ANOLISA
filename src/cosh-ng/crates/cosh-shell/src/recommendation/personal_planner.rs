use std::cmp::Reverse;
use std::collections::BTreeMap;

use super::personal_model::{
    CandidateEvidenceSummary, CandidateSource, ContextAffinity, EntityEvidenceRef, ScopeKind,
};

const MAX_VISIBLE: usize = 3;
const RECENT_MAX_HOURS: u64 = 14 * 24;
const ACTIVE_RECENT_MAX_HOURS: u64 = 7 * 24;
const FREQUENT_MAX_HOURS: u64 = 30 * 24;
const DISPLAY_THRESHOLD: i32 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerCandidate {
    pub(crate) candidate_id: String,
    pub(crate) source: CandidateSource,
    pub(crate) task_ref: String,
    pub(crate) prompt_text: String,
    pub(crate) context_affinity: ContextAffinity,
    pub(crate) last_seen_hour_bucket: u64,
    pub(crate) evidence: CandidateEvidenceSummary,
    pub(crate) entities: Vec<EntityEvidenceRef>,
    pub(crate) suppression_key: String,
    pub(crate) last_action_failed: bool,
    pub(crate) consecutive_explicit_dismissals: u8,
    pub(crate) suppressed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerContext {
    pub(crate) now_hour_bucket: u64,
    pub(crate) repo_id: Option<String>,
    pub(crate) host_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HealthResolution<'a> {
    #[cfg(test)]
    Pending,
    TimedOut,
    Resolved(&'a [PlannerCandidate]),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RenderedStartupSuggestions {
    pub(crate) visible_candidates: Vec<PlannerCandidate>,
    pub(crate) ghost_candidate: Option<PlannerCandidate>,
    pub(crate) omitted_count: usize,
    pub(crate) omitted_reasons: BTreeMap<OmittedReason, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OmittedReason {
    LowConfidence,
    ScopeMismatch,
    Suppressed,
    Duplicate,
    Capacity,
    HealthUnresolved,
}

pub(crate) fn plan_startup(
    context: &PlannerContext,
    health: HealthResolution<'_>,
    personal: &[PlannerCandidate],
) -> RenderedStartupSuggestions {
    let HealthResolution::Resolved(health) = health else {
        let mut rendered = RenderedStartupSuggestions::default();
        for _ in personal {
            omit(&mut rendered, OmittedReason::HealthUnresolved);
        }
        return rendered;
    };

    let mut visible = Vec::with_capacity(MAX_VISIBLE);
    let mut rendered = RenderedStartupSuggestions::default();
    for candidate in health {
        if visible.len() == MAX_VISIBLE {
            omit(&mut rendered, OmittedReason::Capacity);
        } else if !is_duplicate(&visible, candidate) {
            visible.push(candidate.clone());
        } else {
            omit(&mut rendered, OmittedReason::Duplicate);
        }
    }

    let mut recent = Vec::new();
    let mut frequent = Vec::new();
    for candidate in personal {
        match candidate.source {
            CandidateSource::RecentTask => match omission_before_recent_score(context, candidate) {
                Some(reason) => omit(&mut rendered, reason),
                None => match recent_score(context, candidate) {
                    Some(score) => recent.push((score, candidate)),
                    None => omit(&mut rendered, OmittedReason::LowConfidence),
                },
            },
            CandidateSource::FrequentPattern => {
                if candidate.suppressed {
                    omit(&mut rendered, OmittedReason::Suppressed);
                } else if scope_score(context, candidate).is_none() {
                    omit(&mut rendered, OmittedReason::ScopeMismatch);
                } else if frequent_eligible(context, candidate) {
                    frequent.push(candidate);
                } else {
                    omit(&mut rendered, OmittedReason::LowConfidence);
                }
            }
            CandidateSource::Health => omit(&mut rendered, OmittedReason::LowConfidence),
        }
    }
    recent.sort_by_key(|(score, candidate)| {
        (
            Reverse(*score),
            Reverse(candidate.last_seen_hour_bucket),
            candidate.candidate_id.as_str(),
        )
    });

    frequent.sort_by_key(|candidate| {
        (
            Reverse(scope_rank(context, candidate).unwrap_or_default()),
            Reverse(candidate.evidence.active_day_buckets.len()),
            Reverse(candidate.evidence.intent_occurrence_count),
            Reverse(candidate.evidence.submitted_feedback_count),
            Reverse(candidate.last_seen_hour_bucket),
            candidate.candidate_id.as_str(),
        )
    });

    for candidate in recent
        .into_iter()
        .map(|(_, candidate)| candidate)
        .chain(frequent)
    {
        if visible.len() == MAX_VISIBLE {
            omit(&mut rendered, OmittedReason::Capacity);
        } else if is_duplicate(&visible, candidate) {
            omit(&mut rendered, OmittedReason::Duplicate);
        } else {
            visible.push(candidate.clone());
        }
    }

    let ghost = visible.first().cloned();
    rendered.visible_candidates = visible;
    rendered.ghost_candidate = ghost;
    rendered
}

fn omit(rendered: &mut RenderedStartupSuggestions, reason: OmittedReason) {
    rendered.omitted_count += 1;
    *rendered.omitted_reasons.entry(reason).or_default() += 1;
}

fn omission_before_recent_score(
    context: &PlannerContext,
    candidate: &PlannerCandidate,
) -> Option<OmittedReason> {
    if candidate.suppressed {
        Some(OmittedReason::Suppressed)
    } else if scope_score(context, candidate).is_none() {
        Some(OmittedReason::ScopeMismatch)
    } else {
        None
    }
}

fn recent_score(context: &PlannerContext, candidate: &PlannerCandidate) -> Option<i32> {
    if candidate.suppressed || candidate.last_seen_hour_bucket > context.now_hour_bucket {
        return None;
    }
    let age = context.now_hour_bucket - candidate.last_seen_hour_bucket;
    if age > RECENT_MAX_HOURS {
        return None;
    }
    let scope = scope_score(context, candidate)?;
    let has_intent = candidate.evidence.intent_occurrence_count > 0;
    if !has_intent && candidate.evidence.compatible_shell_count < 2 {
        return None;
    }
    if age > ACTIVE_RECENT_MAX_HOURS && (scope != 40 || !candidate.evidence.continuation_evidence) {
        return None;
    }

    let recency = match age {
        0..=24 => 30,
        25..=72 => 22,
        73..=168 => 12,
        _ => 2,
    };
    let score = scope
        + recency
        + if has_intent { 20 } else { 0 }
        + if candidate.last_action_failed { 10 } else { 0 }
        - if candidate.consecutive_explicit_dismissals >= 2 {
            25
        } else {
            0
        };
    (score >= DISPLAY_THRESHOLD).then_some(score)
}

fn frequent_eligible(context: &PlannerContext, candidate: &PlannerCandidate) -> bool {
    candidate.source == CandidateSource::FrequentPattern
        && !candidate.suppressed
        && candidate.last_seen_hour_bucket <= context.now_hour_bucket
        && context.now_hour_bucket - candidate.last_seen_hour_bucket <= FREQUENT_MAX_HOURS
        && candidate.evidence.active_day_buckets.len() >= 3
        && scope_score(context, candidate).is_some()
}

fn scope_score(context: &PlannerContext, candidate: &PlannerCandidate) -> Option<i32> {
    match candidate.context_affinity.scope_kind {
        ScopeKind::Repo => match (
            candidate.context_affinity.repo_id.as_deref(),
            context.repo_id.as_deref(),
        ) {
            (Some(candidate_repo), Some(current_repo)) if candidate_repo == current_repo => {
                Some(40)
            }
            _ => None,
        },
        ScopeKind::HostFallback => same_value(
            candidate.context_affinity.host_id.as_deref(),
            context.host_id.as_deref(),
        )
        .then_some(20),
        ScopeKind::HostWide => (candidate.source == CandidateSource::FrequentPattern
            && same_value(
                candidate.context_affinity.host_id.as_deref(),
                context.host_id.as_deref(),
            ))
        .then_some(20),
    }
}

fn scope_rank(context: &PlannerContext, candidate: &PlannerCandidate) -> Option<u8> {
    scope_score(context, candidate).map(|score| if score == 40 { 2 } else { 1 })
}

fn same_value(left: Option<&str>, right: Option<&str>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

fn is_duplicate(existing: &[PlannerCandidate], candidate: &PlannerCandidate) -> bool {
    let normalized_prompt = normalize_prompt(&candidate.prompt_text);
    existing.iter().any(|other| {
        other.task_ref == candidate.task_ref
            || normalize_prompt(&other.prompt_text) == normalized_prompt
            || same_entity_signature(&other.entities, &candidate.entities)
    })
}

fn same_entity_signature(left: &[EntityEvidenceRef], right: &[EntityEvidenceRef]) -> bool {
    !left.is_empty()
        && left.len() == right.len()
        && left.iter().all(|left| {
            right.iter().any(|right| {
                left.entity.kind == right.entity.kind
                    && left.entity.value.eq_ignore_ascii_case(&right.entity.value)
            })
        })
}

fn normalize_prompt(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
#[path = "personal_planner_tests.rs"]
mod tests;
