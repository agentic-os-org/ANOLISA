use super::*;

pub(super) fn evidence_records<'a>(
    ids: &[String],
    records: &HashMap<String, &'a ActivityRecord>,
) -> Result<Vec<&'a ActivityRecord>, AnalyzerError> {
    if ids.is_empty() {
        return Err(AnalyzerError::EmptyEvidence);
    }
    ids.iter()
        .map(|id| {
            records
                .get(id)
                .copied()
                .ok_or(AnalyzerError::UnknownEvidence)
        })
        .collect()
}

pub(super) fn validate_text(summary: &str, prompt: &str) -> Result<(), AnalyzerError> {
    if summary.len() > 256 || prompt.len() > 512 {
        return Err(AnalyzerError::CapacityExceeded);
    }
    if !is_single_printable_line(summary) || !is_single_printable_line(prompt) {
        return Err(AnalyzerError::UnsafeTerminalText);
    }
    if contains_hard_secret(summary) || contains_hard_secret(prompt) {
        return Err(AnalyzerError::HardSecret);
    }
    Ok(())
}

fn is_single_printable_line(value: &str) -> bool {
    value
        .chars()
        .all(|character| !character.is_control() && !matches!(character, '\u{2028}' | '\u{2029}'))
}

pub(super) fn validate_agent_run_evidence(
    evidence: &[&ActivityRecord],
) -> Result<(), AnalyzerError> {
    for record in evidence {
        let ActivityPayload::AgentRun {
            request_activity_id,
            ..
        } = &record.payload
        else {
            continue;
        };
        let linked = record.source == super::ActivitySource::AgentRun
            && evidence.iter().any(|candidate| {
                candidate.source == super::ActivitySource::AgentRequest
                    && candidate.activity_id == *request_activity_id
                    && matches!(candidate.payload, ActivityPayload::AgentRequest { .. })
            });
        if !linked {
            return Err(AnalyzerError::InvalidAgentRunEvidence);
        }
    }
    Ok(())
}

pub(super) fn validate_prior_task(
    prior_id: &Option<String>,
    snapshots: &[String],
    profile: &UserWorkProfile,
) -> Result<(), AnalyzerError> {
    validate_prior(
        prior_id,
        snapshots,
        profile
            .recent_tasks
            .iter()
            .map(|task| (&task.task_id, &task.evidence_snapshot_ids)),
    )
}

pub(super) fn validate_prior_pattern(
    prior_id: &Option<String>,
    snapshots: &[String],
    profile: &UserWorkProfile,
) -> Result<(), AnalyzerError> {
    validate_prior(
        prior_id,
        snapshots,
        profile
            .frequent_patterns
            .iter()
            .map(|pattern| (&pattern.pattern_id, &pattern.evidence_snapshot_ids)),
    )
}

pub(super) fn validate_prior<'a>(
    prior_id: &Option<String>,
    snapshots: &[String],
    candidates: impl Iterator<Item = (&'a String, &'a Vec<String>)>,
) -> Result<(), AnalyzerError> {
    match prior_id {
        None if snapshots.is_empty() => Ok(()),
        Some(id) => {
            let Some((_, owned)) = candidates
                .into_iter()
                .find(|(candidate, _)| *candidate == id)
            else {
                return Err(AnalyzerError::InvalidPriorReference);
            };
            if snapshots.iter().all(|snapshot| owned.contains(snapshot)) {
                Ok(())
            } else {
                Err(AnalyzerError::InvalidPriorReference)
            }
        }
        None => Err(AnalyzerError::InvalidPriorReference),
    }
}

pub(super) fn validate_entities(
    entities: &[TaskEntity],
    evidence: &[&ActivityRecord],
    prior_id: &Option<String>,
    profile: &UserWorkProfile,
) -> Result<(), AnalyzerError> {
    let evidence_text = evidence
        .iter()
        .filter_map(|record| allowed_business_text(record))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let prior_entities = prior_id
        .as_ref()
        .and_then(|id| {
            profile
                .recent_tasks
                .iter()
                .find(|task| &task.task_id == id)
                .map(|task| &task.entities)
                .or_else(|| {
                    profile
                        .frequent_patterns
                        .iter()
                        .find(|pattern| &pattern.pattern_id == id)
                        .map(|pattern| &pattern.stable_entities)
                })
        })
        .into_iter()
        .flatten()
        .map(|entity| entity.value.to_lowercase())
        .collect::<HashSet<_>>();
    for entity in entities {
        if contains_hard_secret(&entity.value) {
            return Err(AnalyzerError::HardSecret);
        }
        let value = entity.value.to_lowercase();
        if !evidence_text.contains(&value) && !prior_entities.contains(&value) {
            return Err(AnalyzerError::UngroundedEntity);
        }
    }
    Ok(())
}

pub(super) fn validate_generated_text_grounding(
    summary: &str,
    prompt: &str,
    evidence: &[&ActivityRecord],
    prior_id: &Option<String>,
    profile: &UserWorkProfile,
) -> Result<(), AnalyzerError> {
    let mut grounding_text = evidence
        .iter()
        .filter_map(|record| allowed_business_text(record))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(id) = prior_id {
        if let Some(task) = profile.recent_tasks.iter().find(|task| &task.task_id == id) {
            grounding_text.push_str(&task.summary);
            grounding_text.push_str(&task.prompt_text);
            for entity in &task.entities {
                grounding_text.push_str(&entity.value);
            }
        } else if let Some(pattern) = profile
            .frequent_patterns
            .iter()
            .find(|pattern| &pattern.pattern_id == id)
        {
            grounding_text.push_str(&pattern.summary);
            grounding_text.push_str(&pattern.prompt_text);
            for entity in &pattern.stable_entities {
                grounding_text.push_str(&entity.value);
            }
        }
    }
    let grounding_text = grounding_text.to_lowercase();
    for token in identifier_like_tokens(summary).chain(identifier_like_tokens(prompt)) {
        if !grounding_text.contains(&token) {
            return Err(AnalyzerError::UngroundedEntity);
        }
    }
    Ok(())
}

pub(super) fn validate_declared_entities(
    summary: &str,
    prompt: &str,
    entities: &[TaskEntity],
) -> Result<(), AnalyzerError> {
    let generated_text = format!("{summary}\n{prompt}").to_lowercase();
    if entities
        .iter()
        .all(|entity| generated_text.contains(&entity.value.to_lowercase()))
    {
        Ok(())
    } else {
        Err(AnalyzerError::UngroundedEntity)
    }
}

fn identifier_like_tokens(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(|character: char| {
            !(character.is_alphanumeric() || matches!(character, '-' | '_' | '/' | '.'))
        })
        .map(|token| token.trim_matches(['-', '_', '/', '.']).to_lowercase())
        .filter(|token| {
            token.len() >= 3
                && token.chars().any(char::is_alphabetic)
                && (token.contains(['-', '_', '/', '.'])
                    || token.chars().any(|character| character.is_ascii_digit()))
        })
}

fn allowed_business_text(record: &ActivityRecord) -> Option<&str> {
    match &record.payload {
        ActivityPayload::ShellCommand { command, .. }
        | ActivityPayload::BashHistoryCommand { command, .. } => Some(command),
        ActivityPayload::AgentRequest { text, .. } => Some(text),
        ActivityPayload::AgentRun { .. } | ActivityPayload::RecommendationFeedback { .. } => None,
    }
}

pub(super) fn make_snapshot(
    snapshot_id: String,
    evidence: &[&ActivityRecord],
    entities: Vec<TaskEntity>,
    trusted_now_hour_bucket: u64,
) -> EvidenceSnapshot {
    let mut source_kinds = evidence
        .iter()
        .map(|record| record.source)
        .collect::<Vec<_>>();
    source_kinds.sort_by_key(|source| format!("{source:?}"));
    source_kinds.dedup();
    let mut active_day_buckets = evidence
        .iter()
        .filter_map(|record| activity_day(record, trusted_now_hour_bucket))
        .collect::<Vec<_>>();
    active_day_buckets.sort_unstable();
    active_day_buckets.dedup();
    let first_seen_hour_bucket = evidence
        .iter()
        .filter_map(|record| trusted_activity_hour(record, trusted_now_hour_bucket))
        .min()
        .unwrap_or(0);
    let last_seen_hour_bucket = evidence
        .iter()
        .filter_map(|record| trusted_activity_hour(record, trusted_now_hour_bucket))
        .max()
        .unwrap_or(0);
    let intents = evidence
        .iter()
        .filter(|record| trusted_activity_hour(record, trusted_now_hour_bucket).is_some())
        .filter_map(|record| intent_id(record))
        .collect::<HashSet<_>>();
    EvidenceSnapshot {
        snapshot_id,
        source_kinds,
        first_seen_hour_bucket,
        last_seen_hour_bucket,
        active_day_buckets,
        context_affinity: recompute_affinity(evidence),
        entities,
        agent_request_count: saturating_count(
            evidence
                .iter()
                .filter(|r| matches!(r.payload, ActivityPayload::AgentRequest { .. })),
        ),
        compatible_shell_count: saturating_count(
            evidence
                .iter()
                .filter(|r| matches!(r.payload, ActivityPayload::ShellCommand { .. })),
        ),
        submitted_feedback_count: saturating_count(evidence.iter().filter(|r| {
            matches!(
                r.payload,
                ActivityPayload::RecommendationFeedback {
                    action: crate::recommendation::personal_model::FeedbackAction::Submitted,
                    ..
                }
            )
        })),
        intent_occurrence_count: intents.len().min(u16::MAX as usize) as u16,
        last_action_failed: evidence
            .iter()
            .filter_map(|record| {
                trusted_activity_hour(record, trusted_now_hour_bucket).zip(activity_failed(record))
            })
            .max_by_key(|(hour, _)| *hour)
            .is_some_and(|(_, failed)| failed),
    }
}

pub(super) fn activity_failed(record: &ActivityRecord) -> Option<bool> {
    match &record.payload {
        ActivityPayload::ShellCommand { outcome, .. }
        | ActivityPayload::AgentRun { outcome, .. } => Some(*outcome == ActivityOutcome::Failure),
        _ => None,
    }
}

pub(super) fn saturating_count<'a>(values: impl Iterator<Item = &'a &'a ActivityRecord>) -> u16 {
    values.count().min(u16::MAX as usize) as u16
}

pub(super) fn intent_id(record: &ActivityRecord) -> Option<&str> {
    match &record.payload {
        ActivityPayload::AgentRequest {
            intent_lifecycle_id,
            ..
        }
        | ActivityPayload::RecommendationFeedback {
            intent_lifecycle_id,
            ..
        } => Some(intent_lifecycle_id),
        _ => None,
    }
}

pub(super) fn trusted_activity_hour(
    record: &ActivityRecord,
    trusted_now_hour_bucket: u64,
) -> Option<u64> {
    let hour = match &record.payload {
        ActivityPayload::BashHistoryCommand {
            execution_hour_bucket,
            time_unverified,
            ..
        } => (!*time_unverified)
            .then_some(*execution_hour_bucket)
            .flatten(),
        ActivityPayload::AgentRun { .. } => None,
        _ => Some(record.observed_hour_bucket),
    }?;
    (hour <= trusted_now_hour_bucket).then_some(hour)
}

pub(super) fn activity_day(record: &ActivityRecord, trusted_now_hour_bucket: u64) -> Option<u32> {
    trusted_activity_hour(record, trusted_now_hour_bucket)
        .map(|hour| (hour / 24).min(u32::MAX as u64) as u32)
}

pub(super) fn recompute_affinity(evidence: &[&ActivityRecord]) -> ContextAffinity {
    let repos = evidence
        .iter()
        .filter_map(|record| record.context.repo_id.clone())
        .collect::<HashSet<_>>();
    let hosts = evidence
        .iter()
        .filter_map(|record| record.context.host_id.clone())
        .collect::<HashSet<_>>();
    let all_have_repo = evidence
        .iter()
        .all(|record| record.context.repo_id.is_some());
    if all_have_repo && repos.len() == 1 {
        ContextAffinity {
            scope_kind: ScopeKind::Repo,
            repo_id: repos.into_iter().next(),
            host_id: one_value(hosts),
        }
    } else if repos.len() > 1 {
        ContextAffinity {
            scope_kind: ScopeKind::HostWide,
            repo_id: None,
            host_id: one_value(hosts),
        }
    } else {
        ContextAffinity {
            scope_kind: ScopeKind::HostFallback,
            repo_id: repos.into_iter().next(),
            host_id: one_value(hosts),
        }
    }
}

pub(super) fn one_value(values: HashSet<String>) -> Option<String> {
    (values.len() == 1)
        .then(|| values.into_iter().next())
        .flatten()
}

pub(super) fn merged_snapshot_ids(prior: &[String], new_id: &str) -> Vec<String> {
    let mut ids = prior.to_vec();
    ids.push(new_id.to_string());
    ids
}

pub(super) fn merged_active_days(
    prior_ids: &[String],
    snapshot: &EvidenceSnapshot,
    profile: &UserWorkProfile,
    trusted_now_hour_bucket: u64,
) -> Vec<u32> {
    let mut days = profile
        .evidence_snapshots
        .iter()
        .filter(|candidate| prior_ids.contains(&candidate.snapshot_id))
        .flat_map(|candidate| candidate.active_day_buckets.iter().copied())
        .chain(snapshot.active_day_buckets.iter().copied())
        .collect::<Vec<_>>();
    days.sort_unstable();
    days.dedup();
    days.retain(|day| u64::from(*day) <= trusted_now_hour_bucket / 24);
    days
}

pub(super) fn upsert_recent(tasks: &mut Vec<RecentTask>, value: RecentTask) {
    if let Some(existing) = tasks.iter_mut().find(|task| task.task_id == value.task_id) {
        *existing = value;
    } else {
        tasks.push(value);
    }
}

pub(super) fn upsert_pattern(patterns: &mut Vec<FrequentPattern>, value: FrequentPattern) {
    if let Some(existing) = patterns
        .iter_mut()
        .find(|pattern| pattern.pattern_id == value.pattern_id)
    {
        *existing = value;
    } else {
        patterns.push(value);
    }
}
