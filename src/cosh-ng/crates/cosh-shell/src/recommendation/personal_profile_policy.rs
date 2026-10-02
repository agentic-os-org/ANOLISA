//! Owns bounded profile retention and cache projection shared by Analyzer and Store.

use std::collections::HashSet;

use super::personal_model::{
    CachedPromptCandidate, CandidateEvidenceSummary, CandidateSource, ContextAffinity,
    EntityEvidenceRef, EvidenceSnapshot, FrequentPattern, RecentTask, RecommendationCache,
    ScopeKind, TaskEntity, UserWorkProfile,
};

pub(super) const MAX_SNAPSHOTS: usize = 60;
pub(super) const MAX_CACHE_CANDIDATES: usize = 10;
pub(super) const RECENT_CACHE_QUOTA: usize = 7;

pub(super) fn roll_profile_snapshots(
    snapshots: &mut Vec<EvidenceSnapshot>,
    recent: &mut Vec<RecentTask>,
    frequent: &mut Vec<FrequentPattern>,
    trusted_now_hour_bucket: u64,
) {
    snapshots.sort_by(|left, right| {
        trusted_snapshot_last_seen(right, trusted_now_hour_bucket)
            .cmp(&trusted_snapshot_last_seen(left, trusted_now_hour_bucket))
            .then_with(|| left.snapshot_id.cmp(&right.snapshot_id))
    });
    snapshots.truncate(MAX_SNAPSHOTS);
    let retained = snapshots
        .iter()
        .map(|snapshot| snapshot.snapshot_id.as_str())
        .collect::<HashSet<_>>();
    recent.retain_mut(|task| {
        task.evidence_snapshot_ids
            .retain(|id| retained.contains(id.as_str()));
        !task.evidence_snapshot_ids.is_empty()
    });
    frequent.retain_mut(|pattern| {
        pattern
            .evidence_snapshot_ids
            .retain(|id| retained.contains(id.as_str()));
        !pattern.evidence_snapshot_ids.is_empty()
    });
}

pub(super) fn build_cache(
    recent: &[RecentTask],
    frequent: &[FrequentPattern],
    snapshots: &[EvidenceSnapshot],
    generation: u64,
    trusted_now_hour_bucket: u64,
    mut candidate_id_for: impl FnMut(CandidateSource, &str) -> String,
) -> RecommendationCache {
    let mut sorted_recent = recent.iter().collect::<Vec<_>>();
    sorted_recent.sort_by(|left, right| {
        trusted_recent_last_seen(right, trusted_now_hour_bucket)
            .cmp(&trusted_recent_last_seen(left, trusted_now_hour_bucket))
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    let mut sorted_frequent = frequent.iter().collect::<Vec<_>>();
    sorted_frequent.sort_by(|left, right| {
        trusted_pattern_last_seen(right, snapshots, trusted_now_hour_bucket)
            .cmp(&trusted_pattern_last_seen(
                left,
                snapshots,
                trusted_now_hour_bucket,
            ))
            .then_with(|| {
                right
                    .active_day_buckets
                    .len()
                    .cmp(&left.active_day_buckets.len())
            })
            .then_with(|| left.pattern_id.cmp(&right.pattern_id))
    });

    let mut selected_recent = sorted_recent
        .iter()
        .take(RECENT_CACHE_QUOTA)
        .copied()
        .collect::<Vec<_>>();
    let mut selected_prompts = selected_recent
        .iter()
        .map(|task| scoped_prompt_key(&task.prompt_text, &task.context_affinity))
        .collect::<HashSet<_>>();
    let selected_frequent = sorted_frequent
        .iter()
        .copied()
        .filter(|pattern| {
            selected_prompts.insert(scoped_prompt_key(
                &pattern.prompt_text,
                &pattern.context_affinity,
            ))
        })
        .take(MAX_CACHE_CANDIDATES.saturating_sub(selected_recent.len()))
        .collect::<Vec<_>>();
    for task in sorted_recent.iter().skip(RECENT_CACHE_QUOTA).copied() {
        if selected_recent.len() + selected_frequent.len() == MAX_CACHE_CANDIDATES {
            break;
        }
        if selected_prompts.insert(scoped_prompt_key(&task.prompt_text, &task.context_affinity)) {
            selected_recent.push(task);
        }
    }

    let mut candidates = Vec::new();
    for task in selected_recent {
        candidates.push(cached_candidate(
            candidate_id_for(CandidateSource::RecentTask, &task.task_id),
            CandidateSource::RecentTask,
            &task.task_id,
            &task.prompt_text,
            &task.context_affinity,
            trusted_recent_last_seen(task, trusted_now_hour_bucket),
            &task.entities,
            &task.evidence_snapshot_ids,
            snapshots,
            trusted_now_hour_bucket,
        ));
    }
    for pattern in selected_frequent {
        candidates.push(cached_candidate(
            candidate_id_for(CandidateSource::FrequentPattern, &pattern.pattern_id),
            CandidateSource::FrequentPattern,
            &pattern.pattern_id,
            &pattern.prompt_text,
            &pattern.context_affinity,
            trusted_pattern_last_seen(pattern, snapshots, trusted_now_hour_bucket),
            &pattern.stable_entities,
            &pattern.evidence_snapshot_ids,
            snapshots,
            trusted_now_hour_bucket,
        ));
    }
    candidates.shrink_to_fit();
    RecommendationCache {
        profile_generation: generation,
        generated_hour_bucket: snapshots
            .iter()
            .map(|snapshot| snapshot.last_seen_hour_bucket)
            .filter(|hour| *hour <= trusted_now_hour_bucket)
            .max()
            .unwrap_or(0),
        candidates,
    }
}

pub(super) fn reconcile_profile_snapshot_evidence(
    profile: &mut UserWorkProfile,
    trusted_now_hour_bucket: u64,
) {
    for task in &mut profile.recent_tasks {
        task.last_seen_hour_bucket = task
            .evidence_snapshot_ids
            .iter()
            .filter_map(|id| {
                profile
                    .evidence_snapshots
                    .iter()
                    .find(|snapshot| snapshot.snapshot_id == *id)
            })
            .map(|snapshot| trusted_snapshot_last_seen(snapshot, trusted_now_hour_bucket))
            .max()
            .unwrap_or(0);
    }
    profile.frequent_patterns.retain_mut(|pattern| {
        pattern.active_day_buckets = profile
            .evidence_snapshots
            .iter()
            .filter(|snapshot| {
                pattern
                    .evidence_snapshot_ids
                    .contains(&snapshot.snapshot_id)
            })
            .flat_map(|snapshot| snapshot.active_day_buckets.iter().copied())
            .filter(|day| u64::from(*day) <= trusted_now_hour_bucket / 24)
            .collect();
        pattern.active_day_buckets.sort_unstable();
        pattern.active_day_buckets.dedup();
        pattern.active_day_buckets.len() >= 3
    });
}

pub(super) fn rebuild_cache(
    cache: &mut RecommendationCache,
    profile: &UserWorkProfile,
    trusted_now_hour_bucket: u64,
) {
    let existing = cache.candidates.clone();
    *cache = build_cache(
        &profile.recent_tasks,
        &profile.frequent_patterns,
        &profile.evidence_snapshots,
        cache.profile_generation,
        trusted_now_hour_bucket,
        |source, task_ref| {
            existing
                .iter()
                .find(|candidate| candidate.source == source && candidate.task_ref == task_ref)
                .map(|candidate| candidate.candidate_id.clone())
                .unwrap_or_else(|| {
                    let source = match source {
                        CandidateSource::RecentTask => "recent",
                        CandidateSource::FrequentPattern => "frequent",
                        CandidateSource::Health => "health",
                    };
                    format!("candidate-{source}-{task_ref}")
                })
        },
    );
}

fn scoped_prompt_key(prompt: &str, affinity: &ContextAffinity) -> String {
    let scope = match affinity.scope_kind {
        ScopeKind::Repo => "repo",
        ScopeKind::HostWide => "host_wide",
        ScopeKind::HostFallback => "host_fallback",
    };
    let normalized = prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    format!(
        "{scope}\0{}\0{}\0{normalized}",
        affinity.repo_id.as_deref().unwrap_or_default(),
        affinity.host_id.as_deref().unwrap_or_default(),
    )
}

fn trusted_recent_last_seen(task: &RecentTask, trusted_now_hour_bucket: u64) -> u64 {
    if task.last_seen_hour_bucket <= trusted_now_hour_bucket {
        task.last_seen_hour_bucket
    } else {
        0
    }
}

fn trusted_snapshot_last_seen(snapshot: &EvidenceSnapshot, trusted_now_hour_bucket: u64) -> u64 {
    if snapshot.last_seen_hour_bucket <= trusted_now_hour_bucket {
        snapshot.last_seen_hour_bucket
    } else {
        0
    }
}

fn trusted_pattern_last_seen(
    pattern: &FrequentPattern,
    snapshots: &[EvidenceSnapshot],
    trusted_now_hour_bucket: u64,
) -> u64 {
    pattern
        .evidence_snapshot_ids
        .iter()
        .filter_map(|id| {
            snapshots
                .iter()
                .find(|snapshot| &snapshot.snapshot_id == id)
        })
        .map(|snapshot| snapshot.last_seen_hour_bucket)
        .filter(|hour| *hour <= trusted_now_hour_bucket)
        .max()
        .unwrap_or(0)
}

// Keep projection inputs explicit so this boundary does not introduce a second cache model.
#[allow(clippy::too_many_arguments)]
fn cached_candidate(
    candidate_id: String,
    source: CandidateSource,
    task_ref: &str,
    prompt_text: &str,
    context_affinity: &ContextAffinity,
    last_seen_hour_bucket: u64,
    entities: &[TaskEntity],
    snapshot_ids: &[String],
    snapshots: &[EvidenceSnapshot],
    trusted_now_hour_bucket: u64,
) -> CachedPromptCandidate {
    let selected = snapshots
        .iter()
        .filter(|snapshot| snapshot_ids.contains(&snapshot.snapshot_id))
        .collect::<Vec<_>>();
    let mut active_days = selected
        .iter()
        .flat_map(|snapshot| snapshot.active_day_buckets.iter().copied())
        .filter(|day| u64::from(*day) <= trusted_now_hour_bucket / 24)
        .collect::<Vec<_>>();
    active_days.sort_unstable();
    active_days.dedup();
    CachedPromptCandidate {
        candidate_id,
        source,
        task_ref: task_ref.to_string(),
        prompt_text: prompt_text.to_string(),
        context_affinity: context_affinity.clone(),
        last_seen_hour_bucket: if last_seen_hour_bucket <= trusted_now_hour_bucket {
            last_seen_hour_bucket
        } else {
            0
        },
        last_action_failed: selected
            .iter()
            .filter(|snapshot| snapshot.last_seen_hour_bucket <= trusted_now_hour_bucket)
            .max_by_key(|snapshot| snapshot.last_seen_hour_bucket)
            .is_some_and(|snapshot| snapshot.last_action_failed),
        evidence: CandidateEvidenceSummary {
            snapshot_ids: snapshot_ids.to_vec(),
            agent_request_count: selected
                .iter()
                .map(|snapshot| snapshot.agent_request_count)
                .fold(0, u16::saturating_add),
            compatible_shell_count: selected
                .iter()
                .map(|snapshot| snapshot.compatible_shell_count)
                .fold(0, u16::saturating_add),
            submitted_feedback_count: selected
                .iter()
                .map(|snapshot| snapshot.submitted_feedback_count)
                .fold(0, u16::saturating_add),
            intent_occurrence_count: selected
                .iter()
                .map(|snapshot| snapshot.intent_occurrence_count)
                .fold(0, u16::saturating_add),
            active_day_buckets: active_days,
            continuation_evidence: false,
        },
        entities: entities
            .iter()
            .cloned()
            .map(|entity| EntityEvidenceRef {
                entity,
                snapshot_ids: snapshot_ids.to_vec(),
            })
            .collect(),
    }
}
