//! Guide wizard controller: focus, drafts, and saves through existing owners.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Language, LanguageSetting};
use crate::i18n::{I18n, MessageId};
use crate::ui::guide::{ConfirmationReview, DrawnView, WizardRow, WizardView};

use crate::config::guide::cli::UserPaths;

use super::status::{self, Reading, Snapshot, Source};
use crate::adapter::GuideAuthSummary;

use super::navigation::{Frame, Navigation};

mod presentation;

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
    Health,
    Ai,
    Login,
    LoginConfirm(bool),
    Start,
    Analysis,
    Personal,
    PersonalOffConfirm,
    DataImportConfirm,
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
    /// Guide-lifetime gate: kinds whose earlier result stayed unknown. Reading
    /// current state stays allowed; only re-submitting that kind is held.
    pub(crate) blocked_writes: std::collections::BTreeSet<super::result::ActionKind>,
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
    pub(crate) manual_scroll: bool,
    pub(crate) cleanup_unconfirmed: bool,
    pub(crate) ai_results: Vec<super::result::TaskOutcome>,
    review: Option<ConfirmationReview>,
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
            scroll: 0,
            message: None,
            login_guidance: false,
            blocked_writes: std::collections::BTreeSet::new(),
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
            manual_scroll: false,
            cleanup_unconfirmed: false,
            ai_results: Vec::new(),
            review: None,
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
        self.blocked_writes.extend(outcome.unknown_kinds());
        self.reload();
        self.start_ai_load();
        if outcome.reason == super::result::EndReason::Failed {
            status::warn_failure("auth_task", "task_failed");
        }
        if !outcome.is_quiet() {
            self.ai_results.push(outcome);
            self.message_fresh = true;
        }
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
            personal: (self.page == Page::Personal)
                .then(|| self.personal_actions().get(self.choice_index()).copied())
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
            message: self.message.clone().or_else(|| match self.page {
                Page::Ai => self.ai_receipt(),
                Page::Home => super::result::TaskOutcome::summary(&self.ai_results, self.i18n),
                _ => None,
            }),
            markdown: self.markdown(),
            scroll: self.scroll,
            language: self.i18n.language(),
            message_fresh: self.message_fresh,
            follow_cursor: !self.manual_scroll,
            confirmation: self.is_confirmation(),
            review: self.review,
        }
    }

    pub(crate) fn ai_receipt(&self) -> Option<String> {
        (!self.ai_results.is_empty()).then(|| {
            let saved = self
                .ai_results
                .iter()
                .any(super::result::TaskOutcome::saved_any);
            self.ai_results
                .iter()
                .map(|result| result.message_with_saved_context(self.i18n, saved))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    pub(crate) fn apply_draw(&mut self, drawn: DrawnView) {
        self.scroll = drawn.scroll;
        self.review = drawn.review;
    }

    pub(crate) fn on_resize(&mut self) {
        if self.is_confirmation() {
            self.review = None;
            self.select_choice(1);
        }
    }

    pub(crate) fn handle(&mut self, key: KeyEvent) -> Flow {
        self.manual_scroll = matches!(key.code, KeyCode::PageUp | KeyCode::PageDown);
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            return Flow::Exit;
        }
        match key.code {
            KeyCode::Char('q' | 'Q') => return Flow::Exit,
            KeyCode::F(2) | KeyCode::Char('h') => {
                self.go_home();
                return Flow::Continue;
            }
            _ => {}
        }
        if self.is_confirmation() && !self.review.is_some_and(|review| review.ready) {
            self.manual_scroll = true;
            match key.code {
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll += 1,
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::PageDown => self.scroll += 10,
                KeyCode::Enter | KeyCode::Esc => return self.back(),
                _ => {}
            }
            return Flow::Continue;
        }
        if self.is_markdown_page() {
            match key.code {
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll += 1,
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::PageDown => self.scroll += 10,
                KeyCode::Esc => return self.back(),
                KeyCode::Enter if self.page == Page::Start => return Flow::Ask,
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
            _ => {}
        }
        Flow::Continue
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
                    .format(MessageId::GuideWizardLanguagePrefLine, &[("value", &saved)]),
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
                    self.i18n.t(MessageId::GuideWizardSavedMarker).to_string()
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
        if current.is_none() {
            rows.insert(
                1,
                WizardRow::Text(
                    self.i18n
                        .t(MessageId::GuideWizardHealthReadFailed)
                        .to_string(),
                ),
            );
        }
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
        self.manual_scroll = true;
        self.review = None;
        let ordinal = match (frame.page, frame.login) {
            (Page::Login, Some(choice)) => self
                .login_choices()
                .iter()
                .position(|candidate| *candidate == choice)
                .or_else(|| {
                    self.login_choices().iter().position(|choice| {
                        matches!(
                            choice,
                            super::login::LoginChoice::Reread | super::login::LoginChoice::Other
                        )
                    })
                })
                .unwrap_or(frame.choice),
            _ => frame.choice,
        };
        if let (Page::Personal, Some(action)) = (frame.page, frame.personal) {
            let actions = self.personal_actions();
            let resolved = actions
                .iter()
                .position(|candidate| *candidate == action)
                // A vanished action must not turn the next Enter into a write.
                .or_else(|| {
                    actions
                        .iter()
                        .position(|candidate| *candidate == super::tips::PersonalAction::Privacy)
                });
            if let Some(ordinal) = resolved {
                self.select_choice(ordinal);
                return;
            }
        }
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
                0 => self.open_page(Page::Start),
                1 => self.open_page(Page::Login),
                2 => self.open_page(Page::Ai),
                3 => self.open_page(Page::Health),
                4 => self.open_page(Page::Language),
                _ => self.open_page(Page::Tips),
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
                0 => self.open_page(Page::Analysis),
                _ => self.open_page(Page::Personal),
            },
            Page::Analysis
            | Page::Personal
            | Page::PersonalOffConfirm
            | Page::DataImportConfirm
            | Page::DataResetConfirm
            | Page::DataClearConfirm => {
                return self.tips_open();
            }
            Page::DataPrivacy => {}
            Page::Health => {
                let enabled = self.choice_index() == 0;
                self.save_health(enabled);
            }
            Page::Ai => {
                if self.choice_index() == 0 {
                    if self.ai_can_ask() {
                        return Flow::Ask;
                    }
                    self.open_page(Page::Start);
                } else {
                    return Flow::AiManage;
                }
            }
            Page::Login => {
                return match self.login_choices().get(self.choice_index()).copied() {
                    Some(super::login::LoginChoice::Set) => {
                        self.open_page(Page::LoginConfirm(false));
                        Flow::Continue
                    }
                    Some(super::login::LoginChoice::Restore) => {
                        self.open_page(Page::LoginConfirm(true));
                        Flow::Continue
                    }
                    Some(super::login::LoginChoice::Other) => Flow::LoginOther,
                    Some(super::login::LoginChoice::Reread) => Flow::LoginReread,
                    None => Flow::Continue,
                };
            }
            Page::LoginConfirm(restore) => {
                let confirmed = self.choice_index() == 0;
                self.return_to_source();
                if confirmed && !self.login_write_held() {
                    return if restore {
                        Flow::LoginRestore
                    } else {
                        Flow::LoginSet
                    };
                }
            }
            Page::Start => {}
        }
        Flow::Continue
    }

    pub(crate) fn enter(&mut self, page: Page) {
        self.page = page;
        self.cursor = 0;
        self.scroll = 0;
        self.manual_scroll = false;
        self.review = None;
        self.login_guidance = false;
        self.shift(0);
        self.tips_default_focus();
        if matches!(page, Page::LoginConfirm(_)) {
            self.select_choice(1);
        }
        if page == Page::Ai
            && matches!(
                self.ai_manage_label(),
                MessageId::GuideWizardAiConfigure | MessageId::GuideWizardAiCheck
            )
        {
            self.select_choice(1);
        }
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

    pub(crate) fn go_home(&mut self) {
        if let Some(frame) = self.navigation.take_home() {
            self.message = None;
            self.message_fresh = false;
            self.restore(frame);
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
