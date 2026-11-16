//! Bounded task results: why a task stopped, what it changed, and what was
//! verified. Facts are aggregated per action kind so a later cancel or failure
//! never erases an earlier committed change.

use std::collections::BTreeMap;

use crate::i18n::{I18n, MessageId};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EndReason {
    Done,
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChangeFact {
    None,
    Committed,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Verification {
    Verified,
    Unconfirmed,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum ActionKind {
    Configure,
    Activate,
    Delete,
    TrialPreference,
    LoginShell,
}

impl ActionKind {
    fn label(self) -> MessageId {
        match self {
            Self::Configure => MessageId::GuideWizardActionConfigure,
            Self::Activate => MessageId::GuideWizardActionActivate,
            Self::Delete => MessageId::GuideWizardActionDelete,
            Self::TrialPreference => MessageId::GuideWizardActionTrial,
            Self::LoginShell => MessageId::GuideWizardActionLogin,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct ActionFacts {
    pub(crate) committed: bool,
    pub(crate) partial: bool,
    pub(crate) unknown: bool,
}

impl ActionFacts {
    fn record(&mut self, change: ChangeFact) {
        match change {
            ChangeFact::None => {}
            ChangeFact::Committed => self.committed = true,
            ChangeFact::Partial => self.partial = true,
            ChangeFact::Unknown => self.unknown = true,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TaskOutcome {
    pub(crate) reason: EndReason,
    pub(crate) error: Option<String>,
    actions: BTreeMap<ActionKind, ActionFacts>,
    unconfirmed: Vec<ActionKind>,
}

impl Default for TaskOutcome {
    fn default() -> Self {
        Self {
            reason: EndReason::Done,
            error: None,
            actions: BTreeMap::new(),
            unconfirmed: Vec::new(),
        }
    }
}

impl TaskOutcome {
    pub(crate) fn record(
        &mut self,
        kind: ActionKind,
        change: ChangeFact,
        verification: Verification,
    ) {
        self.actions.entry(kind).or_default().record(change);
        if verification == Verification::Unconfirmed && !self.unconfirmed.contains(&kind) {
            self.unconfirmed.push(kind);
        }
    }

    #[cfg(test)]
    pub(crate) fn facts(&self, kind: ActionKind) -> ActionFacts {
        self.actions.get(&kind).copied().unwrap_or_default()
    }

    pub(crate) fn committed_any(&self) -> bool {
        self.actions.values().any(|facts| facts.committed)
    }

    pub(crate) fn unconfirmed_any(&self) -> bool {
        !self.unconfirmed.is_empty() || self.actions.values().any(|facts| facts.unknown)
    }

    fn kinds(&self, select: impl Fn(&ActionFacts) -> bool) -> Vec<ActionKind> {
        self.actions
            .iter()
            .filter(|(_, facts)| select(facts))
            .map(|(kind, _)| *kind)
            .collect()
    }

    fn joined(i18n: I18n, kinds: &[ActionKind]) -> String {
        let separator = match i18n.language() {
            crate::config::Language::ZhCn => "、",
            _ => ", ",
        };
        kinds
            .iter()
            .map(|kind| i18n.t(kind.label()))
            .collect::<Vec<_>>()
            .join(separator)
    }

    /// Full result text: saved facts, unconfirmed facts, and the scope note.
    pub(crate) fn message(&self, i18n: I18n) -> String {
        let mut parts = Vec::new();
        let committed = self.kinds(|facts| facts.committed);
        if !committed.is_empty() {
            parts.push(i18n.format(
                MessageId::GuideWizardResultSaved,
                &[("actions", &Self::joined(i18n, &committed))],
            ));
        }
        let partial = self.kinds(|facts| facts.partial);
        if !partial.is_empty() {
            parts.push(i18n.format(
                MessageId::GuideWizardResultPartial,
                &[("actions", &Self::joined(i18n, &partial))],
            ));
        }
        let mut unknown = self.kinds(|facts| facts.unknown);
        for kind in &self.unconfirmed {
            if !unknown.contains(kind) {
                unknown.push(*kind);
            }
        }
        if !unknown.is_empty() {
            parts.push(i18n.format(
                MessageId::GuideWizardResultUnconfirmed,
                &[("actions", &Self::joined(i18n, &unknown))],
            ));
        }
        if parts.is_empty() {
            parts.push(i18n.t(MessageId::GuideWizardResultNone).to_string());
        }
        match self.reason {
            EndReason::Cancelled => {
                parts.push(i18n.t(MessageId::GuideWizardResultCancelled).to_string())
            }
            EndReason::Failed => parts.push(i18n.format(
                MessageId::GuideWizardResultFailed,
                &[("error", self.error.as_deref().unwrap_or_default())],
            )),
            EndReason::Interrupted => {
                parts.push(i18n.t(MessageId::GuideWizardResultInterrupted).to_string())
            }
            EndReason::Done => {}
        }
        if self.committed_any() || self.unconfirmed_any() {
            parts.push(i18n.t(MessageId::GuideWizardResultScope).to_string());
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Language;

    fn i18n() -> I18n {
        I18n::new(Language::EnUs)
    }

    #[test]
    fn cancel_after_a_committed_switch_keeps_the_saved_fact() {
        let mut outcome = TaskOutcome {
            reason: EndReason::Cancelled,
            ..Default::default()
        };
        outcome.record(
            ActionKind::Activate,
            ChangeFact::Committed,
            Verification::Verified,
        );
        assert!(outcome.committed_any());
        let message = outcome.message(i18n());
        assert!(message.contains("Saved"), "{message}");
        assert!(message.contains("switch"), "{message}");
        assert!(!message.contains("No configuration changed"), "{message}");
    }

    #[test]
    fn switch_saved_then_delete_unknown_reports_both_accurately() {
        let mut outcome = TaskOutcome::default();
        outcome.record(
            ActionKind::Activate,
            ChangeFact::Committed,
            Verification::Verified,
        );
        outcome.record(
            ActionKind::Delete,
            ChangeFact::Unknown,
            Verification::Unconfirmed,
        );
        let message = outcome.message(i18n());
        assert!(message.contains("Saved"), "{message}");
        assert!(message.contains("Unconfirmed"), "{message}");
        assert!(message.contains("delete"), "{message}");
        assert!(outcome.unconfirmed_any());
    }

    #[test]
    fn same_kind_success_then_failure_keeps_both_facts() {
        let mut outcome = TaskOutcome::default();
        outcome.record(
            ActionKind::Configure,
            ChangeFact::Committed,
            Verification::Verified,
        );
        outcome.record(
            ActionKind::Configure,
            ChangeFact::Unknown,
            Verification::Unconfirmed,
        );
        let facts = outcome.facts(ActionKind::Configure);
        assert!(facts.committed);
        assert!(facts.unknown);
        let message = outcome.message(i18n());
        assert!(message.contains("Saved"), "{message}");
        assert!(message.contains("Unconfirmed"), "{message}");
    }

    #[test]
    fn untouched_cancel_reports_no_change_without_a_scope_claim() {
        let outcome = TaskOutcome {
            reason: EndReason::Cancelled,
            ..Default::default()
        };
        let message = outcome.message(i18n());
        assert!(message.contains("No configuration changed"), "{message}");
        assert!(!message.contains("New sessions"), "{message}");
    }

    #[test]
    fn partial_configuration_is_not_reported_as_a_plain_failure() {
        let mut outcome = TaskOutcome::default();
        outcome.record(
            ActionKind::Configure,
            ChangeFact::Partial,
            Verification::Unconfirmed,
        );
        let message = outcome.message(i18n());
        assert!(message.contains("Partly completed"), "{message}");
    }
}
