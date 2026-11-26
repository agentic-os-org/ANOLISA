//! Page-level hint and body presentation for the wizard session.

use crate::config::guide::wizard::content;
use crate::i18n::MessageId;
use crate::runtime::guide::result::ActionKind;
use crate::ui::guide::WizardRow;

use super::{Page, Session};

impl Session {
    pub(super) fn title(&self) -> String {
        let id = match self.page {
            Page::Home => MessageId::GuideWizardTitle,
            Page::Language => MessageId::GuideWizardLanguageTitle,
            Page::Tips => MessageId::GuideWizardEntryTips,
            Page::Health => MessageId::GuideWizardEntryHealth,
            Page::Ai => MessageId::GuideWizardEntryAi,
            Page::Login => MessageId::GuideWizardEntryLogin,
            Page::LoginConfirm(false) => MessageId::GuideWizardLoginSetTitle,
            Page::LoginConfirm(true) => MessageId::GuideWizardLoginRestore,
            Page::Start => MessageId::GuideWizardEntryStart,
            Page::Analysis => MessageId::GuideWizardTipsAnalysis,
            Page::Personal => MessageId::GuideWizardTipsPersonal,
            Page::PersonalOffConfirm => MessageId::GuideWizardPersonalDisable,
            Page::DataImportConfirm => MessageId::GuideWizardDataAllowImport,
            Page::DataPrivacy => MessageId::GuideWizardTipsData,
            Page::DataResetConfirm => MessageId::GuideWizardDataReset,
            Page::DataClearConfirm => MessageId::GuideWizardDataClear,
        };
        self.i18n.t(id).to_string()
    }

    pub(super) fn rows(&self) -> Vec<WizardRow> {
        match self.page {
            Page::Home => self.home_rows(),
            Page::Language => self.language_rows(),
            Page::Tips => self.tips_menu_rows(),
            Page::Analysis
            | Page::Personal
            | Page::PersonalOffConfirm
            | Page::DataImportConfirm
            | Page::DataResetConfirm
            | Page::DataClearConfirm => self.tips_rows(),
            Page::DataPrivacy | Page::Start => Vec::new(),
            Page::Health => {
                self.toggle_rows(MessageId::GuideWizardHealthDetailBody, self.snapshot.health)
            }
            Page::Login => self.login_rows(),
            Page::LoginConfirm(restore) => self.login_confirmation_rows(restore),
            Page::Ai => self.ai_rows(),
        }
    }

    fn home_rows(&self) -> Vec<WizardRow> {
        let login = match &self.snapshot.login {
            _ if self.blocked_writes.contains(&ActionKind::LoginShell) => self
                .i18n
                .t(MessageId::GuideWizardLoginSummaryPending)
                .to_string(),
            Ok(status) if status.conflict => self
                .i18n
                .t(MessageId::GuideWizardLoginSummaryConflict)
                .to_string(),
            Ok(status)
                if status.current_shell == crate::runtime::login::COSH_PATH && !status.managed =>
            {
                self.i18n
                    .t(MessageId::GuideWizardLoginSummaryRecoveryMissing)
                    .to_string()
            }
            Ok(status) if status.current_shell == crate::runtime::login::COSH_PATH => self
                .i18n
                .t(MessageId::GuideWizardLoginSummarySet)
                .to_string(),
            Ok(status) => {
                let path = &status.current_shell;
                let shell = if crate::ui::display_width(path) > 14 {
                    path.rsplit('/').next().unwrap_or(path)
                } else {
                    path
                };
                self.i18n.format(
                    MessageId::GuideWizardLoginSummaryBenefit,
                    &[("shell", shell)],
                )
            }
            Err(_) => self.i18n.t(MessageId::GuideWizardReadFailed).to_string(),
        };
        let values = [
            self.i18n.t(MessageId::GuideWizardStartSummary).to_string(),
            login,
            self.ai_home_value(),
            self.toggle_value(self.snapshot.health),
            self.language_label(self.snapshot.language.effective)
                .to_string(),
            self.i18n.t(MessageId::GuideWizardTipsSummary).to_string(),
        ];
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| WizardRow::Choice {
                label: content::entry_label(self.i18n, index).to_string(),
                value,
            })
            .collect()
    }

    pub(super) fn hint(&self) -> String {
        if self.is_confirmation() {
            let action = self.i18n.t(
                if self.choice_index() == 0 && self.review.is_some_and(|review| review.ready) {
                    match self.page {
                        Page::LoginConfirm(false) => MessageId::GuideWizardConfirmSet,
                        Page::LoginConfirm(true) => MessageId::GuideWizardConfirmRestore,
                        Page::DataResetConfirm => MessageId::GuideWizardConfirmReset,
                        Page::PersonalOffConfirm => MessageId::GuideWizardConfirmDisable,
                        Page::DataClearConfirm => MessageId::GuideWizardConfirmClear,
                        Page::DataImportConfirm => MessageId::GuideWizardConfirmImport,
                        _ => MessageId::GuideWizardConfirm,
                    }
                } else {
                    MessageId::GuideWizardCancelKey
                },
            );
            return self
                .i18n
                .format(MessageId::GuideWizardConfirmHint, &[("action", action)]);
        }
        let id = match self.page {
            Page::Home => MessageId::GuideWizardHomeHint,
            Page::Ai if self.choice_index() == 0 && self.ai_can_ask() => {
                MessageId::GuideWizardAskHint
            }
            Page::Ai => MessageId::GuideWizardOpenHint,
            Page::Login | Page::Tips => MessageId::GuideWizardOpenHint,
            Page::Start => MessageId::GuideWizardAskHint,
            Page::DataPrivacy => MessageId::GuideWizardScrollHint,
            Page::Personal
            | Page::Analysis
            | Page::PersonalOffConfirm
            | Page::DataImportConfirm
            | Page::DataResetConfirm
            | Page::DataClearConfirm => MessageId::GuideWizardDetailHint,
            _ => MessageId::GuideWizardChoiceHint,
        };
        self.i18n.t(id).to_string()
    }

    pub(super) fn markdown(&self) -> Option<String> {
        match self.page {
            Page::Start => Some(content::start_markdown(self.i18n).to_string()),
            Page::DataPrivacy => Some(self.i18n.t(MessageId::GuideWizardDataPrivacy).to_string()),
            _ => None,
        }
    }

    pub(super) fn is_markdown_page(&self) -> bool {
        matches!(self.page, Page::Start | Page::DataPrivacy)
    }

    pub(super) fn is_confirmation(&self) -> bool {
        matches!(
            self.page,
            Page::LoginConfirm(_)
                | Page::PersonalOffConfirm
                | Page::DataImportConfirm
                | Page::DataResetConfirm
                | Page::DataClearConfirm
        )
    }
}
