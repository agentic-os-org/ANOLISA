use super::personal_model::{CandidateSource, EditBucket, FeedbackAction};

#[cfg(test)]
const IMPRESSION_SUPPRESSION_HOURS: u64 = 8;
#[cfg(test)]
const SUBMITTED_SUPPRESSION_HOURS: u64 = 2;
#[cfg(test)]
const DISMISSED_SUPPRESSION_HOURS: u64 = 7 * 24;
#[cfg(test)]
const OVERRIDDEN_SUPPRESSION_HOURS: u64 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenPromptBinding {
    pub(crate) candidate_id: String,
    pub(crate) task_ref: String,
    pub(crate) original_prompt: String,
    pub(crate) source: CandidateSource,
    pub(crate) suppression_key: String,
    pub(crate) profile_generation: u64,
    pub(crate) intent_lifecycle_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeedbackEvent {
    pub(crate) candidate_id: String,
    pub(crate) candidate_source: CandidateSource,
    pub(crate) task_ref: String,
    pub(crate) profile_generation: u64,
    pub(crate) intent_lifecycle_id: String,
    pub(crate) action: FeedbackAction,
    pub(crate) edit_bucket: Option<EditBucket>,
}

#[derive(Debug, Clone)]
pub(crate) struct FeedbackLifecycle {
    binding: FrozenPromptBinding,
    #[cfg(test)]
    impression_emitted: bool,
    accepted: bool,
    terminal: bool,
}

impl FeedbackLifecycle {
    pub(crate) fn new(binding: FrozenPromptBinding) -> Self {
        Self {
            binding,
            #[cfg(test)]
            impression_emitted: false,
            accepted: false,
            terminal: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn impression(&mut self) -> Option<FeedbackEvent> {
        if self.impression_emitted || self.terminal {
            return None;
        }
        self.impression_emitted = true;
        Some(self.event(FeedbackAction::Impression, None))
    }

    pub(crate) fn accept(&mut self) -> Option<FeedbackEvent> {
        if self.accepted || self.terminal {
            return None;
        }
        self.accepted = true;
        Some(self.event(FeedbackAction::TabAccepted, None))
    }

    pub(crate) fn submit(&mut self, final_text: &str) -> Option<FeedbackEvent> {
        if self.terminal {
            return None;
        }
        let (action, edit_bucket) = if self.accepted {
            let edit_bucket = classify_edit(&self.binding.original_prompt, final_text);
            let action = if edit_bucket == EditBucket::Large {
                FeedbackAction::Overridden
            } else {
                FeedbackAction::Submitted
            };
            (action, Some(edit_bucket))
        } else {
            (FeedbackAction::Submitted, None)
        };
        self.terminal = true;
        Some(self.event(action, edit_bucket))
    }

    pub(crate) fn explicit_dismiss(&mut self) -> Option<FeedbackEvent> {
        if !self.accepted || self.terminal {
            return None;
        }
        self.terminal = true;
        Some(self.event(FeedbackAction::ExplicitDismissed, None))
    }

    pub(crate) fn ignore(&mut self) -> Option<FeedbackEvent> {
        if self.terminal {
            return None;
        }
        self.terminal = true;
        Some(self.event(FeedbackAction::Ignored, None))
    }

    fn event(&self, action: FeedbackAction, edit_bucket: Option<EditBucket>) -> FeedbackEvent {
        FeedbackEvent {
            candidate_id: self.binding.candidate_id.clone(),
            candidate_source: self.binding.source,
            task_ref: self.binding.task_ref.clone(),
            profile_generation: self.binding.profile_generation,
            intent_lifecycle_id: self.binding.intent_lifecycle_id.clone(),
            action,
            edit_bucket,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeedbackSuppressionState {
    pub(crate) task_ref: String,
    pub(crate) last_impression_hour_bucket: Option<u64>,
    pub(crate) last_submitted_hour_bucket: Option<u64>,
    pub(crate) consecutive_explicit_dismissals: u8,
    pub(crate) last_explicit_dismissal_hour_bucket: Option<u64>,
    pub(crate) consecutive_overrides: u8,
    pub(crate) last_override_hour_bucket: Option<u64>,
}

#[cfg(test)]
impl FeedbackSuppressionState {
    pub(crate) fn new(task_ref: impl Into<String>) -> Self {
        Self {
            task_ref: task_ref.into(),
            last_impression_hour_bucket: None,
            last_submitted_hour_bucket: None,
            consecutive_explicit_dismissals: 0,
            last_explicit_dismissal_hour_bucket: None,
            consecutive_overrides: 0,
            last_override_hour_bucket: None,
        }
    }

    pub(crate) fn record(&mut self, action: FeedbackAction, hour_bucket: u64) {
        match action {
            FeedbackAction::Impression => {
                self.last_impression_hour_bucket = Some(hour_bucket);
            }
            FeedbackAction::Submitted => {
                self.last_submitted_hour_bucket = Some(hour_bucket);
                self.consecutive_explicit_dismissals = 0;
                self.last_explicit_dismissal_hour_bucket = None;
                self.consecutive_overrides = 0;
                self.last_override_hour_bucket = None;
            }
            FeedbackAction::ExplicitDismissed => {
                self.consecutive_explicit_dismissals =
                    self.consecutive_explicit_dismissals.saturating_add(1);
                self.last_explicit_dismissal_hour_bucket = Some(hour_bucket);
                self.consecutive_overrides = 0;
                self.last_override_hour_bucket = None;
            }
            FeedbackAction::Overridden => {
                self.consecutive_overrides = self.consecutive_overrides.saturating_add(1);
                self.last_override_hour_bucket = Some(hour_bucket);
                self.consecutive_explicit_dismissals = 0;
                self.last_explicit_dismissal_hour_bucket = None;
            }
            FeedbackAction::Ignored => {
                self.consecutive_explicit_dismissals = 0;
                self.last_explicit_dismissal_hour_bucket = None;
                self.consecutive_overrides = 0;
                self.last_override_hour_bucket = None;
            }
            FeedbackAction::TabAccepted => {}
        }
    }

    pub(crate) fn is_suppressed(&self, now_hour_bucket: u64) -> bool {
        self.suppression_until()
            .is_some_and(|deadline| now_hour_bucket < deadline)
    }

    pub(crate) fn suppression_until(&self) -> Option<u64> {
        let mut deadlines = Vec::with_capacity(4);
        if let Some(hour) = self.last_impression_hour_bucket {
            deadlines.push(hour.saturating_add(IMPRESSION_SUPPRESSION_HOURS));
        }
        if let Some(hour) = self.last_submitted_hour_bucket {
            deadlines.push(hour.saturating_add(SUBMITTED_SUPPRESSION_HOURS));
        }
        if self.consecutive_explicit_dismissals >= 2 {
            if let Some(hour) = self.last_explicit_dismissal_hour_bucket {
                deadlines.push(hour.saturating_add(DISMISSED_SUPPRESSION_HOURS));
            }
        }
        if self.consecutive_overrides >= 3 {
            if let Some(hour) = self.last_override_hour_bucket {
                deadlines.push(hour.saturating_add(OVERRIDDEN_SUPPRESSION_HOURS));
            }
        }
        deadlines.into_iter().max()
    }
}

pub(crate) fn classify_edit(original: &str, edited: &str) -> EditBucket {
    let original = original.chars().collect::<Vec<_>>();
    let edited = edited.chars().collect::<Vec<_>>();
    let denominator = original.len().max(edited.len());
    if denominator == 0 {
        return EditBucket::None;
    }
    let distance = levenshtein_distance(&original, &edited);
    if distance == 0 {
        EditBucket::None
    } else if distance.saturating_mul(4) <= denominator {
        EditBucket::Small
    } else {
        EditBucket::Large
    }
}

fn levenshtein_distance(left: &[char], right: &[char]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            current[right_index + 1] = if left_char == right_char {
                previous[right_index]
            } else {
                previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
                    + 1
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommendation::personal_model::CandidateSource;

    fn binding(prompt: &str) -> FrozenPromptBinding {
        FrozenPromptBinding {
            candidate_id: "candidate-1".to_string(),
            task_ref: "task-1".to_string(),
            original_prompt: prompt.to_string(),
            source: CandidateSource::RecentTask,
            suppression_key: "suppress-1".to_string(),
            profile_generation: 7,
            intent_lifecycle_id: "intent-1".to_string(),
        }
    }

    fn edited(count: usize) -> String {
        format!("{}{}", "b".repeat(count), "a".repeat(100 - count))
    }

    #[test]
    fn edit_distance_boundaries_map_to_submitted_or_overridden() {
        assert_eq!(
            classify_edit(&"a".repeat(100), &edited(0)),
            EditBucket::None
        );
        assert_eq!(
            classify_edit(&"a".repeat(100), &edited(25)),
            EditBucket::Small
        );
        assert_eq!(
            classify_edit(&"a".repeat(100), &edited(26)),
            EditBucket::Large
        );

        let mut small = FeedbackLifecycle::new(binding(&"a".repeat(100)));
        small.accept().expect("tab event");
        let submitted = small.submit(&edited(25)).expect("terminal event");
        assert_eq!(submitted.action, FeedbackAction::Submitted);
        assert_eq!(submitted.edit_bucket, Some(EditBucket::Small));

        let mut large = FeedbackLifecycle::new(binding(&"a".repeat(100)));
        large.accept().expect("tab event");
        let overridden = large.submit(&edited(26)).expect("terminal event");
        assert_eq!(overridden.action, FeedbackAction::Overridden);
        assert_eq!(overridden.edit_bucket, Some(EditBucket::Large));
    }

    #[test]
    fn lifecycle_emits_only_one_terminal_action() {
        let mut lifecycle = FeedbackLifecycle::new(binding("continue task"));

        let ignored = lifecycle.ignore().expect("first terminal event");
        assert_eq!(ignored.action, FeedbackAction::Ignored);
        assert!(lifecycle.submit("continue task").is_none());
        assert!(lifecycle.explicit_dismiss().is_none());
        assert!(lifecycle.ignore().is_none());
    }

    #[test]
    fn direct_submit_is_submitted_without_tab_acceptance() {
        let mut lifecycle = FeedbackLifecycle::new(binding("continue task"));

        let submitted = lifecycle.submit("continue task").expect("direct submit");

        assert_eq!(submitted.action, FeedbackAction::Submitted);
        assert_eq!(submitted.edit_bucket, None);
    }

    #[test]
    fn frozen_attribution_survives_lifecycle_events() {
        let mut lifecycle = FeedbackLifecycle::new(binding("continue task"));
        let impression = lifecycle.impression().expect("impression");
        assert_eq!(impression.task_ref, "task-1");
        assert_eq!(impression.profile_generation, 7);
        assert_eq!(impression.candidate_source, CandidateSource::RecentTask);

        lifecycle.accept().expect("tab event");
        let submitted = lifecycle.submit("continue task").expect("submit event");
        assert_eq!(submitted.task_ref, "task-1");
        assert_eq!(submitted.intent_lifecycle_id, "intent-1");
    }

    #[test]
    fn clear_after_accept_is_explicit_dismissal() {
        let mut lifecycle = FeedbackLifecycle::new(binding("continue task"));
        lifecycle.accept().expect("tab event");
        let dismissed = lifecycle.explicit_dismiss().expect("dismiss event");
        assert_eq!(dismissed.action, FeedbackAction::ExplicitDismissed);
        assert_eq!(dismissed.edit_bucket, None);
    }

    #[test]
    fn suppression_uses_8h_2h_7d_and_24h_boundaries() {
        let mut impression = FeedbackSuppressionState::new("task-1");
        impression.record(FeedbackAction::Impression, 100);
        assert!(impression.is_suppressed(107));
        assert!(!impression.is_suppressed(108));

        let mut submitted = FeedbackSuppressionState::new("task-1");
        submitted.record(FeedbackAction::Submitted, 100);
        assert!(submitted.is_suppressed(101));
        assert!(!submitted.is_suppressed(102));

        let mut dismissed = FeedbackSuppressionState::new("task-1");
        dismissed.record(FeedbackAction::ExplicitDismissed, 100);
        dismissed.record(FeedbackAction::ExplicitDismissed, 101);
        assert!(dismissed.is_suppressed(101 + 7 * 24 - 1));
        assert!(!dismissed.is_suppressed(101 + 7 * 24));

        let mut overridden = FeedbackSuppressionState::new("task-1");
        overridden.record(FeedbackAction::Overridden, 100);
        overridden.record(FeedbackAction::Overridden, 101);
        overridden.record(FeedbackAction::Overridden, 102);
        assert!(overridden.is_suppressed(102 + 24 - 1));
        assert!(!overridden.is_suppressed(102 + 24));
    }

    #[test]
    fn ignored_does_not_create_negative_suppression() {
        let mut state = FeedbackSuppressionState::new("task-1");
        state.record(FeedbackAction::Overridden, 99);
        state.record(FeedbackAction::Ignored, 100);
        state.record(FeedbackAction::Overridden, 101);
        state.record(FeedbackAction::Overridden, 102);

        assert!(!state.is_suppressed(102));
        assert_eq!(state.consecutive_overrides, 2);
        assert_eq!(state.consecutive_explicit_dismissals, 0);
    }
}
