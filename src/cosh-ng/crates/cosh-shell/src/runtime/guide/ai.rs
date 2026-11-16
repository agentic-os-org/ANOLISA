use crate::i18n::MessageId;
use crate::ui::guide::WizardRow;

use super::session::Session;
use super::status::Reading;

impl Session {
    pub(super) fn ai_value(&self) -> String {
        if self.ai_loading {
            return self.i18n.t(MessageId::GuideWizardAiLoading).to_string();
        }
        let Reading::Value(summary) = &self.ai else {
            return self.i18n.t(MessageId::GuideWizardAiUnknown).to_string();
        };
        if summary.route.as_deref() == Some("existing")
            || (summary.route.is_none() && summary.configured && !summary.legacy_ecs_identity)
        {
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
        let label = if summary.preference_failed {
            MessageId::GuideWizardAiUnknown
        } else if summary.auto_use == Some(false) {
            MessageId::GuideWizardAiDisabled
        } else {
            match summary.identity_status.as_deref() {
                Some("supported") => MessageId::GuideWizardAiEcs,
                Some("unsupported") => MessageId::GuideWizardAiUnconfigured,
                Some("failed") => MessageId::GuideWizardAiIdentityFailed,
                _ => MessageId::GuideWizardAiPending,
            }
        };
        self.i18n.t(label).to_string()
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
        rows.push(WizardRow::Text(
            self.i18n.t(MessageId::GuideWizardAiReadonly).to_string(),
        ));
        rows.push(WizardRow::Blank);
        for label in [MessageId::GuideWizardAiAsk, MessageId::GuideWizardAiManage] {
            rows.push(WizardRow::Choice {
                label: self.i18n.t(label).to_string(),
                value: String::new(),
            });
        }
        rows
    }
}
