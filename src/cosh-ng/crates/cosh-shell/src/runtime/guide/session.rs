//! Guide wizard controller: focus, drafts, and saves through existing owners.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::guide::GuideStore;
use crate::config::{write_user_language_config, Language, LanguageSetting};
use crate::i18n::{I18n, MessageId};
use crate::ui::guide::{WizardRow, WizardView};

use crate::config::guide::cli::{self, UserPaths};
use crate::config::guide::wizard::content;

use super::status::{self, Reading, Snapshot, Source};
use crate::adapter::GuideAuthSummary;

pub(crate) enum Flow {
    Continue,
    Exit,
    /// Suspend the wizard and hand the terminal to the existing auth flow.
    Handoff,
    LoginSet,
    LoginRestore,
    LoginOther,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Home,
    Language,
    Tips,
    GuideTip,
    Health,
    Ai,
    Login,
    Start,
    Analysis,
    Personal,
    Data,
    DataPrivacy,
    DataResetConfirm,
    DataClearConfirm,
}

pub(crate) struct Session {
    pub(crate) paths: UserPaths,
    pub(crate) i18n: I18n,
    pub(crate) snapshot: Snapshot,
    pub(crate) page: Page,
    pub(crate) cursor: usize,
    pub(crate) home_cursor: usize,
    pub(crate) tips_cursor: usize,
    pub(crate) scroll: usize,
    pub(crate) message: Option<String>,
    pub(crate) ai: Reading<GuideAuthSummary>,
    pub(crate) ai_loading: bool,
    pub(crate) ai_rx: Option<std::sync::mpsc::Receiver<Reading<GuideAuthSummary>>>,
    pub(crate) analysis: crate::config::AnalysisModeStatus,
    pub(crate) bash: crate::config::BashHistoryStatus,
    pub(crate) rec: status::RecStatus,
}

impl Session {
    pub(crate) fn new(paths: UserPaths, i18n: I18n) -> Self {
        let snapshot = status::load(&paths);
        let rec = status::load_recommendations(&paths);
        Self {
            paths,
            i18n,
            snapshot,
            page: Page::Home,
            cursor: 0,
            home_cursor: 0,
            tips_cursor: 0,
            scroll: 0,
            message: None,
            ai: Reading::Unknown,
            ai_loading: false,
            ai_rx: None,
            analysis: crate::config::analysis_mode_status(),
            bash: crate::config::bash_history_status(),
            rec,
        }
    }

    /// Loads the AI service summary off the UI thread; the registry query
    /// spawns cosh-core and must not block rendering.
    pub(crate) fn start_ai_load(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.ai_rx = Some(receiver);
        self.ai_loading = true;
        std::thread::spawn(move || {
            let _ = sender.send(status::load_ai());
        });
    }

    /// Collects a finished background read; called on every loop tick.
    /// Returns true when a new reading arrived and the view should redraw.
    pub(crate) fn tick(&mut self) -> bool {
        if let Some(receiver) = &self.ai_rx {
            if let Ok(reading) = receiver.try_recv() {
                self.ai = reading;
                self.ai_loading = false;
                self.ai_rx = None;
                return true;
            }
        }
        false
    }

    pub(crate) fn language(&self) -> Language {
        self.i18n.language()
    }

    /// Re-read state after the auth handoff child exits; a spawn failure is
    /// surfaced but never blocks the wizard from resuming.
    pub(crate) fn finish_handoff(&mut self, result: Result<(), String>) {
        self.reload();
        self.start_ai_load();
        if let Err(error) = result {
            status::warn_failure("auth_handoff", "child_process");
            self.message = Some(
                self.i18n
                    .format(MessageId::GuideWizardAiHandoffFailed, &[("error", &error)]),
            );
        }
    }

    pub(crate) fn view(&self) -> WizardView {
        let rows = self.rows();
        WizardView {
            title: self.title(),
            cursor: self.cursor,
            rows,
            hint: self.hint(),
            message: self.message.clone(),
            markdown: self.markdown(),
            scroll: self.scroll,
            language: self.i18n.language(),
        }
    }

    pub(crate) fn handle(&mut self, key: KeyEvent) -> Flow {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            return Flow::Exit;
        }
        if self.is_markdown_page() {
            match key.code {
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll += 1,
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::PageDown => self.scroll += 10,
                KeyCode::Esc => return self.back(),
                _ => {}
            }
            return Flow::Continue;
        }
        match key.code {
            KeyCode::Up => self.shift(-1),
            KeyCode::Down => self.shift(1),
            KeyCode::Enter => return self.open(),
            KeyCode::Esc => return self.back(),
            KeyCode::Char('q') | KeyCode::Char('Q') if self.page == Page::Home => {
                return Flow::Exit;
            }
            _ => {}
        }
        Flow::Continue
    }

    fn title(&self) -> String {
        let id = match self.page {
            Page::Home => MessageId::GuideWizardTitle,
            Page::Language => MessageId::GuideWizardLanguageTitle,
            Page::Tips | Page::GuideTip => MessageId::GuideWizardEntryTips,
            Page::Health => MessageId::GuideWizardEntryHealth,
            Page::Ai => MessageId::GuideWizardEntryAi,
            Page::Login => MessageId::GuideWizardEntryLogin,
            Page::Start => MessageId::GuideWizardEntryStart,
            Page::Analysis => MessageId::GuideWizardTipsAnalysis,
            Page::Personal => MessageId::GuideWizardTipsPersonal,
            Page::Data | Page::DataPrivacy | Page::DataResetConfirm | Page::DataClearConfirm => {
                MessageId::GuideWizardTipsData
            }
        };
        self.i18n.t(id).to_string()
    }

    fn hint(&self) -> String {
        let id = match self.page {
            Page::Home => MessageId::GuideWizardHomeHint,
            Page::Start => MessageId::GuideWizardScrollHint,
            Page::DataPrivacy => MessageId::GuideWizardScrollHint,
            _ => MessageId::GuideWizardChoiceHint,
        };
        self.i18n.t(id).to_string()
    }

    fn markdown(&self) -> Option<String> {
        match self.page {
            Page::Start => Some(content::start_markdown(self.i18n).to_string()),
            Page::DataPrivacy => Some(self.i18n.t(MessageId::GuideWizardDataPrivacy).to_string()),
            _ => None,
        }
    }

    fn is_markdown_page(&self) -> bool {
        matches!(self.page, Page::Start | Page::DataPrivacy)
    }

    fn rows(&self) -> Vec<WizardRow> {
        match self.page {
            Page::Home => self.home_rows(),
            Page::Language => self.language_rows(),
            Page::Tips => self.tips_menu_rows(),
            Page::Analysis
            | Page::Personal
            | Page::Data
            | Page::DataResetConfirm
            | Page::DataClearConfirm => self.tips_rows(),
            Page::DataPrivacy => Vec::new(),
            Page::GuideTip => {
                let mut rows =
                    self.toggle_rows(MessageId::GuideWizardGuideTipBody, self.snapshot.guide_tip);
                let runtime = match self.snapshot.guide_runtime {
                    Reading::Value(true) => MessageId::GuideAvailableConditional,
                    Reading::Value(false) => MessageId::GuideClosed,
                    Reading::Unknown => MessageId::GuideUnconfirmed,
                };
                rows.push(WizardRow::Blank);
                rows.push(WizardRow::Text(self.i18n.t(runtime).to_string()));
                rows
            }
            Page::Health => {
                self.toggle_rows(MessageId::GuideWizardHealthDetailBody, self.snapshot.health)
            }
            Page::Login => self.login_rows(),
            Page::Ai => self.ai_rows(),
            Page::Start => Vec::new(),
        }
    }

    fn home_rows(&self) -> Vec<WizardRow> {
        let login = match &self.snapshot.login {
            Ok(status) => format!(
                "{} · {}",
                status.username,
                std::path::Path::new(&status.current_shell)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| status.current_shell.clone()),
            ),
            Err(_) => self.i18n.t(MessageId::GuideWizardReadFailed).to_string(),
        };
        let values = [
            self.language_label(self.snapshot.language.effective)
                .to_string(),
            self.ai_value(),
            String::new(),
            self.toggle_value(self.snapshot.health),
            login,
        ];
        let mut rows = content::ENTRIES
            .iter()
            .enumerate()
            .map(|(index, _)| WizardRow::Choice {
                label: content::entry_label(self.i18n, index).to_string(),
                value: values[index].clone(),
            })
            .collect::<Vec<_>>();
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Choice {
            label: self.i18n.t(MessageId::GuideWizardEntryStart).to_string(),
            value: self.i18n.t(MessageId::GuideWizardStartSummary).to_string(),
        });
        rows
    }

    fn ai_value(&self) -> String {
        if self.ai_loading {
            return self.i18n.t(MessageId::GuideWizardAiLoading).to_string();
        }
        match &self.ai {
            Reading::Value(summary) if summary.configured => {
                summary.service.clone().unwrap_or_else(|| {
                    self.i18n
                        .t(MessageId::GuideWizardAiUnconfigured)
                        .to_string()
                })
            }
            Reading::Value(_) => self
                .i18n
                .t(MessageId::GuideWizardAiUnconfigured)
                .to_string(),
            Reading::Unknown => self.i18n.t(MessageId::GuideWizardReadFailed).to_string(),
        }
    }

    fn ai_rows(&self) -> Vec<WizardRow> {
        let mut rows = vec![WizardRow::Text(self.i18n.format(
            MessageId::GuideWizardAiServiceLine,
            &[("value", &self.ai_value())],
        ))];
        if let Reading::Value(summary) = &self.ai {
            if let Some(source) = &summary.source {
                let label = match source.as_str() {
                    "system" => MessageId::GuideWizardAiSourceSystem,
                    _ => MessageId::GuideWizardAiSourceUser,
                };
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
        rows.push(WizardRow::Choice {
            label: self.i18n.t(MessageId::GuideWizardAiManage).to_string(),
            value: String::new(),
        });
        rows
    }

    fn language_rows(&self) -> Vec<WizardRow> {
        let language = &self.snapshot.language;
        let saved = match language.saved {
            Reading::Value(setting) => self.language_label_setting(setting).to_string(),
            Reading::Unknown => self.i18n.t(MessageId::GuideWizardReadFailed).to_string(),
        };
        let source = match language.source {
            Source::Config => MessageId::GuideWizardSourceConfig,
            Source::Env => MessageId::GuideWizardSourceEnv,
            Source::Default => MessageId::GuideWizardSourceDefault,
        };
        let mut rows = vec![
            WizardRow::Text(
                self.i18n
                    .format(MessageId::GuideWizardSavedLine, &[("value", &saved)]),
            ),
            WizardRow::Text(self.i18n.format(
                MessageId::GuideWizardLanguageEffectiveLine,
                &[("value", self.language_label(language.effective))],
            )),
            WizardRow::Text(self.i18n.format(
                MessageId::GuideWizardLanguageSourceLine,
                &[("value", self.i18n.t(source))],
            )),
        ];
        if language.source == Source::Env {
            rows.push(WizardRow::Text(
                self.i18n
                    .t(MessageId::GuideWizardLanguageEnvNote)
                    .to_string(),
            ));
        }
        rows.push(WizardRow::Blank);
        let current = match language.saved {
            Reading::Value(setting) => Some(setting),
            Reading::Unknown => None,
        };
        for setting in [
            LanguageSetting::Auto,
            LanguageSetting::Language(Language::EnUs),
            LanguageSetting::Language(Language::ZhCn),
        ] {
            rows.push(WizardRow::Choice {
                label: self.language_label_setting(setting).to_string(),
                value: if current == Some(setting) {
                    self.i18n.t(MessageId::GuideWizardCurrent).to_string()
                } else {
                    String::new()
                },
            });
        }
        rows.push(WizardRow::Blank);
        rows.push(WizardRow::Text(
            self.i18n
                .t(MessageId::GuideWizardLanguageScopeNote)
                .to_string(),
        ));
        rows
    }

    fn toggle_rows(&self, body: MessageId, reading: Reading<bool>) -> Vec<WizardRow> {
        let current = match reading {
            Reading::Value(enabled) => Some(enabled),
            Reading::Unknown => None,
        };
        let mut rows = vec![
            WizardRow::Text(self.i18n.t(body).to_string()),
            WizardRow::Blank,
        ];
        for enabled in [true, false] {
            rows.push(WizardRow::Choice {
                label: self.toggle_label(enabled).to_string(),
                value: if current == Some(enabled) {
                    self.i18n.t(MessageId::GuideWizardCurrent).to_string()
                } else {
                    String::new()
                },
            });
        }
        rows
    }

    pub(crate) fn shift(&mut self, delta: i32) {
        let rows = self.rows();
        let choices = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, WizardRow::Choice { .. }))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if choices.is_empty() {
            return;
        }
        let position = choices
            .iter()
            .position(|index| *index == self.cursor)
            .unwrap_or(0);
        let next = (position as i32 + delta).rem_euclid(choices.len() as i32) as usize;
        self.cursor = choices[next];
    }

    pub(crate) fn choice_index(&self) -> usize {
        self.rows()
            .iter()
            .take(self.cursor)
            .filter(|row| matches!(row, WizardRow::Choice { .. }))
            .count()
    }

    fn open(&mut self) -> Flow {
        match self.page {
            Page::Home => {
                self.home_cursor = self.cursor;
                self.message = None;
                self.scroll = 0;
                match self.cursor {
                    0 => self.enter(Page::Language),
                    1 => self.enter(Page::Ai),
                    2 => {
                        self.tips_cursor = 0;
                        self.enter(Page::Tips);
                    }
                    3 => self.enter(Page::Health),
                    4 => self.enter(Page::Login),
                    _ => self.enter(Page::Start),
                }
            }
            Page::Language => {
                let setting = match self.choice_index() {
                    0 => LanguageSetting::Auto,
                    1 => LanguageSetting::Language(Language::EnUs),
                    _ => LanguageSetting::Language(Language::ZhCn),
                };
                self.save_language(setting);
            }
            Page::Tips => {
                self.tips_cursor = self.cursor;
                self.message = None;
                match self.choice_index() {
                    0 => self.enter(Page::GuideTip),
                    1 => self.enter(Page::Analysis),
                    2 => self.enter(Page::Personal),
                    _ => self.enter(Page::Data),
                }
            }
            Page::Analysis
            | Page::Personal
            | Page::Data
            | Page::DataResetConfirm
            | Page::DataClearConfirm => {
                return self.tips_open();
            }
            Page::DataPrivacy => {}
            Page::GuideTip => {
                let enabled = self.choice_index() == 0;
                self.save_guide_tip(enabled);
            }
            Page::Health => {
                let enabled = self.choice_index() == 0;
                self.save_health(enabled);
            }
            Page::Ai => return Flow::Handoff,
            Page::Login => {
                let has_restore =
                    matches!(&self.snapshot.login, Ok(status) if status.original_bash.is_some());
                return match self.choice_index() {
                    0 => Flow::LoginSet,
                    1 if has_restore => Flow::LoginRestore,
                    _ => Flow::LoginOther,
                };
            }
            Page::Start => {}
        }
        Flow::Continue
    }

    pub(crate) fn enter(&mut self, page: Page) {
        self.page = page;
        self.cursor = 0;
        self.scroll = 0;
        self.shift(0);
        self.tips_default_focus();
        if page == Page::Language {
            if let Reading::Value(setting) = self.snapshot.language.saved {
                self.cursor = self
                    .rows()
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| matches!(row, WizardRow::Choice { .. }))
                    .nth(match setting {
                        LanguageSetting::Auto => 0,
                        LanguageSetting::Language(Language::EnUs) => 1,
                        LanguageSetting::Language(Language::ZhCn) => 2,
                    })
                    .map(|(index, _)| index)
                    .unwrap_or(0);
            }
        }
    }

    fn back(&mut self) -> Flow {
        match self.page {
            Page::Home => return Flow::Exit,
            Page::DataResetConfirm | Page::DataClearConfirm => self.enter(Page::Data),
            Page::GuideTip => {
                self.page = Page::Tips;
                self.cursor = self.tips_cursor;
            }
            Page::Analysis | Page::Personal | Page::Data | Page::DataPrivacy => {
                self.page = Page::Tips;
                self.cursor = self.tips_cursor;
            }
            _ => {
                self.page = Page::Home;
                self.cursor = self.home_cursor;
            }
        }
        self.scroll = 0;
        self.message = None;
        Flow::Continue
    }

    fn save_language(&mut self, setting: LanguageSetting) {
        if matches!(self.snapshot.language.saved, Reading::Unknown) {
            status::warn_failure("save_language", "unreadable_config");
            self.message = Some(self.i18n.t(MessageId::GuideWizardReadFailed).to_string());
            return;
        }
        match write_user_language_config(setting.as_config_value()) {
            Ok(_) => {
                self.reload();
                self.i18n = I18n::new(self.snapshot.language.effective);
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardSavedLine,
                    &[("value", self.language_label_setting(setting))],
                ));
                self.page = Page::Home;
                self.cursor = self.home_cursor;
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

    fn save_guide_tip(&mut self, enabled: bool) {
        let Some(store) = self.open_store() else {
            return;
        };
        let output = cli::set(&store, &self.paths, enabled, self.i18n);
        if output.exit_code != 0 {
            status::warn_failure("save_guide_tip", "set_guide");
        }
        self.message = Some(output.summary());
        self.reload();
    }

    fn save_health(&mut self, enabled: bool) {
        let Some(store) = self.open_store() else {
            return;
        };
        match store.set_banner(enabled) {
            Ok(_) => {
                self.message = Some(format!(
                    "{} {}",
                    self.i18n.format(
                        MessageId::GuideWizardSavedLine,
                        &[("value", self.toggle_label(enabled))],
                    ),
                    self.i18n.t(MessageId::GuideWizardSavedNextLogin),
                ));
                self.reload();
            }
            Err(error) => {
                status::warn_failure("save_health", "set_banner");
                self.message = Some(self.i18n.format(
                    MessageId::GuideWizardSaveFailed,
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

    pub(crate) fn reload(&mut self) {
        self.snapshot = status::load(&self.paths);
        self.i18n = I18n::new(self.snapshot.language.effective);
        self.analysis = crate::config::analysis_mode_status();
        self.bash = crate::config::bash_history_status();
        self.rec = status::load_recommendations(&self.paths);
    }

    fn toggle_label(&self, enabled: bool) -> &'static str {
        self.i18n.t(if enabled {
            MessageId::GuideStateOn
        } else {
            MessageId::GuideStateOff
        })
    }

    fn toggle_value(&self, reading: Reading<bool>) -> String {
        match reading {
            Reading::Value(enabled) => self.toggle_label(enabled).to_string(),
            Reading::Unknown => self.i18n.t(MessageId::GuideWizardReadFailed).to_string(),
        }
    }

    fn language_label(&self, language: Language) -> &'static str {
        self.language_label_setting(LanguageSetting::Language(language))
    }

    fn language_label_setting(&self, setting: LanguageSetting) -> &'static str {
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
