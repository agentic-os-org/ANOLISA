use super::login::{log_login_outcome, login_message, settle_outcome};
use super::session::{Flow, Page, Session};
use super::status::{self, Reading};
use crate::config::guide::cli::UserPaths;
use crate::config::guide::GuideStore;
use crate::config::{private_state, Language};
use crate::i18n::I18n;
use crate::runtime::login::COSH_PATH;
use crate::ui::guide::WizardRow;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

fn fixture() -> (tempfile::TempDir, UserPaths) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = UserPaths {
        cosh_dir: Some(root.join(".copilot-shell/cosh")),
        runtime_base: None,
    };
    (temp, paths)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn env_guard() -> EnvGuard {
    let lock = crate::diagnostics::test_env::env_guard();
    let saved = [
        "HOME",
        "COSH_SHELL_LANG",
        "COSH_SHELL_ANALYSIS_MODE",
        "COSH_RECOMMENDATIONS_ENABLED",
        "COSH_RECOMMENDATIONS_BASH_HISTORY",
    ]
    .iter()
    .map(|name| (*name, std::env::var_os(name)))
    .collect();
    EnvGuard { _lock: lock, saved }
}

fn isolated_fixture() -> (EnvGuard, tempfile::TempDir, UserPaths) {
    let guard = env_guard();
    let (temp, paths) = fixture();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_LANG");
    std::env::remove_var("COSH_SHELL_ANALYSIS_MODE");
    std::env::remove_var("COSH_RECOMMENDATIONS_ENABLED");
    std::env::remove_var("COSH_RECOMMENDATIONS_BASH_HISTORY");
    (guard, temp, paths)
}

#[test]
fn session_startup_preserves_unknown_language_for_oversized_config() {
    let (_env, temp, paths) = isolated_fixture();
    std::env::remove_var("COSH_SHELL_LANG");
    let expected = crate::config::detect_language_from_env();
    let saved = if expected == Language::EnUs {
        "zh-CN"
    } else {
        "en-US"
    };
    let dir = temp.path().join(".copilot-shell");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(
        &path,
        format!("[ui]\nlanguage='{saved}'\n#{}", "x".repeat(256 * 1024)),
    )
    .unwrap();
    let session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    assert!(matches!(session.snapshot.language.saved, Reading::Unknown));
    assert_eq!(session.snapshot.language.effective, expected);
    assert!(session.snapshot.language.source == status::Source::Default);
    fs::remove_file(&path).unwrap();
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    assert!(matches!(session.snapshot.language.saved, Reading::Unknown));
    session.enter(super::session::Page::Language);
    session.handle(key(KeyCode::Enter));
    assert_eq!(
        session.message.as_deref(),
        Some(I18n::new(Language::EnUs).t(crate::i18n::MessageId::GuideWizardReadFailed))
    );
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_fifo());
}

fn snapshot_dir(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut entries = std::collections::BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        entries.insert(
            entry.file_name().to_string_lossy().into_owned(),
            fs::read(entry.path()).unwrap(),
        );
    }
    entries
}

fn seeded_record(index: usize) -> crate::recommendation::personal_model::ActivityRecord {
    use crate::recommendation::personal_model::{
        ActivityContext, ActivityPayload, ActivityRecord, ActivitySource, RedactionReport,
    };
    ActivityRecord {
        activity_id: format!("activity-{index}"),
        session_scope_id: Some("session".to_string()),
        source_fingerprint: format!("fingerprint-{index}"),
        observed_hour_bucket: 1_000,
        source: ActivitySource::BashHistory,
        context: ActivityContext::default(),
        payload: ActivityPayload::BashHistoryCommand {
            command: format!("history {index}"),
            origin_unverified: true,
            execution_hour_bucket: None,
            time_unverified: true,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

#[test]
fn missing_store_reads_defaults_but_corrupt_store_is_unknown() {
    let (_guard, _temp, paths) = isolated_fixture();
    let snapshot = status::load(&paths);
    assert_eq!(snapshot.guide_tip, Reading::Value(true));
    assert_eq!(snapshot.health, Reading::Value(true));

    let store = GuideStore::open(paths.cosh_dir.as_ref().unwrap().join("guide")).unwrap();
    private_state::atomic_write(&store.root().join("preferences.json"), b"corrupt", 4096).unwrap();
    let snapshot = status::load(&paths);
    assert_eq!(snapshot.guide_tip, Reading::Unknown);
    assert_eq!(snapshot.health, Reading::Unknown);
}

#[test]
fn saved_toggles_are_read_without_the_write_lock() {
    let (_guard, _temp, paths) = isolated_fixture();
    let store = GuideStore::open(paths.cosh_dir.as_ref().unwrap().join("guide")).unwrap();
    store.set_banner(false).unwrap();
    store.set_guide(None, false).unwrap();
    fs::remove_file(store.root().join("state.lock")).unwrap();
    let snapshot = status::load(&paths);
    assert_eq!(snapshot.guide_tip, Reading::Value(false));
    assert_eq!(snapshot.health, Reading::Value(false));
    assert!(!store.root().join("state.lock").exists());
}

#[test]
fn home_navigation_opens_a_page_and_returns_to_the_same_row() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    assert_eq!(session.view().title, "Cosh setup and getting started");
    for _ in 0..3 {
        session.handle(key(KeyCode::Down));
    }
    assert_eq!(session.view().cursor, 3);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.view().title, "Login health summary");
    session.handle(key(KeyCode::Esc));
    assert_eq!(session.view().cursor, 3);
    assert_eq!(session.view().title, "Cosh setup and getting started");
}

#[test]
fn start_page_scrolls_and_returns_without_saving() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    for _ in 0..5 {
        session.handle(key(KeyCode::Down));
    }
    session.handle(key(KeyCode::Enter));
    assert!(session.view().markdown.is_some());
    session.handle(key(KeyCode::Down));
    assert_eq!(session.view().scroll, 1);
    session.handle(key(KeyCode::PageUp));
    assert_eq!(session.view().scroll, 0);
    session.handle(key(KeyCode::Esc));
    assert!(session.view().markdown.is_none());
    assert!(!paths.cosh_dir.as_ref().unwrap().exists());
}

#[test]
fn data_privacy_page_scrolls_like_other_markdown_pages() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::DataPrivacy);
    assert!(session.view().markdown.is_some());
    session.handle(key(KeyCode::Down));
    assert_eq!(session.view().scroll, 1);
    session.handle(key(KeyCode::PageDown));
    assert_eq!(session.view().scroll, 11);
    session.handle(key(KeyCode::Up));
    assert_eq!(session.view().scroll, 10);
    session.handle(key(KeyCode::Esc));
    assert!(matches!(session.page, super::session::Page::Tips));
}

#[test]
fn language_save_persists_returns_home_and_refreshes_the_ui_language() {
    let _guard = env_guard();
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_LANG");
    let (_temp, paths) = fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.view().title, "Language / 语言");
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    let config = fs::read_to_string(temp.path().join(".copilot-shell/config.toml")).unwrap();
    assert!(config.contains("zh-CN"), "{config}");
    assert_eq!(session.view().title, "Cosh 配置与入门");
    assert!(session.view().message.unwrap().contains("已保存"));
}

#[test]
fn language_escape_keeps_the_draft_unsaved() {
    let _guard = env_guard();
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_LANG");
    let (_temp, paths) = fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.handle(key(KeyCode::Enter));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Esc));
    assert!(!temp.path().join(".copilot-shell/config.toml").exists());
}

#[test]
fn health_save_reports_the_next_login_scope() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    for _ in 0..3 {
        session.handle(key(KeyCode::Down));
    }
    session.handle(key(KeyCode::Enter));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    let message = session.view().message.unwrap();
    assert!(message.contains("Saved: off"), "{message}");
    assert!(message.contains("next login"), "{message}");
    let store = GuideStore::open(paths.cosh_dir.as_ref().unwrap().join("guide")).unwrap();
    assert!(!store.peek_preferences().unwrap().unwrap().banner_enabled);
}

#[test]
fn guide_tip_save_reuses_the_cli_result_and_keeps_banner_independent() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.view().title, "Tips and recommendations");
    session.handle(key(KeyCode::Enter));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    let message = session.view().message.unwrap();
    assert!(message.contains("AI tips"), "{message}");
    let store = GuideStore::open(paths.cosh_dir.as_ref().unwrap().join("guide")).unwrap();
    let preferences = store.peek_preferences().unwrap().unwrap();
    assert!(!preferences.guide_enabled);
    assert!(preferences.banner_enabled);
}

#[test]
fn ctrl_c_exits_from_any_page() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.handle(key(KeyCode::Enter));
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(matches!(session.handle(ctrl_c), Flow::Exit));
}

#[test]
fn analysis_save_persists_default_and_reports_new_session_scope() {
    let (_guard, _temp, paths) = isolated_fixture();
    std::env::set_var("COSH_SHELL_LANG", "en-US");
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.page = super::session::Page::Analysis;
    session.cursor = 0;
    session.shift(0);
    session.shift(1); // auto
    session.tips_open();
    assert_eq!(session.analysis.saved.as_deref(), Some("auto"));
    assert_eq!(session.analysis.source, "config");
    let message = session.view().message.unwrap();
    assert!(message.contains("new sessions"), "{message}");
}

#[test]
fn forced_off_rejects_enable_without_persisting() {
    let _guard = env_guard();
    let (temp, paths) = fixture();
    std::env::set_var("HOME", temp.path());
    std::env::set_var("COSH_RECOMMENDATIONS_ENABLED", "0");
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    assert!(session.rec.forced_off);
    session.page = super::session::Page::Personal;
    session.cursor = 0;
    session.shift(0); // on
    session.tips_open();
    let message = session.view().message.unwrap();
    assert!(message.contains("forces recommendations off"), "{message}");
    assert_eq!(session.rec.user_enabled, Reading::Unknown);
}

#[test]
fn import_toggle_saves_preference_and_keeps_existing_data() {
    let _guard = env_guard();
    let (temp, paths) = fixture();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_RECOMMENDATIONS_BASH_HISTORY");
    let root = paths.cosh_dir.as_ref().unwrap().join("recommendations");
    let store = crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
    let initial = store.set_user_enabled(true, 1_000).unwrap();
    store
        .merge(
            &crate::recommendation::personal_store::StateVersion::of(&initial),
            1_000,
            |state| {
                state.journal.records.push(seeded_record(0));
                state.journal.records.push(seeded_record(1));
            },
        )
        .unwrap();
    let before = snapshot_dir(&root);
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    assert!(!session.bash.effective);
    session.page = super::session::Page::Data;
    session.cursor = 0;
    session.shift(0); // primary import action
    session.tips_open();
    assert_eq!(session.bash.saved, Some(true));
    assert!(session.bash.effective);
    assert_eq!(
        before,
        snapshot_dir(&root),
        "import toggle must not touch recommendation data"
    );
    assert_eq!(
        store.peek().unwrap().unwrap().journal.records.len(),
        2,
        "import toggle must preserve the journal payload"
    );
}

#[test]
fn clear_data_requires_confirmation_and_keeps_the_on_off_preference() {
    let (_guard, _temp, paths) = isolated_fixture();
    std::env::set_var("COSH_SHELL_LANG", "en-US");
    let store = crate::recommendation::personal_store::PersonalStore::open(
        paths.cosh_dir.as_ref().unwrap().join("recommendations"),
    )
    .unwrap();
    let initial = store.set_user_enabled(true, 1_000).unwrap();
    store
        .merge(
            &crate::recommendation::personal_store::StateVersion::of(&initial),
            1_000,
            |state| {
                state.journal.records.push(seeded_record(0));
                state.journal.records.push(seeded_record(1));
            },
        )
        .unwrap();
    let epoch_before = store.peek().unwrap().unwrap().store_epoch;
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    assert_eq!(session.rec.user_enabled, Reading::Value(true));
    session.enter(super::session::Page::Data);
    session.shift(1); // clear data only
    session.tips_open();
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    assert_eq!(store.peek().unwrap().unwrap().store_epoch, epoch_before);
    session.handle(key(KeyCode::Enter)); // default Cancel
    assert!(matches!(session.page, super::session::Page::Data));
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    session.enter(super::session::Page::Data);
    session.shift(1);
    session.tips_open();
    session.handle(key(KeyCode::Esc));
    assert!(matches!(session.page, super::session::Page::Data));
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    session.enter(super::session::Page::Data);
    session.shift(1);
    session.tips_open();
    session.shift(-1); // Confirm
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.rec.user_enabled, Reading::Value(true));
    let message = session.view().message.unwrap();
    assert!(message.contains("kept"), "{message}");
    let epoch_after = store.peek().unwrap().unwrap().store_epoch;
    assert_ne!(
        epoch_before, epoch_after,
        "clear must reset the stored data"
    );
    assert!(
        store.peek().unwrap().unwrap().journal.records.is_empty(),
        "clear must empty the journal payload"
    );
}

#[test]
fn unreadable_data_page_defaults_to_privacy_with_or_without_reset() {
    for damaged in [false, true] {
        let (_guard, temp, paths) = isolated_fixture();
        let config_dir = temp.path().join(".copilot-shell");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("config.toml"), "broken = = config").unwrap();
        let root = paths.cosh_dir.as_ref().unwrap().join("recommendations");
        let store = crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
        let initial = store.set_user_enabled(true, 1_000).unwrap();
        store
            .merge(
                &crate::recommendation::personal_store::StateVersion::of(&initial),
                1_000,
                |state| state.journal.records.push(seeded_record(0)),
            )
            .unwrap();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.rec.damaged = damaged;
        let before = snapshot_dir(&root);
        session.enter(super::session::Page::Data);
        session.handle(key(KeyCode::Enter));
        assert!(matches!(session.page, super::session::Page::DataPrivacy));
        assert_eq!(before, snapshot_dir(&root));
    }
}

#[test]
fn data_clear_reset_and_privacy_routes_follow_visible_choices() {
    use super::session::Page;
    use crate::i18n::MessageId;
    use crate::ui::guide::WizardRow;
    for unreadable in [false, true] {
        let (_guard, _temp, paths) = isolated_fixture();
        let root = paths.cosh_dir.as_ref().unwrap().join("recommendations");
        let store = crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
        store.set_user_enabled(true, 1_000).unwrap();
        private_state::atomic_write(&root.join("state.json"), b"corrupt", 4096).unwrap();
        let before = snapshot_dir(&root);
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.bash.unreadable = unreadable;
        assert!(session.rec.damaged);
        for (label, page) in [
            (MessageId::GuideWizardDataClear, Page::DataClearConfirm),
            (MessageId::GuideWizardDataReset, Page::DataResetConfirm),
            (MessageId::GuideWizardTipsData, Page::DataPrivacy),
        ] {
            session.enter(Page::Data);
            session.cursor = session.view().rows.iter().position(|row| {
                matches!(row, WizardRow::Choice { label: text, .. } if text == session.i18n.t(label))
            }).unwrap();
            session.handle(key(KeyCode::Enter));
            assert!(session.page == page);
            assert_eq!(before, snapshot_dir(&root));
            if page != Page::DataPrivacy {
                session.handle(key(KeyCode::Enter));
                assert!(session.page == Page::Data);
                assert_eq!(before, snapshot_dir(&root));
            }
        }
        session.enter(Page::DataResetConfirm);
        session.shift(-1);
        session.handle(key(KeyCode::Enter));
        assert!(session.page == Page::Data);
        assert_eq!(session.rec.user_enabled, Reading::Value(false));
        assert!(store.peek().unwrap().unwrap().journal.records.is_empty());
    }
}

#[derive(Clone, Default)]
struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn failed_config_saves_are_logged_without_sensitive_toml_lines() {
    let (_guard, temp, paths) = isolated_fixture();
    let config_dir = temp.path().join(".copilot-shell");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "api_key = TOP_SECRET_PROMPT = =",
    )
    .unwrap();
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.enter(super::session::Page::Language);
        session.handle(key(KeyCode::Enter));
        assert!(session.view().message.unwrap().contains("failed"));
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("operation=\"save_language\""), "{logs}");
    assert!(logs.contains("result=\"failed\""), "{logs}");
    assert!(!logs.contains("TOP_SECRET"), "{logs}");
    assert!(!logs.contains("api_key"), "{logs}");
}

#[test]
fn login_page_offers_only_the_actions_the_account_state_supports() {
    let (_guard, _temp, paths) = isolated_fixture();
    let cases = vec![
        (
            Ok(login_status(
                "/usr/bin/cosh",
                Some("/bin/bash"),
                true,
                false,
            )),
            vec!["Restore original login shell"],
            None,
        ),
        (
            Ok(login_status("/bin/bash", Some("/bin/bash"), false, false)),
            vec!["Set Cosh as login shell"],
            None,
        ),
        (
            Ok(login_status("/bin/bash", None, false, false)),
            vec!["Set Cosh as login shell"],
            None,
        ),
        (
            Ok(login_status(COSH_PATH, None, false, false)),
            Vec::new(),
            Some("no recovery record"),
        ),
        (
            Ok(login_status("/bin/zsh", None, false, true)),
            Vec::new(),
            Some("conflicts"),
        ),
        (
            Err("permission denied".to_string()),
            Vec::new(),
            Some("Unable to read the login shell: permission denied"),
        ),
    ];
    for (status, actions, note) in cases {
        let readable = status.is_ok();
        let mut session = login_session_with(paths.clone(), status);
        session.enter(Page::Login);
        let rows = session.view().rows;
        assert_eq!(choice_labels(&session), actions);
        if readable {
            assert!(
                rows.iter().any(
                    |row| matches!(row, WizardRow::Text(text) if text.starts_with("Other users"))
                ),
                "the other-user guidance must stay a text row"
            );
        }
        if let Some(note) = note {
            assert!(
                rows.iter()
                    .any(|row| matches!(row, WizardRow::Text(text) if text.contains(note))),
                "missing {note}"
            );
        }
        if !readable {
            assert!(
                !rows
                    .iter()
                    .any(|row| matches!(row, WizardRow::Choice { .. })),
                "an unreadable state must not offer an action"
            );
        }
        if actions.is_empty() {
            assert!(matches!(
                session.handle(key(KeyCode::Enter)),
                Flow::Continue
            ));
        } else {
            assert!(matches!(rows[session.cursor], WizardRow::Choice { .. }));
        }
    }
}

#[test]
fn login_page_routes_each_visible_choice_to_its_own_action() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = login_session(
        paths,
        login_status("/usr/bin/cosh", Some("/bin/bash"), true, false),
    );
    session.enter(Page::Login);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginRestore
    ));

    session.snapshot.login = Ok(login_status("/bin/bash", Some("/bin/bash"), false, false));
    session.enter(Page::Login);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginSet
    ));

    session.snapshot.login = Ok(login_status("/bin/zsh", None, false, true));
    session.enter(Page::Login);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::Continue
    ));
}

#[test]
fn login_page_cursor_wraps_only_between_visible_actions() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = login_session(
        paths,
        login_status("/usr/bin/cosh", Some("/bin/bash"), true, false),
    );
    session.enter(Page::Login);
    let choices = session
        .view()
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, WizardRow::Choice { .. }))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(choices, vec![session.cursor]);
    session.handle(key(KeyCode::Down));
    assert_eq!(choices, vec![session.cursor]);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginRestore
    ));
}

#[test]
fn login_handoff_failures_have_distinct_feedback() {
    use crate::runtime::login::guide_protocol::{HandoffFailure, Outcome};
    let i18n = I18n::new(Language::EnUs);
    for (failure, expected) in [
        (
            HandoffFailure::Spawn {
                message: "no sudo binary".into(),
                kind: std::io::ErrorKind::NotFound,
            },
            "handoff failed: no sudo binary",
        ),
        (HandoffFailure::Cancelled, "cancelled"),
        (HandoffFailure::Usage, "exit 2"),
        (HandoffFailure::Exit(7), "exit 7"),
    ] {
        let message = login_message(i18n, Outcome::HandoffFailed(failure), None);
        assert!(message.to_lowercase().contains(expected), "{message}");
    }
}

#[test]
fn login_feedback_stays_visible_after_the_page_reloads() {
    use crate::runtime::login::guide_protocol::Outcome;
    let (_guard, _temp, paths) = isolated_fixture();
    std::env::set_var("COSH_SHELL_LANG", "en-US");
    let mut session = login_session(paths, login_status("/bin/bash", None, false, false));
    session.finish_login("restore", "/nonexistent/cosh-target", Outcome::Unconfirmed);
    let message = session.view().message.clone().expect("failure feedback");
    assert!(message.to_lowercase().contains("unconfirmed"), "{message}");
    session.handle(key(KeyCode::Down));
    assert_eq!(session.view().message.as_deref(), Some(message.as_str()));
}

#[test]
fn login_failure_with_an_unreadable_reread_stays_unconfirmed() {
    use crate::runtime::login::guide_protocol::{HandoffFailure, Outcome};
    let (_guard, _temp, paths) = isolated_fixture();
    std::env::set_var("COSH_SHELL_LANG", "en-US");
    let mut session = login_session(paths, login_status("/bin/bash", None, false, false));
    // The worker can apply the change before exiting nonzero. A failed
    // re-read therefore proves nothing, and the page must not claim the
    // change did not complete.
    session.finish_login(
        "set",
        COSH_PATH,
        Outcome::HandoffFailed(HandoffFailure::Exit(1)),
    );
    assert!(
        session.snapshot.login.is_err(),
        "the fixture must fail the post-handoff status read"
    );
    let message = session
        .view()
        .message
        .clone()
        .expect("unknown-state feedback");
    assert!(message.to_lowercase().contains("unconfirmed"), "{message}");
}

#[test]
fn login_other_user_command_is_readable_at_forty_columns() {
    let (_guard, _temp, paths) = isolated_fixture();
    for (language, command) in [
        (Language::EnUs, "`cosh login set --user USER`"),
        (Language::ZhCn, "`cosh login set --user 用户名`"),
    ] {
        let mut session =
            login_session(paths.clone(), login_status("/bin/bash", None, false, false));
        session.i18n = I18n::new(language);
        session.enter(Page::Login);
        let text = render_wizard_lines(&session.view(), 40, 24).join("\n");
        let expected = command.replace(' ', "");
        let shown = text.lines().any(|line| {
            line.split_whitespace()
                .collect::<String>()
                .contains(&expected)
        });
        assert!(
            shown,
            "{language:?} truncates the other-user command:\n{text}"
        );
    }
}

fn render_wizard_lines(
    view: &crate::ui::guide::WizardView,
    width: u16,
    height: u16,
) -> Vec<String> {
    use ratatui::{backend::TestBackend, Terminal};
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::guide::render(frame, view);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn login_outcomes_are_settled_against_the_reread_shell() {
    use crate::runtime::login::guide_protocol::{HandoffFailure, Outcome};
    let verified = || {
        Outcome::Verified(login_status(
            "/usr/bin/cosh",
            Some("/bin/bash"),
            true,
            false,
        ))
    };
    let failed = || Outcome::HandoffFailed(HandoffFailure::Exit(1));
    assert_eq!(
        settle_outcome(failed(), Some("/usr/bin/cosh"), "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        settle_outcome(failed(), Some("/bin/bash"), "/usr/bin/cosh"),
        failed()
    );
    assert_eq!(
        settle_outcome(failed(), None, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        settle_outcome(verified(), Some("/bin/bash"), "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        settle_outcome(verified(), Some("/usr/bin/cosh"), "/usr/bin/cosh"),
        verified()
    );
    assert_eq!(
        settle_outcome(verified(), None, "/usr/bin/cosh"),
        verified()
    );
}

fn login_status(
    shell: &str,
    original: Option<&str>,
    managed: bool,
    conflict: bool,
) -> crate::runtime::login::LoginStatus {
    crate::runtime::login::LoginStatus {
        username: "alice".into(),
        uid: 1000,
        current_shell: shell.into(),
        original_bash: original.map(str::to_string),
        transaction: if conflict {
            "conflict"
        } else if managed {
            "applied"
        } else if original.is_some() {
            "restored"
        } else {
            "unmanaged"
        }
        .into(),
        managed,
        conflict,
    }
}

fn login_session(paths: UserPaths, status: crate::runtime::login::LoginStatus) -> Session {
    login_session_with(paths, Ok(status))
}

fn login_session_with(
    paths: UserPaths,
    status: Result<crate::runtime::login::LoginStatus, String>,
) -> Session {
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.snapshot.login = status;
    session
}

fn choice_labels(session: &Session) -> Vec<String> {
    session
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            WizardRow::Choice { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn failed_login_outcomes_are_logged_without_error_payloads() {
    use crate::runtime::login::guide_protocol::{HandoffFailure, Outcome};
    let (_guard, _temp, paths) = isolated_fixture();
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.finish_login("set", "/nonexistent/cosh-target", Outcome::Unconfirmed);
        session.finish_login(
            "restore",
            "/nonexistent/cosh-target",
            Outcome::Error("SECRET_PASSWORD".to_string()),
        );
        // An unreadable re-read settles this failure to Unconfirmed, so the
        // spawn branch is exercised directly.
        log_login_outcome(
            "restore",
            &Outcome::HandoffFailed(HandoffFailure::Spawn {
                message: "SECRET_API_KEY".into(),
                kind: std::io::ErrorKind::NotFound,
            }),
        );
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("result=\"unconfirmed\""), "{logs}");
    assert!(logs.contains("operation=\"restore\""), "{logs}");
    assert!(logs.contains("stage=\"spawn\""), "{logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}

#[test]
fn recommendation_status_read_has_no_side_effects() {
    let _guard = env_guard();
    let (temp, paths) = fixture();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_RECOMMENDATIONS_ENABLED");
    let root = paths.cosh_dir.as_ref().unwrap().join("recommendations");
    let store = crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
    store.set_user_enabled(true, 0).unwrap();
    fs::remove_file(root.join("state.lock")).unwrap();
    fs::write(root.join("state.tmp"), b"stale transient payload").unwrap();

    let before = snapshot_dir(&root);
    let rec = status::load_recommendations(&paths);
    let after = snapshot_dir(&root);

    assert_eq!(rec.user_enabled, Reading::Value(true));
    assert_eq!(
        before, after,
        "status read must not create, delete, or rewrite store files"
    );
    assert!(!root.join("state.lock").exists());
}

#[test]
fn unreadable_config_shows_unknown_and_blocks_saves() {
    let _guard = env_guard();
    let (temp, paths) = fixture();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_ANALYSIS_MODE");
    std::env::remove_var("COSH_RECOMMENDATIONS_BASH_HISTORY");
    let config_dir = temp.path().join(".copilot-shell");
    fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    let broken = "this is = = not toml";
    fs::write(&config_path, broken).unwrap();

    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    assert!(session.analysis.unreadable);
    assert!(session.bash.unreadable);

    session.page = super::session::Page::Analysis;
    session.cursor = 0;
    session.shift(0);
    assert!(!session
        .tips_rows()
        .iter()
        .any(|row| matches!(row, crate::ui::guide::WizardRow::Choice { .. })));
    session.tips_open();
    assert_eq!(fs::read_to_string(&config_path).unwrap(), broken);

    session.page = super::session::Page::Data;
    session.cursor = 0;
    session.shift(0);
    assert!(session.tips_rows().iter().any(|row| matches!(
        row,
        crate::ui::guide::WizardRow::Text(text) if text.contains("Unable to read")
    )));
    session.tips_open();
    assert_eq!(fs::read_to_string(&config_path).unwrap(), broken);
}

#[test]
fn reload_refreshes_ui_language_after_handoff_config_change() {
    let _guard = env_guard();
    let (temp, paths) = fixture();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_LANG");
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    assert_eq!(session.language(), Language::EnUs);
    let config_dir = temp.path().join(".copilot-shell");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "[ui]\nlanguage = \"zh-CN\"\n",
    )
    .unwrap();
    session.reload();
    assert_eq!(session.language(), Language::ZhCn);
    assert_eq!(session.view().title, "Cosh 配置与入门");
}

#[test]
fn too_small_screen_only_accepts_exit_keys() {
    for code in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('Q')] {
        assert!(matches!(super::too_small_flow(key(code)), Flow::Exit));
    }
    for code in [KeyCode::Char('c'), KeyCode::Char('d')] {
        let ctrl = KeyEvent::new(code, KeyModifiers::CONTROL);
        assert!(matches!(super::too_small_flow(ctrl), Flow::Exit));
    }
    for code in [
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Char('a'),
        KeyCode::Char('c'),
        KeyCode::Char('d'),
    ] {
        assert!(matches!(super::too_small_flow(key(code)), Flow::Continue));
    }
}
