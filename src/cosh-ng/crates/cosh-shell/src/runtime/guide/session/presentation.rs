//! Page-level hint and body presentation for the wizard session.

use crate::config::guide::wizard::content;
use crate::i18n::MessageId;

use super::{Page, Session};

impl Session {
    pub(super) fn hint(&self) -> String {
        if matches!(self.page, Page::LoginConfirm(_)) {
            let action = self.i18n.t(if self.choice_index() == 0 {
                MessageId::GuideWizardConfirm
            } else {
                MessageId::GuideWizardCancel
            });
            return self
                .i18n
                .format(MessageId::GuideWizardConfirmHint, &[("action", action)]);
        }
        let id = match self.page {
            Page::Home => MessageId::GuideWizardHomeHint,
            Page::Ai => MessageId::GuideWizardHomeHint,
            Page::Login | Page::Tips => MessageId::GuideWizardOpenHint,
            Page::Start => MessageId::GuideWizardScrollHint,
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
}
