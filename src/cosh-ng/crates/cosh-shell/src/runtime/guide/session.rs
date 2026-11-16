//! Guide wizard controller: focus, drafts, and saves through existing owners.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Language, LanguageSetting};
use crate::i18n::{I18n, MessageId};
use crate::ui::guide::{WizardRow, WizardView};

use crate::config::guide::cli::UserPaths;
use crate::config::guide::wizard::content;

use super::status::{self, Reading, Snapshot, Source};
use crate::adapter::GuideAuthSummary;

use super::navigation::{Frame, Navigation};

pub(crate) enum Flow {
    Continue,
    Exit,
    Ask,
    /// Suspend the wizard and run the finite, in-process auth management task.
    AiManage,
    LoginSet,
    LoginRestore,
    LoginOther,
    LoginReread,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    PersonalOffConfirm,
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
    pub(crate) scroll: usize,
    pub(crate) message: Option<String>,
    pub(crate) login_guidance: bool,
    pub(crate) ai: Reading<GuideAuthSummary>,
    pub(crate) ai_loading: bool,
    pub(crate) ai_rx: Option<std::sync::mpsc::Receiver<(u64, Reading<GuideAuthSummary>)>>,
    pub(crate) ai_join: Option<std::thread::JoinHandle<()>>,
    pub(crate) ai_generation: u64,
    pub(crate) analysis: crate::config::AnalysisModeStatus,
    pub(crate) bash: crate::config::BashHistoryStatus,
    pub(crate) rec: status::RecStatus,
    pub(crate) navigation: Navigation,
    pub(crate) message_fresh: bool,
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
            cursor: content::ENTRIES.len() + 1,
            scroll: 0,
            message: None,
            login_guidance: false,
            ai: Reading::Unknown,
            ai_loading: false,
            ai_rx: None,
            ai_join: None,
            ai_generation: 0,
            analysis: crate::config::analysis_mode_status(),
            bash: crate::config::bash_history_status(),
            rec,
            navigation: Navigation::default(),
            message_fresh: false,
        }
    }

    /// Loads the AI service summary off the UI thread; the registry query
    /// spawns cosh-core and must not block rendering.
    pub(crate) fn start_ai_load(&mut self) {
        self.settle_ai_load();
        self.ai_generation = self.ai_generation.wrapping_add(1);
        let generation = self.ai_generation;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.ai_rx = Some(receiver);
        self.ai_loading = true;
        self.ai_join = std::thread::Builder::new()
            .name("cosh-guide-ai-summary".to_string())
            .spawn(move || {
                let _ = sender.send((generation, status::load_ai()));
            })
            .ok();
        if self.ai_join.is_none() {
            self.ai_loading = false;
            self.ai_rx = None;
        }
    }

    /// Retires an in-flight summary read and joins its worker. The wizard owns
    /// the thread until it is reaped, so quitting or opening the auth task
    /// never leaves a detached registry child behind.
    pub(crate) fn settle_ai_load(&mut self) {
        self.ai_generation = self.ai_generation.wrapping_add(1);
        self.ai_rx = None;
        self.ai_loading = false;
        if let Some(handle) = self.ai_join.take() {
            let _ = handle.join();
        }
    }

    pub(crate) fn set_message(&mut self, message: String) {
        self.message = Some(message);
    }

    /// A newly produced result scrolls its full text into view once; later
    /// scrolling stays where the user put it.
    pub(crate) fn refresh_message_focus(&mut self, previous: &Option<String>) {
        if self.message != *previous {
            self.message_fresh = self.message.is_some();
            if self.message_fresh {
                self.scroll = 0;
            }
        }
    }

    /// Collects a finished background read; called on every loop tick.
    /// Returns true when a new reading arrived and the view should redraw.
    pub(crate) fn tick(&mut self) -> bool {
        let received = self
            .ai_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some((generation, reading)) = received {
            if generation == self.ai_generation {
                let ai_choice = (self.page == Page::Ai).then(|| self.choice_index());
                self.ai = reading;
                self.ai_loading = false;
                self.ai_rx = None;
                if let Some(handle) = self.ai_join.take() {
                    let _ = handle.join();
                }
                if let Some(choice) = ai_choice {
                    self.select_choice(choice);
                }
                return true;
            }
        }
        false
    }

    pub(crate) fn language(&self) -> Language {
        self.i18n.language()
    }

    /// Re-read state after the finite auth task returns and report what it
    /// actually changed. Facts come from the task, never from the exit path.
    pub(crate) fn finish_ai_task(&mut self, outcome: super::result::TaskOutcome) {
        self.reload();
        self.start_ai_load();
        if outcome.reason == super::result::EndReason::Failed {
            status::warn_failure("auth_task", "task_failed");
        }
        let message = outcome.message(self.i18n);
        self.set_message(message);
    }

    /// Restores the page that opened the auth task, keeping its choice.
    pub(crate) fn restore_frame(&mut self, frame: Frame) {
        self.restore(frame);
    }

    pub(crate) fn current_frame(&self) -> Frame {
        Frame {
            page: self.page,
            choice: self.choice_index(),
            scroll: self.scroll,
            login: (self.page == Page::Login)
                .then(|| self.login_choices().get(self.choice_index()).copied())
                .flatten(),
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
            message_fresh: self.message_fresh,
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
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                self.message_fresh = false;
            }
            KeyCode::PageDown => {
                self.scroll += 10;
                self.message_fresh = false;
            }
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
            Page::Personal | Page::PersonalOffConfirm => MessageId::GuideWizardTipsPersonal,
            Page::Data | Page::DataPrivacy | Page::DataResetConfirm | Page::DataClearConfirm => {
                MessageId::GuideWizardTipsData
            }
        };
        self.i18n.t(id).to_string()
    }

    fn hint(&self) -> String {
        let id = match self.page {
            Page::Home => MessageId::GuideWizardHomeHint,
            Page::Ai => MessageId::GuideWizardHomeHint,
            Page::Start => MessageId::GuideWizardScrollHint,
            Page::DataPrivacy => MessageId::GuideWizardScrollHint,
            Page::Data
            | Page::Personal
            | Page::Analysis
            | Page::PersonalOffConfirm
            | Page::DataResetConfirm
            | Page::DataClearConfirm => MessageId::GuideWizardDetailHint,
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
            | Page::PersonalOffConfirm
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
            label: self.i18n.t(MessageId::GuideWizardAiAsk).to_string(),
            value: String::new(),
        });
        rows.push(WizardRow::Choice {
            label: self.i18n.t(MessageId::GuideWizardEntryStart).to_string(),
            value: self.i18n.t(MessageId::GuideWizardStartSummary).to_string(),
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

    fn choice_positions(&self) -> Vec<usize> {
        self.rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, WizardRow::Choice { .. }))
            .map(|(index, _)| index)
            .collect()
    }

    pub(super) fn select_choice(&mut self, ordinal: usize) {
        let positions = self.choice_positions();
        self.cursor = positions
            .get(ordinal.min(positions.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0);
    }

    #[cfg(test)]
    pub(crate) fn select_choice_for_test(&mut self, ordinal: usize) {
        self.select_choice(ordinal);
    }

    #[cfg(test)]
    pub(crate) fn select_home_entry(&mut self, ordinal: usize) {
        self.page = Page::Home;
        self.select_choice(ordinal);
    }

    /// Opens a sub-page, remembering the page and choice that opened it.
    fn open_page(&mut self, page: Page) {
        let frame = self.current_frame();
        self.navigation.push(frame);
        self.message = None;
        self.message_fresh = false;
        self.enter(page);
    }

    fn restore(&mut self, frame: Frame) {
        self.page = frame.page;
        self.login_guidance = false;
        self.scroll = frame.scroll;
        let ordinal = match (frame.page, frame.login) {
            (Page::Login, Some(choice)) => self
                .login_choices()
                .iter()
                .position(|candidate| *candidate == choice)
                .unwrap_or(frame.choice),
            _ => frame.choice,
        };
        self.select_choice(ordinal);
    }

    /// Returns to the page that opened the current one, keeping any result
    /// message the caller just produced.
    pub(crate) fn return_to_source(&mut self) {
        if let Some(frame) = self.navigation.pop() {
            self.restore(frame);
        }
    }

    pub(crate) fn open_subpage(&mut self, page: Page) {
        self.open_page(page);
    }

    fn open(&mut self) -> Flow {
        match self.page {
            Page::Home => match self.choice_index() {
                0 => self.open_page(Page::Language),
                1 => self.open_page(Page::Ai),
                2 => self.open_page(Page::Tips),
                3 => self.open_page(Page::Health),
                4 => self.open_page(Page::Login),
                5 => return Flow::Ask,
                _ => self.open_page(Page::Start),
            },
            Page::Language => {
                let setting = match self.choice_index() {
                    0 => LanguageSetting::Auto,
                    1 => LanguageSetting::Language(Language::EnUs),
                    _ => LanguageSetting::Language(Language::ZhCn),
                };
                self.save_language(setting);
            }
            Page::Tips => match self.choice_index() {
                0 => self.open_page(Page::GuideTip),
                1 => self.open_page(Page::Analysis),
                2 => self.open_page(Page::Personal),
                _ => self.open_page(Page::Data),
            },
            Page::Analysis
            | Page::Personal
            | Page::PersonalOffConfirm
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
            Page::Ai => {
                return if self.choice_index() == 0 {
                    Flow::Ask
                } else {
                    Flow::AiManage
                };
            }
            Page::Login => {
                return match self.login_choices().get(self.choice_index()).copied() {
                    Some(super::login::LoginChoice::Set) => Flow::LoginSet,
                    Some(super::login::LoginChoice::Restore) => Flow::LoginRestore,
                    Some(super::login::LoginChoice::Other) => Flow::LoginOther,
                    Some(super::login::LoginChoice::Reread) => Flow::LoginReread,
                    None => Flow::Continue,
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
        self.login_guidance = false;
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
        match self.navigation.pop() {
            Some(frame) => {
                self.message = None;
                self.message_fresh = false;
                self.restore(frame);
                Flow::Continue
            }
            None => Flow::Exit,
        }
    }

    pub(crate) fn reload(&mut self) {
        self.snapshot = status::load(&self.paths);
        self.i18n = I18n::new(self.snapshot.language.effective);
        self.analysis = crate::config::analysis_mode_status();
        self.bash = crate::config::bash_history_status();
        self.rec = status::load_recommendations(&self.paths);
    }
}
