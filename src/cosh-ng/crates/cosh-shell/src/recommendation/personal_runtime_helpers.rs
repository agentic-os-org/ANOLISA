use super::*;

pub(super) fn clear_store_with_retry(
    store: &PersonalStore,
    now_hour_bucket: u64,
    mut settle_observed_lease: impl FnMut(Option<&AnalyzerLease>) -> Result<(), PersonalRuntimeError>,
) -> Result<RecommendationState, PersonalRuntimeError> {
    for _ in 0..MAX_CLEAR_CAS_ATTEMPTS {
        let current = match store.load(now_hour_bucket)? {
            Some(current) => current,
            None => store.initialize(now_hour_bucket)?,
        };
        settle_observed_lease(current.scheduler.lease.as_ref())?;
        match store.clear_if_current(&StateVersion::of(&current), now_hour_bucket) {
            Ok(cleared) => return Ok(cleared),
            Err(StoreError::StaleState) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(StoreError::StaleState.into())
}

impl SourceCounts {
    pub(super) fn increment(&mut self, source: ActivitySource) {
        let counter = match source {
            ActivitySource::ShellCommand => &mut self.shell_command,
            ActivitySource::AgentRequest => &mut self.agent_request,
            ActivitySource::AgentRun => &mut self.agent_run,
            ActivitySource::RecommendationFeedback => &mut self.recommendation_feedback,
            ActivitySource::BashHistory => &mut self.bash_history,
        };
        *counter = counter.saturating_add(1);
    }
}

pub(super) fn merge_records(
    state: &mut RecommendationState,
    records: &[ActivityRecord],
    now_hour_bucket: u64,
) {
    for record in records {
        if state.journal.records.iter().any(|existing| {
            existing.activity_id == record.activity_id
                || existing.source_fingerprint == record.source_fingerprint
        }) {
            continue;
        }
        merge_feedback_state(state, record, now_hour_bucket);
        state.journal.records.push(record.clone());
    }
}

pub(super) fn merge_feedback_state(
    state: &mut RecommendationState,
    record: &ActivityRecord,
    now_hour_bucket: u64,
) {
    let ActivityPayload::RecommendationFeedback {
        task_ref, action, ..
    } = &record.payload
    else {
        return;
    };
    let index = state
        .feedback
        .iter()
        .position(|feedback| feedback.task_ref == *task_ref)
        .unwrap_or_else(|| {
            state.feedback.push(RecommendationFeedbackState {
                task_ref: task_ref.clone(),
                last_impression_hour_bucket: None,
                last_submitted_hour_bucket: None,
                consecutive_explicit_dismissals: 0,
                last_explicit_dismissal_hour_bucket: None,
                consecutive_overrides: 0,
                last_override_hour_bucket: None,
            });
            state.feedback.len() - 1
        });
    let feedback = &mut state.feedback[index];
    match action {
        FeedbackAction::Impression => {
            feedback.last_impression_hour_bucket = Some(now_hour_bucket);
        }
        FeedbackAction::Submitted => {
            feedback.last_submitted_hour_bucket = Some(now_hour_bucket);
            feedback.consecutive_explicit_dismissals = 0;
            feedback.last_explicit_dismissal_hour_bucket = None;
            feedback.consecutive_overrides = 0;
            feedback.last_override_hour_bucket = None;
        }
        FeedbackAction::ExplicitDismissed => {
            feedback.consecutive_explicit_dismissals =
                feedback.consecutive_explicit_dismissals.saturating_add(1);
            feedback.last_explicit_dismissal_hour_bucket = Some(now_hour_bucket);
            feedback.consecutive_overrides = 0;
            feedback.last_override_hour_bucket = None;
        }
        FeedbackAction::Overridden => {
            feedback.consecutive_overrides = feedback.consecutive_overrides.saturating_add(1);
            feedback.last_override_hour_bucket = Some(now_hour_bucket);
            feedback.consecutive_explicit_dismissals = 0;
            feedback.last_explicit_dismissal_hour_bucket = None;
        }
        FeedbackAction::Ignored => {
            feedback.consecutive_explicit_dismissals = 0;
            feedback.last_explicit_dismissal_hour_bucket = None;
            feedback.consecutive_overrides = 0;
            feedback.last_override_hour_bucket = None;
        }
        FeedbackAction::TabAccepted => {}
    }
    if state.feedback.len() > MAX_FEEDBACK_STATES {
        let remove = state.feedback.len() - MAX_FEEDBACK_STATES;
        state.feedback.drain(..remove);
    }
}

pub(super) fn planner_candidate(
    candidate: &CachedPromptCandidate,
    feedback: &[RecommendationFeedbackState],
    now_hour_bucket: u64,
) -> PlannerCandidate {
    let feedback = feedback
        .iter()
        .find(|feedback| feedback.task_ref == candidate.task_ref);
    let consecutive_explicit_dismissals = feedback
        .map(|feedback| feedback.consecutive_explicit_dismissals)
        .unwrap_or(0);
    let suppressed =
        feedback.is_some_and(|feedback| {
            feedback.last_impression_hour_bucket.is_some_and(|hour| {
                now_hour_bucket < hour.saturating_add(IMPRESSION_SUPPRESSION_HOURS)
            }) || feedback.last_submitted_hour_bucket.is_some_and(|hour| {
                now_hour_bucket < hour.saturating_add(SUBMITTED_SUPPRESSION_HOURS)
            }) || (feedback.consecutive_explicit_dismissals >= 2
                && feedback
                    .last_explicit_dismissal_hour_bucket
                    .is_some_and(|hour| now_hour_bucket < hour.saturating_add(7 * 24)))
                || (feedback.consecutive_overrides >= 3
                    && feedback
                        .last_override_hour_bucket
                        .is_some_and(|hour| now_hour_bucket < hour.saturating_add(24)))
        });
    PlannerCandidate {
        candidate_id: candidate.candidate_id.clone(),
        source: candidate.source,
        task_ref: candidate.task_ref.clone(),
        prompt_text: candidate.prompt_text.clone(),
        context_affinity: candidate.context_affinity.clone(),
        last_seen_hour_bucket: candidate.last_seen_hour_bucket,
        evidence: candidate.evidence.clone(),
        entities: candidate.entities.clone(),
        suppression_key: candidate.task_ref.clone(),
        last_action_failed: candidate.last_action_failed,
        consecutive_explicit_dismissals,
        suppressed,
    }
}

pub(super) fn clear_history_derived(state: &mut RecommendationState) {
    state
        .journal
        .records
        .retain(|record| record.source != ActivitySource::BashHistory);
    state.profile = UserWorkProfile::default();
    state.cache = RecommendationCache::default();
    state.feedback.clear();
}

pub(super) fn is_weak(record: &ActivityRecord) -> bool {
    match &record.payload {
        ActivityPayload::AgentRequest { .. } => false,
        ActivityPayload::ShellCommand { outcome, .. } => *outcome != ActivityOutcome::Failure,
        ActivityPayload::RecommendationFeedback { action, .. } => !matches!(
            action,
            FeedbackAction::Submitted
                | FeedbackAction::ExplicitDismissed
                | FeedbackAction::Overridden
        ),
        ActivityPayload::AgentRun { .. } | ActivityPayload::BashHistoryCommand { .. } => true,
    }
}

pub(super) fn source_tag(source: ActivitySource) -> u8 {
    match source {
        ActivitySource::ShellCommand => 1,
        ActivitySource::AgentRequest => 2,
        ActivitySource::AgentRun => 3,
        ActivitySource::RecommendationFeedback => 4,
        ActivitySource::BashHistory => 5,
    }
}
