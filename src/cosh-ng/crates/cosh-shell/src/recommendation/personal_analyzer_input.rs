use super::*;

pub(super) fn is_trigger(record: &ActivityRecord) -> bool {
    match &record.payload {
        ActivityPayload::AgentRequest { .. } => true,
        ActivityPayload::RecommendationFeedback { action, .. } => {
            *action == crate::recommendation::personal_model::FeedbackAction::Submitted
        }
        _ => false,
    }
}

pub(super) fn fit_trigger(mut record: ActivityRecord) -> Result<ActivityRecord, AnalyzerError> {
    while serde_json::to_vec(&record)
        .map_err(|_| AnalyzerError::InputTooLarge)?
        .len()
        > TRIGGER_BUDGET
    {
        let ActivityPayload::AgentRequest { text, .. } = &mut record.payload else {
            return Err(AnalyzerError::InputTooLarge);
        };
        if text.len() < 32 {
            return Err(AnalyzerError::InputTooLarge);
        }
        let next_len = text.len() * 3 / 4;
        text.truncate(floor_char_boundary(text, next_len));
    }
    Ok(record)
}

pub(super) fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn bounded_profile_text(value: &str, max_bytes: usize) -> String {
    value[..floor_char_boundary(value, max_bytes)].to_string()
}

pub(super) fn activity_priority(record: &ActivityRecord, context: &ActivityContext) -> u8 {
    match &record.payload {
        ActivityPayload::AgentRequest { .. } => 5,
        ActivityPayload::ShellCommand {
            outcome: ActivityOutcome::Failure,
            ..
        } => 4,
        ActivityPayload::RecommendationFeedback { action, .. }
            if *action == crate::recommendation::personal_model::FeedbackAction::Submitted =>
        {
            3
        }
        _ if record.context.repo_id == context.repo_id
            || record.context.host_id == context.host_id =>
        {
            2
        }
        ActivityPayload::BashHistoryCommand { .. } => 1,
        _ => 0,
    }
}

pub(super) fn project_profile(
    profile: &UserWorkProfile,
    context: &ActivityContext,
) -> ProfileProjection {
    let mut projection = ProfileProjection {
        summary_generation: profile.summary_generation,
        ..ProfileProjection::default()
    };
    for task in &profile.recent_tasks {
        if affinity_matches(&task.context_affinity, context) {
            let item = ProjectedProfileItem {
                id: task.task_id.clone(),
                summary: bounded_profile_text(&task.summary, 256),
                prompt_text: bounded_profile_text(&task.prompt_text, 512),
                entities: task.entities.clone(),
                snapshot_ids: task.evidence_snapshot_ids.clone(),
            };
            projection.recent_tasks.push(item);
            if serde_json::to_vec(&projection).map_or(true, |json| json.len() > PROFILE_BUDGET) {
                projection.recent_tasks.pop();
                break;
            }
        }
    }
    for pattern in &profile.frequent_patterns {
        if affinity_matches(&pattern.context_affinity, context) {
            let item = ProjectedProfileItem {
                id: pattern.pattern_id.clone(),
                summary: bounded_profile_text(&pattern.summary, 256),
                prompt_text: bounded_profile_text(&pattern.prompt_text, 512),
                entities: pattern.stable_entities.clone(),
                snapshot_ids: pattern.evidence_snapshot_ids.clone(),
            };
            projection.frequent_patterns.push(item);
            if serde_json::to_vec(&projection).map_or(true, |json| json.len() > PROFILE_BUDGET) {
                projection.frequent_patterns.pop();
                break;
            }
        }
    }
    projection
}

pub(super) fn affinity_matches(affinity: &ContextAffinity, context: &ActivityContext) -> bool {
    match affinity.scope_kind {
        ScopeKind::Repo => affinity.repo_id == context.repo_id,
        ScopeKind::HostFallback | ScopeKind::HostWide => affinity.host_id == context.host_id,
    }
}

pub(super) fn project_feedback(
    values: &[RecommendationFeedbackState],
) -> Vec<RecommendationFeedbackState> {
    let mut projected = Vec::new();
    for value in values.iter().rev() {
        projected.push(value.clone());
        if serde_json::to_vec(&projected).map_or(true, |json| json.len() > FEEDBACK_BUDGET) {
            projected.pop();
            break;
        }
    }
    projected
}

pub(super) fn serialize_envelope(
    profile: &ProfileProjection,
    activities: &[ActivityRecord],
    feedback: &[RecommendationFeedbackState],
) -> Result<String, AnalyzerError> {
    serde_json::to_string(&AnalyzerEnvelope {
        previous_profile: profile.clone(),
        new_activities: activities.to_vec(),
        feedback_summary: feedback.to_vec(),
        current_limits: AnalyzerLimits {
            max_recent_tasks: 10,
            max_frequent_patterns: 10,
            max_candidates: 10,
        },
    })
    .map_err(|_| AnalyzerError::InputTooLarge)
}
