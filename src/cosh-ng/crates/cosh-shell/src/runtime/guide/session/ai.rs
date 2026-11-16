//! AI access page: read-only summary state, offered actions, and focus handoff.

use super::status::{self, Reading};
use super::{Flow, Page, Session};

use crate::adapter::{AccessPath, GuideAuthSummary, ProbeState};
use crate::i18n::MessageId;
use crate::ui::guide::WizardRow;

/// Wizard-facing interpretation of the core access summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiStatus {
    Configured { service: Option<String> },
    AutoReady,
    AutoPending,
    AutoProbeFailed,
    AutoDisabled,
    NeedsConfig,
    ExplicitIncomplete,
    Unknown,
}

/// Actions the AI page offers, primary first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiAction {
    Ask,
    Manage,
    Check,
    Retry,
    Enable,
}

/// Maps the read-only summary onto one user-visible state.
///
/// A missing or unconfirmed probe never proves automatic access unavailable,
/// and a saved trial provider is a trusted identity fact even before probing.
pub(crate) fn ai_status(summary: &GuideAuthSummary) -> AiStatus {
    match summary.access {
        AccessPath::EcsTrial => match (summary.configured, summary.probe) {
            (_, ProbeState::ConfirmedSupported) | (true, _) => AiStatus::AutoReady,
            (false, ProbeState::ConfirmedUnsupported) => AiStatus::NeedsConfig,
            (false, ProbeState::Failed) => AiStatus::AutoProbeFailed,
            (false, ProbeState::NotChecked) => AiStatus::AutoPending,
        },
        AccessPath::None => AiStatus::AutoDisabled,
        AccessPath::Explicit if !summary.configured => AiStatus::ExplicitIncomplete,
        AccessPath::Explicit => AiStatus::Configured {
            service: summary.service.clone(),
        },
        // The trial route reads the preference before answering, so an
        // unreadable preference rejects the question even when a trial
        // provider is saved. That is unknown, not ready to ask.
        AccessPath::Unknown if summary.auto_use_unreadable => AiStatus::Unknown,
        // An older core reports no access summary; a configured provider of
        // its own still works, so only that case stays configured.
        AccessPath::Unknown if summary.configured => AiStatus::Configured {
            service: summary.service.clone(),
        },
        AccessPath::Unknown => AiStatus::Unknown,
    }
}

pub(crate) fn ai_actions(status: &AiStatus) -> Vec<AiAction> {
    match status {
        AiStatus::Configured { .. } | AiStatus::AutoReady => {
            vec![AiAction::Ask, AiAction::Manage]
        }
        AiStatus::AutoPending | AiStatus::AutoProbeFailed => {
            vec![AiAction::Ask, AiAction::Check, AiAction::Manage]
        }
        AiStatus::AutoDisabled => vec![AiAction::Enable, AiAction::Manage],
        AiStatus::NeedsConfig | AiStatus::ExplicitIncomplete => vec![AiAction::Manage],
        AiStatus::Unknown => vec![AiAction::Retry, AiAction::Manage],
    }
}

impl Session {
    /// Loads the AI service summary off the UI thread; the registry query
    /// spawns cosh-core and must not block rendering.
    pub(crate) fn start_ai_load(&mut self) {
        self.start_ai_read(status::load_ai);
    }

    /// Runs the read-only identity probe for an explicit user action; it never
    /// activates access, writes preferences, or calls a model.
    pub(crate) fn start_ai_probe(&mut self) {
        self.start_ai_read(status::load_ai_probe);
    }

    fn start_ai_read(&mut self, read: fn() -> Reading<GuideAuthSummary>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.ai_rx = Some(receiver);
        self.ai_loading = true;
        std::thread::spawn(move || {
            let _ = sender.send(read());
        });
    }

    pub(super) fn ai_value(&self) -> String {
        if self.ai_loading {
            return self.i18n.t(MessageId::GuideWizardAiLoading).to_string();
        }
        match &self.ai {
            Reading::Value(summary) => self.ai_short(&ai_status(summary)),
            Reading::Unknown => self
                .i18n
                .t(MessageId::GuideWizardAiShortUnknown)
                .to_string(),
        }
    }

    fn ai_short(&self, status: &AiStatus) -> String {
        match status {
            AiStatus::Configured {
                service: Some(service),
            } => service.clone(),
            AiStatus::Configured { service: None } => self
                .i18n
                .t(MessageId::GuideWizardAiShortConfigured)
                .to_string(),
            AiStatus::AutoReady => self
                .i18n
                .t(MessageId::GuideWizardAiShortAutoReady)
                .to_string(),
            AiStatus::AutoPending | AiStatus::AutoProbeFailed => self
                .i18n
                .t(MessageId::GuideWizardAiShortAutoPending)
                .to_string(),
            AiStatus::AutoDisabled => self
                .i18n
                .t(MessageId::GuideWizardAiShortAutoDisabled)
                .to_string(),
            AiStatus::NeedsConfig | AiStatus::ExplicitIncomplete => self
                .i18n
                .t(MessageId::GuideWizardAiUnconfigured)
                .to_string(),
            AiStatus::Unknown => self
                .i18n
                .t(MessageId::GuideWizardAiShortUnknown)
                .to_string(),
        }
    }

    fn ai_status_text(&self, status: &AiStatus) -> String {
        match status {
            AiStatus::Configured {
                service: Some(service),
            } => self
                .i18n
                .format(MessageId::GuideWizardAiServiceLine, &[("value", service)]),
            AiStatus::Configured { service: None } => self
                .i18n
                .t(MessageId::GuideWizardAiStatusConfiguredGeneric)
                .to_string(),
            AiStatus::AutoReady => self
                .i18n
                .t(MessageId::GuideWizardAiStatusAutoReady)
                .to_string(),
            AiStatus::AutoPending => self
                .i18n
                .t(MessageId::GuideWizardAiStatusAutoPending)
                .to_string(),
            AiStatus::AutoProbeFailed => match self.ai_probe_error_code() {
                Some(code) => self.i18n.format(
                    MessageId::GuideWizardAiStatusAutoProbeFailed,
                    &[("value", &code)],
                ),
                None => self
                    .i18n
                    .t(MessageId::GuideWizardAiStatusAutoPending)
                    .to_string(),
            },
            AiStatus::AutoDisabled => self
                .i18n
                .t(MessageId::GuideWizardAiStatusAutoDisabled)
                .to_string(),
            AiStatus::NeedsConfig => self
                .i18n
                .t(MessageId::GuideWizardAiStatusNeedsConfig)
                .to_string(),
            AiStatus::ExplicitIncomplete => self
                .i18n
                .t(MessageId::GuideWizardAiStatusExplicitIncomplete)
                .to_string(),
            AiStatus::Unknown => self
                .i18n
                .t(MessageId::GuideWizardAiStatusUnknown)
                .to_string(),
        }
    }

    fn ai_probe_error_code(&self) -> Option<String> {
        match &self.ai {
            Reading::Value(summary) => summary.probe_error_code.clone(),
            Reading::Unknown => None,
        }
    }

    fn ai_plan(&self) -> Vec<AiAction> {
        // Nothing is executable while a read is in flight: restarting it would
        // replace the receiver and discard an in-flight probe result.
        if self.ai_loading {
            return Vec::new();
        }
        match &self.ai {
            Reading::Value(summary) => ai_actions(&ai_status(summary)),
            Reading::Unknown => vec![AiAction::Retry, AiAction::Manage],
        }
    }

    fn ai_action_label(&self, action: AiAction) -> &'static str {
        self.i18n.t(match action {
            AiAction::Ask => MessageId::GuideWizardAiActionAsk,
            AiAction::Manage => MessageId::GuideWizardAiManage,
            AiAction::Check => MessageId::GuideWizardAiActionCheck,
            AiAction::Retry => MessageId::GuideWizardAiActionRetry,
            AiAction::Enable => MessageId::GuideWizardAiActionEnable,
        })
    }

    fn ai_source_label(&self, source: &str) -> Option<&'static str> {
        Some(self.i18n.t(match source {
            "user" => MessageId::GuideWizardAiSourceUser,
            "system" => MessageId::GuideWizardAiSourceSystem,
            "runtime" => MessageId::GuideWizardAiSourceRuntime,
            _ => return None,
        }))
    }

    pub(super) fn ai_rows(&self) -> Vec<WizardRow> {
        let mut rows = Vec::new();
        if self.ai_loading {
            rows.push(WizardRow::Text(
                self.i18n.t(MessageId::GuideWizardAiLoading).to_string(),
            ));
        } else {
            match &self.ai {
                Reading::Value(summary) => {
                    let status = ai_status(summary);
                    rows.push(WizardRow::Text(self.ai_status_text(&status)));
                    if matches!(status, AiStatus::Configured { .. }) {
                        rows.push(WizardRow::Text(
                            self.i18n
                                .t(MessageId::GuideWizardAiStatusUnverified)
                                .to_string(),
                        ));
                    }
                    if let Some(source) = summary.source.as_deref() {
                        if let Some(label) = self.ai_source_label(source) {
                            rows.push(WizardRow::Text(
                                self.i18n.format(
                                    MessageId::GuideWizardAiSourceLine,
                                    &[("value", label)],
                                ),
                            ));
                        }
                    }
                    if let Some(auto_use) = summary.auto_use {
                        let label = if auto_use {
                            MessageId::GuideWizardAiAutoUseOn
                        } else {
                            MessageId::GuideWizardAiAutoUseOff
                        };
                        rows.push(WizardRow::Text(self.i18n.format(
                            MessageId::GuideWizardAiAutoUseLine,
                            &[("value", self.i18n.t(label))],
                        )));
                    }
                }
                Reading::Unknown => rows.push(WizardRow::Text(
                    self.i18n
                        .t(MessageId::GuideWizardAiStatusUnknown)
                        .to_string(),
                )),
            }
        }
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::GuideWizardAiReadonly).to_string(),
        ));
        rows.push(WizardRow::Blank);
        rows.extend(self.ai_plan().into_iter().map(|action| WizardRow::Choice {
            label: self.ai_action_label(action).to_string(),
            value: String::new(),
        }));
        rows
    }

    /// Remembers the action under the cursor before the row set is replaced.
    pub(super) fn open_ai_page(&mut self) -> Flow {
        let Some(action) = self.ai_plan().get(self.choice_index()).copied() else {
            // A stale cursor, or a read in flight with no actions, must not
            // turn Enter into a handoff.
            return Flow::Continue;
        };
        self.ai_focus = Some(action);
        self.run_ai_action(action)
    }

    fn run_ai_action(&mut self, action: AiAction) -> Flow {
        match action {
            AiAction::Ask => return Flow::Exit,
            AiAction::Manage | AiAction::Enable => return Flow::Handoff,
            AiAction::Check => self.start_ai_probe(),
            AiAction::Retry => self.start_ai_load(),
        }
        self.message = None;
        Flow::Continue
    }

    /// Keeps the cursor on a real choice after loading or a reading swaps rows.
    ///
    /// The remembered action survives when the new plan still offers it;
    /// otherwise the primary action takes focus so the next key cannot fall
    /// through to a stale row index.
    pub(super) fn reconcile_ai_focus(&mut self) {
        if self.page != Page::Ai {
            return;
        }
        let plan = self.ai_plan();
        if plan.is_empty() {
            return;
        }
        let position = self
            .ai_focus
            .and_then(|focus| plan.iter().position(|action| *action == focus))
            .unwrap_or(0);
        if let Some(index) = self
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, WizardRow::Choice { .. }))
            .nth(position)
            .map(|(index, _)| index)
        {
            self.cursor = index;
        }
    }
}
