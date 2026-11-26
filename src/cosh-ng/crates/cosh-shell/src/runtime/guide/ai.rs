use crate::i18n::MessageId;
use crate::ui::guide::WizardRow;

use super::session::Session;
use super::status::Reading;
use crate::adapter::GuideAuthSummary;

fn explicit_service(summary: &GuideAuthSummary) -> bool {
    summary.route.as_deref() == Some("existing")
        || (summary.route.is_none() && summary.configured && !summary.legacy_ecs_identity)
}

fn automatic_access(summary: &GuideAuthSummary) -> MessageId {
    if summary.preference_failed {
        MessageId::GuideWizardAiUnknown
    } else if summary.auto_use == Some(false) {
        MessageId::GuideWizardAiDisabled
    } else {
        match summary.identity_status.as_deref() {
            Some("unsupported") => MessageId::GuideWizardAiUnconfigured,
            Some("failed") => MessageId::GuideWizardAiIdentityFailed,
            _ => MessageId::GuideWizardAiPending,
        }
    }
}

impl Session {
    pub(super) fn ai_home_value(&self) -> String {
        match &self.ai {
            Reading::Value(summary)
                if explicit_service(summary) && summary.configured && !self.ai_loading =>
            {
                self.i18n
                    .t(MessageId::GuideWizardAiConfiguredHome)
                    .to_string()
            }
            Reading::Value(summary)
                if !explicit_service(summary)
                    && matches!(automatic_access(summary), MessageId::GuideWizardAiPending)
                    && !self.ai_loading =>
            {
                self.i18n.t(MessageId::GuideWizardAiPendingHome).to_string()
            }
            _ => self.ai_value(),
        }
    }

    pub(super) fn ai_value(&self) -> String {
        if self.ai_loading {
            return self.i18n.t(MessageId::GuideWizardAiLoading).to_string();
        }
        let Reading::Value(summary) = &self.ai else {
            return self.i18n.t(MessageId::GuideWizardAiUnknown).to_string();
        };
        if explicit_service(summary) {
            if !summary.configured && summary.service.is_none() {
                return self.i18n.t(MessageId::GuideWizardAiIncomplete).to_string();
            }
            let service = summary
                .service
                .as_deref()
                .unwrap_or(self.i18n.t(MessageId::GuideWizardAiConfigured));
            let status = if summary.configured {
                MessageId::GuideWizardAiUnverified
            } else {
                MessageId::GuideWizardAiIncomplete
            };
            return format!("{service} · {}", self.i18n.t(status));
        }
        self.i18n.t(automatic_access(summary)).to_string()
    }

    pub(super) fn ai_can_ask(&self) -> bool {
        match &self.ai {
            Reading::Value(summary) if explicit_service(summary) => summary.configured,
            Reading::Value(summary) => matches!(
                automatic_access(summary),
                MessageId::GuideWizardAiPending | MessageId::GuideWizardAiIdentityFailed
            ),
            Reading::Unknown => false,
        }
    }

    pub(super) fn ai_manage_label(&self) -> MessageId {
        match &self.ai {
            Reading::Value(summary) if explicit_service(summary) && !summary.configured => {
                MessageId::GuideWizardAiCheck
            }
            Reading::Value(summary)
                if !explicit_service(summary)
                    && matches!(
                        automatic_access(summary),
                        MessageId::GuideWizardAiUnconfigured
                    ) =>
            {
                MessageId::GuideWizardAiConfigure
            }
            _ => MessageId::GuideWizardAiManage,
        }
    }

    pub(super) fn ai_rows(&self) -> Vec<WizardRow> {
        let mut rows = vec![WizardRow::Text(self.i18n.format(
            MessageId::GuideWizardAiServiceLine,
            &[("value", &self.ai_value())],
        ))];
        if let Reading::Value(summary) = &self.ai {
            if summary.route.as_deref() == Some("existing") && !summary.configured {
                rows.push(WizardRow::Text(
                    self.i18n.t(MessageId::GuideWizardAiNoFallback).to_string(),
                ));
            }
            if let Some(label) = match summary.source.as_deref() {
                Some("system") => Some(MessageId::GuideWizardAiSourceSystem),
                Some("user") => Some(MessageId::GuideWizardAiSourceUser),
                Some("runtime") => Some(MessageId::GuideWizardAiSourceRuntime),
                _ => None,
            } {
                rows.push(WizardRow::Text(self.i18n.format(
                    MessageId::GuideWizardAiSourceLine,
                    &[("value", self.i18n.t(label))],
                )));
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
        if self.ai_can_ask() {
            rows.push(WizardRow::Text(
                self.i18n.t(MessageId::GuideWizardAiReadonly).to_string(),
            ));
        } else if matches!(&self.ai, Reading::Value(summary) if !explicit_service(summary) && matches!(automatic_access(summary), MessageId::GuideWizardAiDisabled))
        {
            rows.push(WizardRow::Text(
                self.i18n
                    .t(MessageId::GuideWizardAiDisabledNext)
                    .to_string(),
            ));
        }
        rows.push(WizardRow::Blank);
        for label in [
            if self.ai_can_ask() {
                MessageId::GuideWizardAiAsk
            } else {
                MessageId::GuideWizardAiHowTo
            },
            self.ai_manage_label(),
        ] {
            rows.push(WizardRow::Choice {
                label: self.i18n.t(label).to_string(),
                value: String::new(),
            });
        }
        rows
    }
}
