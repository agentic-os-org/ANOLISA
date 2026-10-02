use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::personal_model::{
    ActivityContext, ActivityOutcome, ActivityPayload, ActivityRecord, ActivitySource,
    ContextAffinity, EvidenceSnapshot, FrequentPattern, ProfileAnalyzerResult, RecentTask,
    RecommendationCache, RecommendationFeedbackState, RecommendationState, ScopeKind, TaskEntity,
    UserWorkProfile,
};
use super::personal_profile_policy::{self, roll_profile_snapshots};
use super::personal_sanitize::contains_hard_secret;

pub(crate) const MAX_ACTIVITY_RECORDS: usize = 20;
pub(crate) const MAX_DYNAMIC_INPUT_BYTES: usize = 8 * 1024;
pub(crate) const MAX_FIXED_PROMPT_BYTES: usize = 4 * 1024;
pub(crate) const MAX_PROVIDER_INPUT_BYTES: usize = 12 * 1024;
pub(crate) const MAX_OUTPUT_BYTES: usize = 16 * 1024;

const TRIGGER_BUDGET: usize = 3 * 1024;
const PROFILE_BUDGET: usize = 2 * 1024;
const FEEDBACK_BUDGET: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnalyzerError {
    NoEligibleTrigger,
    InputTooLarge,
    PromptTooLarge,
    OutputTooLarge,
    InvalidJson,
    UnknownEvidence,
    EmptyEvidence,
    DiscardEvidenceOverlap,
    DuplicateEvidence,
    InvalidPriorReference,
    InvalidAgentRunEvidence,
    UngroundedEntity,
    HardSecret,
    InvalidFrequentEvidence,
    CapacityExceeded,
    UnsafeTerminalText,
    StaleEpoch,
    StaleProfile,
}

#[derive(Debug, Clone)]
pub(crate) struct BuiltAnalyzerInput {
    pub(crate) json: String,
    pub(crate) activity_ids: Vec<String>,
    pub(crate) base_epoch: String,
    pub(crate) base_profile_generation: u64,
    pub(crate) trusted_now_hour_bucket: u64,
}

#[derive(Debug, Clone, Serialize)]
struct AnalyzerEnvelope {
    previous_profile: ProfileProjection,
    new_activities: Vec<ActivityRecord>,
    feedback_summary: Vec<RecommendationFeedbackState>,
    current_limits: AnalyzerLimits,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ProfileProjection {
    summary_generation: u64,
    recent_tasks: Vec<ProjectedProfileItem>,
    frequent_patterns: Vec<ProjectedProfileItem>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectedProfileItem {
    id: String,
    summary: String,
    prompt_text: String,
    entities: Vec<TaskEntity>,
    snapshot_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct AnalyzerLimits {
    max_recent_tasks: usize,
    max_frequent_patterns: usize,
    max_candidates: usize,
}

pub(crate) trait LocalIdSource {
    fn next_id(&mut self, prefix: &str) -> String;
}

#[derive(Debug, Clone)]
pub(crate) struct MergePatch {
    base_epoch: String,
    base_profile_generation: u64,
    consumed_activity_ids: Vec<String>,
    evidence_snapshots: Vec<EvidenceSnapshot>,
    recent_tasks: Vec<RecentTask>,
    frequent_patterns: Vec<FrequentPattern>,
    cache: RecommendationCache,
    updated_hour_bucket: u64,
}

pub(crate) fn build_input(
    state: &RecommendationState,
    current_session_scope_id: &str,
    trusted_now_hour_bucket: u64,
) -> Result<BuiltAnalyzerInput, AnalyzerError> {
    let trigger = state
        .journal
        .records
        .iter()
        .rev()
        .find(|record| {
            record.summarized_generation.is_none()
                && record.session_scope_id.as_deref() == Some(current_session_scope_id)
                && is_trigger(record)
        })
        .ok_or(AnalyzerError::NoEligibleTrigger)?;
    let trigger = fit_trigger(trigger.clone())?;
    let trigger_context = trigger.context.clone();
    let mut activities = vec![trigger.clone()];

    let mut supporting = state
        .journal
        .records
        .iter()
        .filter(|record| {
            record.summarized_generation.is_none() && record.activity_id != trigger.activity_id
        })
        .cloned()
        .collect::<Vec<_>>();
    supporting.sort_by_key(|record| {
        (
            std::cmp::Reverse(activity_priority(record, &trigger_context)),
            std::cmp::Reverse(record.observed_hour_bucket),
        )
    });

    let profile = project_profile(&state.profile, &trigger_context);
    let feedback = project_feedback(&state.feedback);
    for record in supporting {
        if activities.len() == MAX_ACTIVITY_RECORDS {
            break;
        }
        let mut candidate = activities.clone();
        candidate.push(record);
        if serialize_envelope(&profile, &candidate, &feedback)?.len() <= MAX_DYNAMIC_INPUT_BYTES {
            activities = candidate;
        }
    }

    let json = serialize_envelope(&profile, &activities, &feedback)?;
    if json.len() > MAX_DYNAMIC_INPUT_BYTES {
        return Err(AnalyzerError::InputTooLarge);
    }
    Ok(BuiltAnalyzerInput {
        activity_ids: activities
            .iter()
            .map(|record| record.activity_id.clone())
            .collect(),
        json,
        base_epoch: state.store_epoch.clone(),
        base_profile_generation: state.profile.summary_generation,
        trusted_now_hour_bucket,
    })
}

pub(crate) fn build_fixed_prompt(schema: &str) -> Result<String, AnalyzerError> {
    let prompt = format!(
        "Summarize only grounded work into the supplied JSON schema. List every business entity mentioned by a summary or prompt in that item's entities field. Business terms in summaries, prompts, and entities must appear in supplied activity evidence or the referenced previous profile item. Abstain when evidence is insufficient. Output exactly one JSON object.\nSCHEMA:\n{schema}"
    );
    (prompt.len() <= MAX_FIXED_PROMPT_BYTES)
        .then_some(prompt)
        .ok_or(AnalyzerError::PromptTooLarge)
}

pub(crate) fn validate_provider_budget(
    prompt: &str,
    input: &BuiltAnalyzerInput,
) -> Result<(), AnalyzerError> {
    if prompt.len() > MAX_FIXED_PROMPT_BYTES {
        return Err(AnalyzerError::PromptTooLarge);
    }
    if input.json.len() > MAX_DYNAMIC_INPUT_BYTES
        || prompt.len().saturating_add(input.json.len()) > MAX_PROVIDER_INPUT_BYTES
    {
        return Err(AnalyzerError::InputTooLarge);
    }
    Ok(())
}

pub(crate) fn prepare_merge_patch(
    raw: &str,
    input: &BuiltAnalyzerInput,
    state: &RecommendationState,
    ids: &mut impl LocalIdSource,
) -> Result<MergePatch, AnalyzerError> {
    if raw.len() > MAX_OUTPUT_BYTES {
        return Err(AnalyzerError::OutputTooLarge);
    }
    let result: ProfileAnalyzerResult =
        serde_json::from_str(raw).map_err(|_| AnalyzerError::InvalidJson)?;
    if result.recent_tasks.len() + result.frequent_patterns.len() > 20 {
        return Err(AnalyzerError::CapacityExceeded);
    }
    let input_ids = input.activity_ids.iter().cloned().collect::<HashSet<_>>();
    let records = state
        .journal
        .records
        .iter()
        .filter(|record| input_ids.contains(&record.activity_id))
        .map(|record| (record.activity_id.clone(), record))
        .collect::<HashMap<_, _>>();
    if records.len() != input_ids.len() {
        return Err(AnalyzerError::UnknownEvidence);
    }

    let discarded = result
        .discarded_activities
        .iter()
        .map(|item| item.activity_id.clone())
        .collect::<HashSet<_>>();
    if discarded.len() != result.discarded_activities.len() {
        return Err(AnalyzerError::DuplicateEvidence);
    }
    if !discarded.iter().all(|id| input_ids.contains(id)) {
        return Err(AnalyzerError::UnknownEvidence);
    }
    let mut accepted = HashSet::new();
    for evidence_id in result
        .recent_tasks
        .iter()
        .flat_map(|item| item.evidence_activity_ids.iter())
        .chain(
            result
                .frequent_patterns
                .iter()
                .flat_map(|item| item.evidence_activity_ids.iter()),
        )
    {
        if !input_ids.contains(evidence_id) {
            return Err(AnalyzerError::UnknownEvidence);
        }
        if !accepted.insert(evidence_id.clone()) {
            return Err(AnalyzerError::DuplicateEvidence);
        }
    }
    if accepted.iter().any(|id| discarded.contains(id)) {
        return Err(AnalyzerError::DiscardEvidenceOverlap);
    }

    let mut snapshots = Vec::new();
    let mut recent_tasks = state.profile.recent_tasks.clone();
    for item in result.recent_tasks {
        validate_text(&item.summary, &item.prompt_text)?;
        let evidence = evidence_records(&item.evidence_activity_ids, &records)?;
        validate_agent_run_evidence(&evidence)?;
        validate_prior_task(
            &item.prior_task_id,
            &item.prior_snapshot_ids,
            &state.profile,
        )?;
        validate_generated_text_grounding(
            &item.summary,
            &item.prompt_text,
            &evidence,
            &item.prior_task_id,
            &state.profile,
        )?;
        validate_declared_entities(&item.summary, &item.prompt_text, &item.entities)?;
        validate_entities(
            &item.entities,
            &evidence,
            &item.prior_task_id,
            &state.profile,
        )?;
        let snapshot = make_snapshot(
            ids.next_id("snapshot"),
            &evidence,
            item.entities.clone(),
            input.trusted_now_hour_bucket,
        );
        let task_id = item
            .prior_task_id
            .clone()
            .unwrap_or_else(|| ids.next_id("task"));
        upsert_recent(
            &mut recent_tasks,
            RecentTask {
                task_id,
                summary: item.summary,
                entities: item.entities,
                context_affinity: snapshot.context_affinity.clone(),
                last_seen_hour_bucket: snapshot.last_seen_hour_bucket,
                evidence_snapshot_ids: merged_snapshot_ids(
                    &item.prior_snapshot_ids,
                    &snapshot.snapshot_id,
                ),
                prompt_text: item.prompt_text,
            },
        );
        snapshots.push(snapshot);
    }

    let mut frequent_patterns = state.profile.frequent_patterns.clone();
    for item in result.frequent_patterns {
        validate_text(&item.summary, &item.prompt_text)?;
        let evidence = evidence_records(&item.evidence_activity_ids, &records)?;
        validate_agent_run_evidence(&evidence)?;
        if item
            .stable_entities
            .iter()
            .any(|entity| entity.volatility == super::personal_model::EntityVolatility::Ephemeral)
        {
            return Err(AnalyzerError::InvalidFrequentEvidence);
        }
        validate_prior_pattern(
            &item.prior_pattern_id,
            &item.prior_snapshot_ids,
            &state.profile,
        )?;
        validate_generated_text_grounding(
            &item.summary,
            &item.prompt_text,
            &evidence,
            &item.prior_pattern_id,
            &state.profile,
        )?;
        validate_declared_entities(&item.summary, &item.prompt_text, &item.stable_entities)?;
        validate_entities(
            &item.stable_entities,
            &evidence,
            &item.prior_pattern_id,
            &state.profile,
        )?;
        let snapshot = make_snapshot(
            ids.next_id("snapshot"),
            &evidence,
            item.stable_entities.clone(),
            input.trusted_now_hour_bucket,
        );
        let active_days = merged_active_days(
            &item.prior_snapshot_ids,
            &snapshot,
            &state.profile,
            input.trusted_now_hour_bucket,
        );
        if active_days.len() < 3 {
            return Err(AnalyzerError::InvalidFrequentEvidence);
        }
        let pattern_id = item
            .prior_pattern_id
            .clone()
            .unwrap_or_else(|| ids.next_id("pattern"));
        upsert_pattern(
            &mut frequent_patterns,
            FrequentPattern {
                pattern_id,
                summary: item.summary,
                stable_entities: item.stable_entities,
                active_day_buckets: active_days,
                context_affinity: snapshot.context_affinity.clone(),
                evidence_snapshot_ids: merged_snapshot_ids(
                    &item.prior_snapshot_ids,
                    &snapshot.snapshot_id,
                ),
                prompt_text: item.prompt_text,
            },
        );
        snapshots.push(snapshot);
    }

    let mut all_snapshots = state.profile.evidence_snapshots.clone();
    all_snapshots.extend(snapshots.clone());
    if recent_tasks.len() + frequent_patterns.len() > 20 {
        return Err(AnalyzerError::CapacityExceeded);
    }
    roll_profile_snapshots(
        &mut all_snapshots,
        &mut recent_tasks,
        &mut frequent_patterns,
        input.trusted_now_hour_bucket,
    );
    let next_generation = state.profile.summary_generation.saturating_add(1);
    let cache = build_cache(
        &recent_tasks,
        &frequent_patterns,
        &all_snapshots,
        next_generation,
        input.trusted_now_hour_bucket,
        ids,
    );
    let bounded_state_hour = state.updated_hour_bucket.min(input.trusted_now_hour_bucket);
    let updated_hour_bucket = records
        .values()
        .filter_map(|record| trusted_activity_hour(record, input.trusted_now_hour_bucket))
        .max()
        .unwrap_or(bounded_state_hour)
        .max(bounded_state_hour);
    let mut consumed_activity_ids = accepted.into_iter().collect::<Vec<_>>();
    consumed_activity_ids.extend(discarded);
    consumed_activity_ids.sort();
    Ok(MergePatch {
        base_epoch: input.base_epoch.clone(),
        base_profile_generation: input.base_profile_generation,
        consumed_activity_ids,
        evidence_snapshots: all_snapshots,
        recent_tasks,
        frequent_patterns,
        cache,
        updated_hour_bucket,
    })
}

pub(crate) fn apply_merge_patch(
    state: &mut RecommendationState,
    patch: MergePatch,
) -> Result<(), AnalyzerError> {
    if state.store_epoch != patch.base_epoch {
        return Err(AnalyzerError::StaleEpoch);
    }
    if state.profile.summary_generation != patch.base_profile_generation {
        return Err(AnalyzerError::StaleProfile);
    }
    if !patch.consumed_activity_ids.iter().all(|id| {
        state
            .journal
            .records
            .iter()
            .any(|record| record.activity_id == *id)
    }) {
        return Err(AnalyzerError::UnknownEvidence);
    }
    let mut next = state.clone();
    let summary_generation = next.profile.summary_generation.saturating_add(1);
    for record in &mut next.journal.records {
        if patch.consumed_activity_ids.contains(&record.activity_id) {
            record.summarized_generation = Some(summary_generation);
        }
    }
    next.profile.summary_generation = summary_generation;
    next.profile.updated_hour_bucket = patch.updated_hour_bucket;
    next.profile.evidence_snapshots = patch.evidence_snapshots;
    next.profile.recent_tasks = patch.recent_tasks;
    next.profile.frequent_patterns = patch.frequent_patterns;
    next.cache = patch.cache;
    next.generation = next.generation.saturating_add(1);
    next.updated_hour_bucket = patch.updated_hour_bucket;
    *state = next;
    Ok(())
}

fn build_cache(
    recent: &[RecentTask],
    frequent: &[FrequentPattern],
    snapshots: &[EvidenceSnapshot],
    generation: u64,
    trusted_now_hour_bucket: u64,
    ids: &mut impl LocalIdSource,
) -> RecommendationCache {
    personal_profile_policy::build_cache(
        recent,
        frequent,
        snapshots,
        generation,
        trusted_now_hour_bucket,
        |_, _| ids.next_id("candidate"),
    )
}

#[path = "personal_analyzer_input.rs"]
mod input;
#[path = "personal_analyzer_profile.rs"]
mod profile;
use input::*;
use profile::*;
#[cfg(test)]
#[path = "personal_analyzer_tests.rs"]
mod tests;
