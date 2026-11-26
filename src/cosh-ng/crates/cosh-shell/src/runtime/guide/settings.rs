//! Wizard persistence for the simple settings pages, through the existing
//! owners and the ownership-checked configuration writers.

use crate::config::guide::cli;
use crate::config::guide::GuideStore;
use crate::config::{write_user_language_config_checked, Language, LanguageSetting};
use crate::i18n::{I18n, MessageId};

use super::session::Session;
use super::status::{self, Reading, Source};

impl Session {
    pub(super) fn save_language(&mut self, setting: LanguageSetting) {
        if matches!(self.snapshot.language.saved, Reading::Unknown) {
            status::warn_failure("save_language", "unreadable_config");
            self.set_message(self.i18n.t(MessageId::GuideWizardReadFailed).to_string());
            return;
        }
        match write_user_language_config_checked(setting.as_config_value()) {
            Ok(_) => {
                let choice = self.choice_index();
                self.reload();
                self.i18n = I18n::new(self.snapshot.language.effective);
                let message = self.i18n.format(
                    if self.snapshot.language.source == Source::Env {
                        MessageId::GuideWizardLanguageOverride
                    } else if setting == LanguageSetting::Auto {
                        MessageId::GuideWizardLanguageFollowed
                    } else {
                        MessageId::GuideWizardLanguageSaved
                    },
                    &[
                        ("saved", self.language_label_setting(setting)),
                        ("effective", self.language_label(self.language())),
                    ],
                );
                self.select_choice(choice);
                self.set_message(message);
            }
            Err(error) => {
                status::warn_failure("save_language", "write_config");
                self.message = Some(
                    self.i18n
                        .format(MessageId::GuideWizardSaveFailed, &[("error", &error)]),
                );
            }
        }
    }

    pub(super) fn save_health(&mut self, enabled: bool) {
        let Some(store) = self.open_store() else {
            return;
        };
        match store.set_banner(enabled) {
            Ok(_) => {
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardHealthSaved,
                    &[("value", self.toggle_label(enabled))],
                ));
                self.reload();
            }
            Err(error) => {
                status::warn_failure("save_health", "set_banner");
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardHealthSaveFailed,
                    &[("error", &cli::error_reason(self.i18n, &error))],
                ));
            }
        }
    }

    fn open_store(&mut self) -> Option<GuideStore> {
        let Some(cosh_dir) = self.paths.cosh_dir.clone() else {
            status::warn_failure("open_guide_store", "missing_home");
            self.message = Some(self.i18n.t(MessageId::ConfigHomeMissingBody).to_string());
            return None;
        };
        match GuideStore::open(cosh_dir.join("guide")) {
            Ok(store) => Some(store),
            Err(error) => {
                status::warn_failure("open_guide_store", "open_store");
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardSaveFailed,
                    &[("error", &cli::error_reason(self.i18n, &error))],
                ));
                None
            }
        }
    }

    pub(super) fn toggle_label(&self, enabled: bool) -> &'static str {
        self.i18n.t(if enabled {
            MessageId::GuideStateOn
        } else {
            MessageId::GuideStateOff
        })
    }

    pub(super) fn toggle_value(&self, reading: Reading<bool>) -> String {
        match reading {
            Reading::Value(enabled) => self.toggle_label(enabled).to_string(),
            Reading::Unknown => self.i18n.t(MessageId::GuideWizardReadFailed).to_string(),
        }
    }

    pub(super) fn language_label(&self, language: Language) -> &'static str {
        self.language_label_setting(LanguageSetting::Language(language))
    }

    pub(super) fn language_label_setting(&self, setting: LanguageSetting) -> &'static str {
        match setting {
            LanguageSetting::Auto => self.i18n.t(MessageId::GuideWizardLanguageAuto),
            LanguageSetting::Language(Language::EnUs) => {
                self.i18n.t(MessageId::GuideWizardLanguageEn)
            }
            LanguageSetting::Language(Language::ZhCn) => {
                self.i18n.t(MessageId::GuideWizardLanguageZh)
            }
        }
    }
}
