//! Tips & recommendation sub-pages: rows and saves through existing owners.

use crate::config::{self, parse_analysis_mode};
use crate::i18n::{I18n, MessageId};
use crate::recommendation::personal_store::PersonalStore;
use crate::ui::guide::WizardRow;

use super::session::{Flow, Page, Session};
use super::status::{now_hour_bucket, warn_failure, Reading};

impl Session {
    pub(crate) fn tips_default_focus(&mut self) {
        if matches!(
            self.page,
            Page::DataClearConfirm | Page::DataResetConfirm | Page::PersonalOffConfirm
        ) {
            self.shift(1);
        } else if self.page == Page::Data && self.bash.unreadable {
            self.shift(-1);
        }
    }

    pub(crate) fn tips_menu_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        vec![
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardGuideTipLabel).to_string(),
                value: bool_value(i18n, self.snapshot.guide_tip),
            },
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardTipsAnalysis).to_string(),
                value: analysis_label(i18n, &self.analysis.effective).to_string(),
            },
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardTipsPersonal).to_string(),
                value: bool_value(i18n, self.rec.effective),
            },
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardTipsData).to_string(),
                value: String::new(),
            },
        ]
    }

    pub(crate) fn tips_rows(&self) -> Vec<WizardRow> {
        match self.page {
            Page::Analysis => self.analysis_rows(),
            Page::Personal => self.personal_rows(),
            Page::PersonalOffConfirm => self.personal_off_confirm_rows(),
            Page::Data => self.data_rows(),
            Page::DataResetConfirm => self.reset_confirm_rows(),
            Page::DataClearConfirm => self.clear_confirm_rows(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn tips_open(&mut self) -> Flow {
        match self.page {
            Page::Analysis => {
                if self.analysis.unreadable {
                    return Flow::Continue;
                }
                let mode = match self.choice_index() {
                    0 => "smart",
                    1 => "auto",
                    _ => "manual",
                };
                self.save_analysis(mode);
            }
            Page::Personal => {
                let enabled = self.choice_index() == 0;
                if enabled {
                    self.save_personal(true);
                } else {
                    // Turning recommendations off clears local data even when
                    // the environment already forces it off, so it always asks.
                    self.open_subpage(Page::PersonalOffConfirm);
                }
            }
            Page::PersonalOffConfirm => {
                if self.choice_index() == 0 {
                    self.save_personal(false);
                    self.return_to_source();
                } else {
                    self.message = None;
                    self.message_fresh = false;
                    self.return_to_source();
                }
            }
            Page::Data => {
                if self.bash.unreadable {
                    match self.choice_index() {
                        0 => self.open_subpage(Page::DataClearConfirm),
                        1 if self.rec.damaged => {
                            self.open_subpage(Page::DataResetConfirm);
                        }
                        _ => self.open_subpage(Page::DataPrivacy),
                    }
                } else {
                    match self.choice_index() {
                        0 => self.save_import(),
                        1 => self.open_subpage(Page::DataClearConfirm),
                        2 if self.rec.damaged => {
                            self.open_subpage(Page::DataResetConfirm);
                        }
                        2 => self.open_subpage(Page::DataPrivacy),
                        _ => self.open_subpage(Page::DataPrivacy),
                    }
                }
            }
            Page::DataResetConfirm => {
                if self.choice_index() == 0 {
                    self.reset_data();
                } else {
                    self.message = None;
                    self.message_fresh = false;
                    self.return_to_source();
                }
            }
            Page::DataClearConfirm => {
                if self.choice_index() == 0 {
                    self.clear_data();
                } else {
                    self.message = None;
                    self.message_fresh = false;
                    self.return_to_source();
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn analysis_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        let saved = if self.analysis.unreadable {
            i18n.t(MessageId::GuideWizardReadFailed).to_string()
        } else {
            self.analysis
                .saved
                .as_deref()
                .and_then(parse_analysis_mode)
                .map(|mode| analysis_label(i18n, mode).to_string())
                .unwrap_or_else(|| i18n.t(MessageId::GuideWizardSourceDefault).to_string())
        };
        let mut rows = vec![
            WizardRow::Text(i18n.format(MessageId::GuideWizardSavedLine, &[("value", &saved)])),
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardEffectiveLine,
                &[("value", analysis_label(i18n, &self.analysis.effective))],
            )),
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardSourceLine,
                &[("value", source_label(i18n, self.analysis.source))],
            )),
            WizardRow::Blank,
        ];
        if self.analysis.unreadable {
            rows.push(WizardRow::Text(
                i18n.t(MessageId::GuideWizardConfigUnreadable).to_string(),
            ));
            return rows;
        }
        let current = self.analysis.saved.as_deref().and_then(parse_analysis_mode);
        for mode in ["smart", "auto", "manual"] {
            rows.push(WizardRow::Choice {
                label: analysis_label(i18n, mode).to_string(),
                value: if current == Some(mode) {
                    i18n.t(MessageId::GuideWizardCurrent).to_string()
                } else {
                    String::new()
                },
            });
        }
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Text(
            i18n.t(MessageId::GuideWizardNewSessionNote).to_string(),
        ));
        rows
    }

    fn personal_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        let mut rows = vec![
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardSavedLine,
                &[("value", &bool_value(i18n, self.rec.user_enabled))],
            )),
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardEffectiveLine,
                &[("value", &bool_value(i18n, self.rec.effective))],
            )),
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardSourceLine,
                &[("value", source_label(i18n, self.rec.source))],
            )),
            WizardRow::Blank,
        ];
        let current = match self.rec.user_enabled {
            Reading::Value(value) => Some(value),
            Reading::Unknown => None,
        };
        for enabled in [true, false] {
            rows.push(WizardRow::Choice {
                label: bool_label(i18n, enabled).to_string(),
                value: if current == Some(enabled) {
                    i18n.t(MessageId::GuideWizardCurrent).to_string()
                } else {
                    String::new()
                },
            });
        }
        rows.push(WizardRow::Blank);
        if self.rec.forced_off {
            rows.push(WizardRow::Text(
                i18n.t(MessageId::GuideWizardForcedOffNote).to_string(),
            ));
        }
        if self.analysis.effective == "manual" {
            rows.push(WizardRow::Text(
                i18n.t(MessageId::GuideWizardManualPausedNote).to_string(),
            ));
        }
        rows
    }

    fn data_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        let overridden = self.bash.source == "env";
        let import_value = if self.bash.unreadable {
            i18n.t(MessageId::GuideWizardReadFailed).to_string()
        } else {
            bool_label(i18n, self.bash.effective).to_string()
        };
        let saved_value = if self.bash.unreadable {
            i18n.t(MessageId::GuideWizardReadFailed).to_string()
        } else {
            bool_value(
                i18n,
                self.bash
                    .saved
                    .map(Reading::Value)
                    .unwrap_or(Reading::Unknown),
            )
        };
        let mut rows = vec![
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardDataImportLine,
                &[("value", &import_value)],
            )),
            WizardRow::Text(
                i18n.format(MessageId::GuideWizardSavedLine, &[("value", &saved_value)]),
            ),
            WizardRow::Text(i18n.format(
                MessageId::GuideWizardSourceLine,
                &[("value", source_label(i18n, self.bash.source))],
            )),
            WizardRow::Blank,
        ];
        if self.bash.unreadable {
            rows.push(WizardRow::Text(
                i18n.t(MessageId::GuideWizardConfigUnreadable).to_string(),
            ));
        } else {
            let primary = if self.bash.effective {
                if overridden {
                    MessageId::GuideWizardDataSaveOffPref
                } else {
                    MessageId::GuideWizardDataStopImport
                }
            } else if overridden {
                MessageId::GuideWizardDataSaveOnPref
            } else {
                MessageId::GuideWizardDataAllowImport
            };
            rows.push(WizardRow::Choice {
                label: i18n.t(primary).to_string(),
                value: String::new(),
            });
            if overridden {
                rows.push(WizardRow::Text(
                    i18n.t(MessageId::GuideWizardDataEnvNote).to_string(),
                ));
            }
            rows.push(WizardRow::Text(
                i18n.t(MessageId::GuideWizardDataStopKeepsNote).to_string(),
            ));
        }
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Choice {
            label: i18n.t(MessageId::GuideWizardDataClear).to_string(),
            value: String::new(),
        });
        if self.rec.damaged {
            rows.push(WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardDataReset).to_string(),
                value: String::new(),
            });
        }
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Choice {
            label: i18n.t(MessageId::GuideWizardTipsData).to_string(),
            value: String::new(),
        });
        rows
    }

    fn reset_confirm_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        vec![
            WizardRow::Text(i18n.t(MessageId::GuideWizardDataDamaged).to_string()),
            WizardRow::Text(i18n.t(MessageId::GuideWizardDataReset).to_string()),
            WizardRow::Text(i18n.t(MessageId::GuideWizardDataResetScope).to_string()),
            WizardRow::Blank,
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardConfirm).to_string(),
                value: String::new(),
            },
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardCancel).to_string(),
                value: String::new(),
            },
        ]
    }

    fn clear_confirm_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        vec![
            WizardRow::Text(i18n.t(MessageId::GuideWizardDataClearConfirm).to_string()),
            WizardRow::Text(i18n.t(MessageId::GuideWizardDataClearScope).to_string()),
            WizardRow::Blank,
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardConfirm).to_string(),
                value: String::new(),
            },
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardCancel).to_string(),
                value: String::new(),
            },
        ]
    }

    fn personal_off_confirm_rows(&self) -> Vec<WizardRow> {
        let i18n = self.i18n;
        vec![
            WizardRow::Text(i18n.t(MessageId::GuideWizardPersonalOffConfirm).to_string()),
            WizardRow::Text(i18n.t(MessageId::GuideWizardPersonalOffScope).to_string()),
            WizardRow::Blank,
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardConfirm).to_string(),
                value: String::new(),
            },
            WizardRow::Choice {
                label: i18n.t(MessageId::GuideWizardCancel).to_string(),
                value: String::new(),
            },
        ]
    }

    fn save_analysis(&mut self, mode: &str) {
        match config::write_analysis_mode_checked(mode) {
            Ok(_) => {
                self.reload();
                let mut message = self.i18n.format(
                    MessageId::GuideWizardSavedLine,
                    &[("value", analysis_label(self.i18n, mode))],
                );
                message.push(' ');
                message.push_str(self.i18n.t(MessageId::GuideWizardNewSessionNote));
                if self.analysis.source == "env" {
                    message.push(' ');
                    message.push_str(self.i18n.t(MessageId::GuideWizardOverriddenNote));
                }
                self.message = Some(message);
            }
            Err(error) => {
                warn_failure("save_analysis", "write_config");
                self.message = Some(
                    self.i18n
                        .format(MessageId::GuideWizardSaveFailed, &[("error", &error)]),
                );
            }
        }
    }

    fn save_personal(&mut self, enabled: bool) {
        if enabled && self.rec.forced_off {
            self.message = Some(self.i18n.t(MessageId::GuideWizardForcedOffNote).to_string());
            return;
        }
        let Some(store) = self.open_rec_store() else {
            return;
        };
        // Turning recommendations off already rotates the epoch and clears the
        // payload inside one owner operation; a second clear here could report
        // failure after the data was in fact removed.
        match store.set_user_enabled(enabled, now_hour_bucket()) {
            Ok(_) => {
                self.reload();
                let mut message = self.i18n.format(
                    MessageId::GuideWizardSavedLine,
                    &[("value", bool_label(self.i18n, enabled))],
                );
                message.push(' ');
                message.push_str(self.i18n.t(MessageId::GuideWizardNewSessionNote));
                self.message = Some(message);
            }
            Err(error) => {
                warn_failure("save_personal", "update_recommendations");
                self.reload();
                let saved_off = !enabled && self.rec.user_enabled == Reading::Value(false);
                self.message = Some(if saved_off {
                    self.i18n
                        .t(MessageId::GuideWizardPersonalOffUnconfirmed)
                        .to_string()
                } else {
                    self.i18n.format(
                        MessageId::GuideWizardSaveFailed,
                        &[("error", &error.to_string())],
                    )
                });
            }
        }
    }

    fn save_import(&mut self) {
        let target = !self.bash.effective;
        match config::write_bash_history_checked(target) {
            Ok(_) => {
                self.reload();
                let mut message = self.i18n.format(
                    MessageId::GuideWizardSavedLine,
                    &[("value", bool_label(self.i18n, target))],
                );
                message.push(' ');
                message.push_str(self.i18n.t(if self.bash.source == "env" {
                    MessageId::GuideWizardDataEnvNote
                } else {
                    MessageId::GuideWizardDataStopKeepsNote
                }));
                self.message = Some(message);
            }
            Err(error) => {
                warn_failure("save_import", "write_config");
                self.message = Some(
                    self.i18n
                        .format(MessageId::GuideWizardSaveFailed, &[("error", &error)]),
                );
            }
        }
    }

    fn clear_data(&mut self) {
        let Some(store) = self.open_rec_store() else {
            return;
        };
        match store.clear(now_hour_bucket()) {
            Ok(_) => {
                self.reload();
                self.message = Some(self.i18n.t(MessageId::GuideWizardDataCleared).to_string());
                self.return_to_source();
            }
            Err(error) => {
                warn_failure("clear_data", "clear_recommendations");
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardSaveFailed,
                    &[("error", &error.to_string())],
                ));
            }
        }
    }

    fn reset_data(&mut self) {
        let Some(store) = self.open_rec_store() else {
            return;
        };
        match store.recover_corrupt_state(false, now_hour_bucket()) {
            Ok(_) => {
                self.reload();
                self.message = Some(self.i18n.t(MessageId::GuideWizardDataResetDone).to_string());
                self.return_to_source();
            }
            Err(error) => {
                warn_failure("reset_data", "recover_recommendations");
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardSaveFailed,
                    &[("error", &error.to_string())],
                ));
            }
        }
    }

    fn open_rec_store(&mut self) -> Option<PersonalStore> {
        let Some(cosh_dir) = self.paths.cosh_dir.clone() else {
            warn_failure("open_recommendation_store", "missing_home");
            self.message = Some(self.i18n.t(MessageId::ConfigHomeMissingBody).to_string());
            return None;
        };
        match PersonalStore::open(cosh_dir.join("recommendations")) {
            Ok(store) => Some(store),
            Err(error) => {
                warn_failure("open_recommendation_store", "open_store");
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardSaveFailed,
                    &[("error", &error.to_string())],
                ));
                None
            }
        }
    }
}

fn bool_label(i18n: I18n, enabled: bool) -> &'static str {
    i18n.t(if enabled {
        MessageId::GuideStateOn
    } else {
        MessageId::GuideStateOff
    })
}

fn bool_value(i18n: I18n, reading: Reading<bool>) -> String {
    match reading {
        Reading::Value(value) => bool_label(i18n, value).to_string(),
        Reading::Unknown => i18n.t(MessageId::GuideWizardReadFailed).to_string(),
    }
}

fn analysis_label(i18n: I18n, mode: &str) -> &'static str {
    match mode {
        "auto" => i18n.t(MessageId::GuideWizardAnalysisAuto),
        "manual" => i18n.t(MessageId::GuideWizardAnalysisManual),
        _ => i18n.t(MessageId::GuideWizardAnalysisSmart),
    }
}

fn source_label(i18n: I18n, source: &str) -> &'static str {
    match source {
        "config" => i18n.t(MessageId::GuideWizardSourceConfig),
        "env" => i18n.t(MessageId::GuideWizardSourceEnv),
        "user" => i18n.t(MessageId::GuideWizardSourceUser),
        _ => i18n.t(MessageId::GuideWizardSourceDefault),
    }
}
