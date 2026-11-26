use super::login::LoginChoice;
use super::session::{Flow, Session};
use super::status::{self, Reading};
use crate::config::guide::cli::UserPaths;
use crate::config::guide::GuideStore;
use crate::config::{private_state, Language};
use crate::i18n::I18n;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

mod acceptance;
mod ai;

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
        "LC_ALL",
        "LC_MESSAGES",
        "LANG",
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
    std::env::remove_var("LC_ALL");
    std::env::remove_var("LC_MESSAGES");
    std::env::remove_var("LANG");
    (guard, temp, paths)
}

#[test]
fn guide_ai_unknown_access_invites_questions_and_keeps_management_secondary() {
    use crate::adapter::GuideAuthSummary;
    use crate::ui::guide::WizardRow;
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    session.ai = Reading::Value(GuideAuthSummary {
        auto_use: Some(true),
        ..Default::default()
    });
    session.enter(super::session::Page::Ai);
    let view = session.view();
    assert!(view.rows.iter().any(
        |row| matches!(row, WizardRow::Text(text) if text.contains("免配置自动接入，可直接提问"))
    ));
    assert!(
        matches!(&view.rows[view.cursor], WizardRow::Choice {label, ..} if label == "回终端去提问")
    );
    assert!(matches!(session.handle(key(KeyCode::Enter)), Flow::Ask));
    session.handle(key(KeyCode::Down));
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::AiManage
    ));
}

#[test]
fn guide_ai_configured_without_name_is_still_configured() {
    use crate::adapter::GuideAuthSummary;
    use crate::ui::guide::WizardRow;
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    session.ai = Reading::Value(GuideAuthSummary {
        configured: true,
        ..Default::default()
    });
    session.enter(super::session::Page::Ai);
    assert!(session.view().rows.iter().any(|row| matches!(row, WizardRow::Text(text) if text.contains("已配置 AI 接入") && text.contains("连接未验证"))));
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
    session.cursor = 0;
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
    session.select_home_entry(0);
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
    session.open_subpage(super::session::Page::Personal);
    session.open_subpage(super::session::Page::DataPrivacy);
    assert!(session.view().markdown.is_some());
    session.handle(key(KeyCode::Down));
    assert_eq!(session.view().scroll, 1);
    session.handle(key(KeyCode::PageDown));
    assert_eq!(session.view().scroll, 11);
    session.handle(key(KeyCode::Up));
    assert_eq!(session.view().scroll, 10);
    session.handle(key(KeyCode::Esc));
    assert!(matches!(session.page, super::session::Page::Personal));
}

#[test]
fn language_save_persists_stays_on_page_and_refreshes_the_ui_language() {
    let _guard = env_guard();
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_LANG");
    let (_temp, paths) = fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.select_home_entry(4);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.view().title, "Language / 语言");
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    let config = fs::read_to_string(temp.path().join(".copilot-shell/config.toml")).unwrap();
    assert!(config.contains("zh-CN"), "{config}");
    assert_eq!(session.view().title, "语言 / Language");
    assert!(session
        .view()
        .message
        .unwrap()
        .contains("界面语言已切换为 简体中文"));
}

#[test]
fn language_escape_keeps_the_draft_unsaved() {
    let _guard = env_guard();
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", temp.path());
    std::env::remove_var("COSH_SHELL_LANG");
    let (_temp, paths) = fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.cursor = 0;
    session.handle(key(KeyCode::Enter));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Esc));
    assert!(!temp.path().join(".copilot-shell/config.toml").exists());
}

#[test]
fn health_save_reports_the_next_login_scope() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.cursor = 0;
    for _ in 0..3 {
        session.handle(key(KeyCode::Down));
    }
    session.handle(key(KeyCode::Enter));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    let message = session.view().message.unwrap();
    assert!(message.contains("Login health summary: off"), "{message}");
    assert!(message.contains("next login"), "{message}");
    let store = GuideStore::open(paths.cosh_dir.as_ref().unwrap().join("guide")).unwrap();
    assert!(!store.peek_preferences().unwrap().unwrap().banner_enabled);
}

#[test]
fn health_save_states_the_saved_result_once() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.cursor = 0;
    for _ in 0..3 {
        session.handle(key(KeyCode::Down));
    }
    session.handle(key(KeyCode::Enter));
    session.handle(key(KeyCode::Down));
    session.handle(key(KeyCode::Enter));
    let message = session.view().message.unwrap();
    assert_eq!(message.matches("next login").count(), 1, "{message}");
}

#[test]
fn login_confirmation_hint_follows_the_focused_action() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    session.handle(key(KeyCode::Enter));
    let hint = session.view().hint;
    assert!(hint.contains("Enter cancel"), "{hint}");
    render_confirmation(&mut session);
    session.handle(key(KeyCode::Up));
    let hint = session.view().hint;
    assert!(hint.contains("Enter set Cosh"), "{hint}");
}

#[test]
fn ctrl_c_exits_from_any_page() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths.clone(), I18n::new(Language::EnUs));
    session.enter(super::session::Page::Language);
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
    session.page = super::session::Page::Personal;
    session.select_choice_for_test(2); // import follows the two recommendation choices
    session.tips_open();
    render_confirmation(&mut session);
    session.select_choice_for_test(0);
    session.handle(key(KeyCode::Enter));
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
    session.enter(super::session::Page::Personal);
    session.select_choice_for_test(3); // clear data only
    session.tips_open();
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    assert_eq!(store.peek().unwrap().unwrap().store_epoch, epoch_before);
    session.handle(key(KeyCode::Enter)); // default Cancel
    assert!(matches!(session.page, super::session::Page::Personal));
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    session.enter(super::session::Page::Personal);
    session.select_choice_for_test(3);
    session.tips_open();
    session.handle(key(KeyCode::Esc));
    assert!(matches!(session.page, super::session::Page::Personal));
    assert_eq!(store.peek().unwrap().unwrap().journal.records.len(), 2);
    session.enter(super::session::Page::Personal);
    session.select_choice_for_test(3);
    session.tips_open();
    render_confirmation(&mut session);
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
        session.enter(super::session::Page::Personal);
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
            session.enter(Page::Personal);
            session.cursor = session.view().rows.iter().position(|row| {
                matches!(row, WizardRow::Choice { label: text, .. } if text == session.i18n.t(label))
            }).unwrap();
            session.handle(key(KeyCode::Enter));
            assert!(session.page == page);
            assert_eq!(before, snapshot_dir(&root));
            if page != Page::DataPrivacy {
                session.handle(key(KeyCode::Enter));
                assert!(session.page == Page::Personal);
                assert_eq!(before, snapshot_dir(&root));
            }
        }
        session.enter(Page::Personal);
        session.open_subpage(Page::DataResetConfirm);
        render_confirmation(&mut session);
        session.shift(-1);
        session.handle(key(KeyCode::Enter));
        assert!(session.page == Page::Personal);
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

fn login_status(
    current_shell: &str,
    original_bash: Option<&str>,
    transaction: &str,
    managed: bool,
    conflict: bool,
) -> crate::runtime::login::LoginStatus {
    crate::runtime::login::LoginStatus {
        username: "root".into(),
        uid: 0,
        current_shell: current_shell.into(),
        original_bash: original_bash.map(str::to_string),
        transaction: transaction.into(),
        managed,
        conflict,
    }
}

fn login_session(
    status: crate::runtime::login::LoginStatus,
) -> (EnvGuard, tempfile::TempDir, Session) {
    let (guard, temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.snapshot.login = Ok(status);
    session.enter(super::session::Page::Login);
    (guard, temp, session)
}

fn login_choice_labels(session: &Session) -> Vec<String> {
    session
        .login_rows()
        .iter()
        .filter_map(|row| match row {
            crate::ui::guide::WizardRow::Choice { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn restored_login_page_offers_set_and_other_only() {
    let (_guard, _temp, session) = login_session(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    assert_eq!(
        session.login_choices(),
        vec![LoginChoice::Set, LoginChoice::Other]
    );
    assert_eq!(
        login_choice_labels(&session),
        vec![
            "Set Cosh as login shell",
            "View other-user management commands"
        ]
    );
}

#[test]
fn restored_login_second_choice_routes_to_other() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    let second_choice = session
        .login_rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, crate::ui::guide::WizardRow::Choice { .. }))
        .nth(1)
        .unwrap()
        .0;
    session.cursor = second_choice;
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginOther
    ));
}

#[test]
fn managed_login_page_offers_restore_and_other_only() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/usr/bin/cosh",
        Some("/bin/bash"),
        "applied",
        true,
        false,
    ));
    assert_eq!(
        session.login_choices(),
        vec![LoginChoice::Restore, LoginChoice::Other]
    );
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::Continue
    ));
    render_confirmation(&mut session);
    session.select_choice_for_test(0);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginRestore
    ));
}

#[test]
fn unmanaged_bash_login_page_offers_set_and_other() {
    let (_guard, _temp, session) =
        login_session(login_status("/bin/bash", None, "unmanaged", false, false));
    assert_eq!(
        session.login_choices(),
        vec![LoginChoice::Set, LoginChoice::Other]
    );
}

#[test]
fn unmanaged_cosh_login_page_has_no_mutation_choice() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/usr/bin/cosh",
        None,
        "unmanaged",
        false,
        false,
    ));
    assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginReread
    ));
}

#[test]
fn conflict_login_page_has_no_mutation_choice() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/bin/zsh",
        Some("/bin/bash"),
        "conflict",
        false,
        true,
    ));
    assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginReread
    ));
}

#[test]
fn read_error_login_page_has_no_mutation_choice() {
    let (_guard, _temp, mut session) =
        login_session(login_status("/bin/bash", None, "unmanaged", false, false));
    session.snapshot.login = Err("read failed".into());
    assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
    assert_eq!(login_choice_labels(&session), vec!["Reread login status"]);
    session.shift(0);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginReread
    ));
}

#[test]
fn login_cursor_clamps_when_choices_shrink() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/usr/bin/cosh",
        Some("/bin/bash"),
        "applied",
        true,
        false,
    ));
    let second_choice = session
        .login_rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, crate::ui::guide::WizardRow::Choice { .. }))
        .nth(1)
        .unwrap()
        .0;
    session.cursor = second_choice;
    session.snapshot.login = Ok(login_status(
        "/bin/zsh",
        Some("/bin/bash"),
        "conflict",
        false,
        true,
    ));
    session.shift(0);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginReread
    ));
}

#[test]
fn other_user_login_shows_cli_guidance_without_handoff() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    session.run_login("other");
    let rows = session.login_rows();
    for command in [
        "cosh login status --user USER",
        "cosh login set --user USER",
        "cosh login restore --user USER",
    ] {
        assert!(
            rows.iter().any(
                |row| matches!(row, crate::ui::guide::WizardRow::Text(text) if text == command)
            ),
            "{command} missing"
        );
    }
    assert_eq!(
        session.login_choices(),
        vec![LoginChoice::Set, LoginChoice::Other]
    );
}

#[test]
fn reread_login_reports_refresh() {
    let (_guard, _temp, mut session) =
        login_session(login_status("/bin/bash", None, "unmanaged", false, false));
    session.run_login("reread");
    let expected = match &session.snapshot.login {
        Ok(_) => "Login status reread.",
        Err(_) => "Unable to read",
    };
    assert_eq!(session.message.as_deref(), Some(expected));
}

#[test]
fn reread_login_message_follows_host_locale() {
    let (_guard, _temp, mut session) =
        login_session(login_status("/bin/bash", None, "unmanaged", false, false));
    std::env::set_var("LC_ALL", "zh_CN.UTF-8");
    session.run_login("reread");
    let expected = match &session.snapshot.login {
        Ok(_) => "已重新读取登录状态。",
        Err(_) => "无法读取",
    };
    assert_eq!(session.message.as_deref(), Some(expected));
}

#[test]
fn reread_message_id_maps_read_failure_to_read_failed() {
    let status = login_status("/bin/bash", None, "unmanaged", false, false);
    assert_eq!(
        super::login::reread_message_id(&Ok(status)),
        crate::i18n::MessageId::GuideWizardLoginRereadDone
    );
    assert_eq!(
        super::login::reread_message_id(&Err("read failed".into())),
        crate::i18n::MessageId::GuideWizardReadFailed
    );
    assert_eq!(
        I18n::new(Language::EnUs).t(crate::i18n::MessageId::GuideWizardLoginRereadDone),
        "Login status reread."
    );
    assert_eq!(
        I18n::new(Language::ZhCn).t(crate::i18n::MessageId::GuideWizardLoginRereadDone),
        "已重新读取登录状态。"
    );
    assert_eq!(
        I18n::new(Language::EnUs).t(crate::i18n::MessageId::GuideWizardReadFailed),
        "Unable to read"
    );
    assert_eq!(
        I18n::new(Language::ZhCn).t(crate::i18n::MessageId::GuideWizardReadFailed),
        "无法读取"
    );
}

#[test]
fn other_user_guidance_clears_stale_message() {
    let (_guard, _temp, mut session) = login_session(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    session.message = Some("stale failure notice".into());
    session.run_login("other");
    assert!(session.message.is_none());
    assert!(session.login_guidance);
}

#[test]
fn failed_login_outcomes_and_handoffs_are_logged_without_error_payloads() {
    use crate::runtime::login::guide_protocol::Outcome;
    let (_guard, _temp, paths) = isolated_fixture();
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.finish_login("set", Outcome::Unconfirmed);
        session.finish_login("restore", Outcome::Error("SECRET_PASSWORD".to_string()));
        session.run_login("other");
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("result=\"unconfirmed\""), "{logs}");
    assert!(logs.contains("operation=\"restore\""), "{logs}");
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

    session.page = super::session::Page::Personal;
    session.tips_default_focus();
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
        assert!(matches!(super::too_small_flow(ctrl), Flow::Continue));
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

#[test]
fn every_home_entry_returns_to_its_own_row_across_repeated_visits() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    for _ in 0..3 {
        for entry in 0..6 {
            session.select_home_entry(entry);
            session.handle(key(KeyCode::Enter));
            assert_ne!(session.page, super::session::Page::Home);
            session.handle(key(KeyCode::Esc));
            assert_eq!(session.page, super::session::Page::Home);
            assert_eq!(session.choice_index(), entry);
        }
    }
}

#[test]
fn tips_subpages_return_to_the_tips_row_that_opened_them() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.select_home_entry(5);
    session.handle(key(KeyCode::Enter));
    for entry in 0..2 {
        session.select_choice_for_test(entry);
        session.handle(key(KeyCode::Enter));
        assert_ne!(session.page, super::session::Page::Tips);
        session.handle(key(KeyCode::Esc));
        assert_eq!(session.page, super::session::Page::Tips);
        assert_eq!(session.choice_index(), entry);
    }
}

#[test]
fn turning_recommendations_off_asks_first_and_cancel_writes_nothing() {
    let (_guard, _temp, paths) = isolated_fixture();
    let store = crate::recommendation::personal_store::PersonalStore::open(
        paths.cosh_dir.as_ref().unwrap().join("recommendations"),
    )
    .unwrap();
    store.set_user_enabled(true, 1_000).unwrap();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.open_subpage(super::session::Page::Personal);
    session.shift(1);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::PersonalOffConfirm);
    let rows = session.view().rows;
    assert!(rows.iter().any(|row| matches!(
        row,
        crate::ui::guide::WizardRow::Text(text) if text.contains("Bash history")
    )));
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::Personal);
    assert_eq!(
        store.peek().unwrap().unwrap().preferences.user_enabled,
        Some(true)
    );
}

#[test]
fn confirming_recommendations_off_clears_once_and_returns_to_the_page() {
    let (_guard, _temp, paths) = isolated_fixture();
    let store = crate::recommendation::personal_store::PersonalStore::open(
        paths.cosh_dir.as_ref().unwrap().join("recommendations"),
    )
    .unwrap();
    store.set_user_enabled(true, 1_000).unwrap();
    store
        .merge(
            &crate::recommendation::personal_store::StateVersion::of(
                &store.peek().unwrap().unwrap(),
            ),
            1_000,
            |state| state.journal.records.push(seeded_record(0)),
        )
        .unwrap();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.open_subpage(super::session::Page::Personal);
    session.shift(1);
    session.handle(key(KeyCode::Enter));
    render_confirmation(&mut session);
    session.shift(-1);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::Personal);
    let state = store.peek().unwrap().unwrap();
    assert_eq!(state.preferences.user_enabled, Some(false));
    assert!(state.journal.records.is_empty());
    let message = session.view().message.unwrap();
    assert!(
        message.contains("cleared local recommendation data"),
        "{message}"
    );
}

#[test]
fn other_user_commands_expand_in_place_and_escape_leaves_the_login_page() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.snapshot.login = Ok(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    session.select_home_entry(1);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::Login);
    session.shift(1);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginOther
    ));
    session.run_login("other");
    assert_eq!(session.page, super::session::Page::Login);
    let rows = session.view().rows;
    assert!(rows.iter().any(|row| matches!(
        row,
        crate::ui::guide::WizardRow::Text(text) if text.contains("USER is a placeholder")
    )));
    session.handle(key(KeyCode::Esc));
    assert_eq!(session.page, super::session::Page::Home);
    assert_eq!(session.choice_index(), 1);
}

#[test]
fn rereview_fix_returning_to_login_keeps_the_semantic_action_when_choices_change() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.snapshot.login = Ok(login_status(
        "/usr/bin/cosh",
        Some("/bin/bash"),
        "applied",
        true,
        false,
    ));
    session.select_home_entry(1);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::Login);
    session.shift(1);
    assert_eq!(
        session.login_choices().get(session.choice_index()),
        Some(&LoginChoice::Other)
    );
    let frame = session.current_frame();
    session.snapshot.login = Ok(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    session.restore_frame(frame);
    assert_eq!(session.page, super::session::Page::Login);
    assert_eq!(
        session.login_choices().get(session.choice_index()),
        Some(&LoginChoice::Other)
    );
}

#[test]
fn rereview_fix_unknown_login_stays_read_only_after_reread_and_reentry() {
    use super::result::ActionKind;
    use crate::runtime::login::guide_protocol::Outcome;
    let (_guard, _temp, mut session) =
        login_session(login_status("/bin/bash", None, "unmanaged", false, false));
    session.select_home_entry(1);
    session.handle(key(KeyCode::Enter));
    // A refusal before any grant is certain: it must not hold later writes,
    // and it must not claim the change may already be committed.
    session.finish_login("set", Outcome::Denied);
    assert!(
        session.blocked_writes.is_empty(),
        "a certain pre-write refusal must not hold writes: {:?}",
        session.blocked_writes
    );
    let denied = session.message.clone().unwrap_or_default();
    assert!(
        denied.contains("Administrator authorization did not complete"),
        "{denied}"
    );
    assert!(!denied.contains("may already be committed"), "{denied}");
    assert!(
        !denied.contains("see the sudo message"),
        "resume clears sudo's output, so the message must not promise to show it: {denied}"
    );
    session.finish_login("set", Outcome::Unconfirmed);
    assert!(session.blocked_writes.contains(&ActionKind::LoginShell));
    for reread in [
        Ok(login_status(
            "/bin/bash",
            Some("/bin/bash"),
            "applied",
            true,
            false,
        )),
        Ok(login_status(
            "/usr/bin/cosh",
            Some("/bin/bash"),
            "applied",
            true,
            false,
        )),
        Err("account lookup unavailable".to_string()),
    ] {
        session.snapshot.login = reread.clone();
        session.enter(super::session::Page::Login);
        assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
        for action in ["set", "restore"] {
            session.run_login(action);
            assert!(session.message.as_deref().unwrap().contains("unconfirmed"));
        }
        session.run_login("reread");
        assert!(session.blocked_writes.contains(&ActionKind::LoginShell));
        assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
        session.snapshot.login = reread;
        assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
        let pending = session
            .i18n
            .t(crate::i18n::MessageId::GuideWizardLoginCommitPending);
        assert!(session.login_rows().iter().any(|row| matches!(row,
            crate::ui::guide::WizardRow::Text(text) if text == pending
        )));
        session.handle(key(KeyCode::Esc));
        assert_eq!(session.page, super::session::Page::Home);
        assert_eq!(session.choice_index(), 1);
        assert!(session.view().rows.iter().any(|row| matches!(row,
            crate::ui::guide::WizardRow::Choice { value, .. } if value == "Change unconfirmed"
        )));
        session.handle(key(KeyCode::Enter));
        assert_eq!(session.page, super::session::Page::Login);
        assert_eq!(session.login_choices(), vec![LoginChoice::Reread]);
        for restore in [false, true] {
            session.open_subpage(super::session::Page::LoginConfirm(restore));
            session.select_choice_for_test(0);
            assert!(matches!(
                session.handle(key(KeyCode::Enter)),
                Flow::Continue
            ));
            assert_eq!(session.page, super::session::Page::Login);
        }
    }
}

#[test]
fn rereview_fix_home_login_summary_keeps_actual_recovery_limits() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    for (status, expected) in [
        (
            login_status("/usr/bin/cosh", Some("/bin/bash"), "conflict", true, true),
            "conflict",
        ),
        (
            login_status("/bin/zsh", Some("/bin/bash"), "conflict", true, true),
            "conflict",
        ),
        (
            login_status("/usr/bin/cosh", None, "unmanaged", false, false),
            "recovery record",
        ),
    ] {
        session.snapshot.login = Ok(status);
        let value = session
            .view()
            .rows
            .into_iter()
            .filter_map(|row| match row {
                crate::ui::guide::WizardRow::Choice { value, .. } => Some(value),
                _ => None,
            })
            .nth(1)
            .unwrap();
        assert!(value.contains(expected), "{value}");
    }
}

#[test]
fn home_leads_with_the_quick_guide_and_keeps_the_default_login_shell_second() {
    for (language, expected) in [
        (
            Language::EnUs,
            [
                "Quick start guide",
                "Default login shell",
                "AI service",
                "Login health summary",
                "Language",
                "More settings",
            ],
        ),
        (
            Language::ZhCn,
            [
                "快速使用指南",
                "默认登录 Shell",
                "AI 服务",
                "登录健康摘要",
                "语言",
                "更多设置",
            ],
        ),
    ] {
        let (_guard, _temp, paths) = isolated_fixture();
        let session = Session::new(paths, I18n::new(language));
        let view = session.view();
        let labels = view
            .rows
            .iter()
            .filter_map(|row| match row {
                crate::ui::guide::WizardRow::Choice { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, expected);
    }
}

#[test]
fn more_settings_only_offers_analysis_and_recommendation_data() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::Tips);
    let labels = session
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::ui::guide::WizardRow::Choice { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(labels.len(), 2, "{labels:?}");
    assert!(labels[0].contains("analysis"), "{labels:?}");
    assert!(!labels.iter().any(|label| label.contains("Usage tips")));
}

#[test]
fn login_page_states_the_benefit_and_the_terminal_verification() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    session.snapshot.login = Ok(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    session.enter(super::session::Page::Login);
    let text = session
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::ui::guide::WizardRow::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("让每次登录直接进入 Cosh"), "{text}");
    assert!(text.contains("需要管理员授权"), "{text}");
}

#[test]
fn sudo_handoff_names_the_target_and_the_terminal_steps_without_a_wizard_page() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    session.snapshot.login = Ok(login_status(
        "/bin/bash",
        Some("/bin/bash"),
        "restored",
        false,
        false,
    ));
    let set = session.login_handoff_lines("set").join("\n");
    assert!(set.contains("目标账号："), "{set}");
    assert!(set.contains("UID"), "{set}");
    assert!(set.contains("将默认登录 Shell 设为 /usr/bin/cosh"), "{set}");
    assert!(set.contains("Ctrl-C 请求取消。"), "{set}");
    assert!(!set.contains("Esc"), "{set}");
    // The fixture account is root, and sudo never asks root for a password.
    assert!(!set.contains("如 sudo 要求密码"), "{set}");
    assert!(!set.contains("输入后按 Enter 继续"), "{set}");
    assert!(!set.contains("输入通常不显示字符或星号"), "{set}");
    assert!(!set.contains("无需密码时直接继续"), "{set}");
    let restore = session.login_handoff_lines("restore").join("\n");
    assert!(restore.contains("恢复为 /bin/bash"), "{restore}");

    let mut unprivileged = login_status("/bin/bash", Some("/bin/bash"), "restored", false, false);
    unprivileged.username = "coshtest".into();
    unprivileged.uid = 1000;
    session.snapshot.login = Ok(unprivileged);
    let unprivileged_set = session.login_handoff_lines("set").join("\n");
    assert!(
        unprivileged_set.contains("输入后按 Enter 继续。"),
        "{unprivileged_set}"
    );
    assert!(
        unprivileged_set.contains("如 sudo 要求密码，请在当前终端输入。"),
        "{unprivileged_set}"
    );
    assert!(
        unprivileged_set.contains("输入通常不显示字符或星号。"),
        "{unprivileged_set}"
    );
    assert!(
        unprivileged_set.contains("无需密码时直接继续。"),
        "{unprivileged_set}"
    );
}

#[test]
fn returning_to_the_data_list_never_lands_on_a_write_when_the_import_row_vanishes() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::Personal);
    session.select_choice_for_test(2); // import action
    let frame = session.current_frame();
    session.open_subpage(super::session::Page::DataPrivacy);
    session.bash.unreadable = true;
    session.restore_frame(frame);
    assert_eq!(
        session.personal_actions().get(session.choice_index()),
        Some(&super::tips::PersonalAction::Privacy)
    );
    let before = session.rec.user_enabled;
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::DataPrivacy);
    assert_eq!(session.rec.user_enabled, before);
}

#[test]
fn import_target_follows_the_saved_preference_under_an_environment_override() {
    for override_on in [true, false] {
        for saved_on in [true, false] {
            let _guard = env_guard();
            let (temp, paths) = fixture();
            std::env::set_var("HOME", temp.path());
            std::env::set_var(
                "COSH_RECOMMENDATIONS_BASH_HISTORY",
                if override_on { "1" } else { "0" },
            );
            crate::config::write_bash_history_checked(saved_on).unwrap();
            let mut session = Session::new(paths, I18n::new(Language::EnUs));
            assert_eq!(session.bash.saved, Some(saved_on));
            assert_eq!(session.bash.effective, override_on);
            session.enter(super::session::Page::Personal);
            session.select_choice_for_test(2);
            session.tips_open();
            if !saved_on {
                render_confirmation(&mut session);
                session.select_choice_for_test(0);
                session.handle(key(KeyCode::Enter));
            }
            assert_eq!(
                session.bash.saved,
                Some(!saved_on),
                "override={override_on} saved={saved_on}"
            );
            assert_eq!(session.bash.effective, override_on, "the env value stays");
            let message = session.view().message.unwrap_or_default();
            assert!(
                message.contains("COSH_RECOMMENDATIONS_BASH_HISTORY"),
                "{message}"
            );
            assert!(
                message.ends_with(if override_on { "to on." } else { "to off." }),
                "{message}"
            );
        }
    }
}

#[test]
fn environment_forced_off_recommendations_refuse_to_enable_without_saving() {
    let (_guard, _temp, paths) = isolated_fixture();
    let store = crate::recommendation::personal_store::PersonalStore::open(
        paths.cosh_dir.as_ref().unwrap().join("recommendations"),
    )
    .unwrap();
    store.set_user_enabled(false, 1_000).unwrap();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.rec.forced_off = true;
    session.enter(super::session::Page::Personal);
    session.select_choice_for_test(0); // enable
    session.tips_open();
    assert_eq!(session.rec.user_enabled, Reading::Value(false));
    assert_eq!(
        store.peek().unwrap().unwrap().preferences.user_enabled,
        Some(false)
    );
    let message = session.view().message.unwrap_or_default();
    assert!(!message.contains("Saved"), "{message}");
}

#[test]
fn an_unconfirmed_management_result_blocks_that_kind_for_the_rest_of_the_wizard() {
    use super::result::{ActionKind, ChangeFact, TaskOutcome, Verification};
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    let mut committed = TaskOutcome::default();
    committed.record(
        ActionKind::Configure,
        ChangeFact::Committed,
        Verification::Verified,
    );
    session.finish_ai_task(committed);
    assert!(session.blocked_writes.is_empty());

    let mut unknown = TaskOutcome::default();
    unknown.record(
        ActionKind::Delete,
        ChangeFact::Unknown,
        Verification::Unconfirmed,
    );
    session.finish_ai_task(unknown);
    assert_eq!(
        session.blocked_writes.iter().copied().collect::<Vec<_>>(),
        vec![ActionKind::Delete]
    );

    // Reopening management keeps the gate; a later success of another kind
    // must not clear it.
    let mut later = TaskOutcome::default();
    later.record(
        ActionKind::Activate,
        ChangeFact::Committed,
        Verification::Verified,
    );
    session.finish_ai_task(later);
    assert!(session.blocked_writes.contains(&ActionKind::Delete));
}

fn rendered(session: &Session, width: u16, height: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let view = session.view();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::guide::render(frame, &view);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw_session(session: &mut Session, width: u16, height: u16) -> String {
    let view = session.view();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    let mut drawn = None;
    terminal
        .draw(|frame| drawn = Some(crate::ui::guide::render(frame, &view)))
        .unwrap();
    session.apply_draw(drawn.unwrap());
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn render_confirmation(session: &mut Session) {
    draw_session(session, 80, 24);
}

#[test]
fn navigation_review_home_from_nested_privacy_restores_its_home_entry() {
    use super::session::Page;

    for shortcut in [KeyCode::F(2), KeyCode::Char('h')] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.select_home_entry(5);
        session.handle(key(KeyCode::Enter));
        session.select_choice_for_test(1);
        session.handle(key(KeyCode::Enter));
        session.open_subpage(Page::DataPrivacy);
        assert_eq!(session.page, Page::DataPrivacy);
        session.handle(key(shortcut));
        assert_eq!(session.page, Page::Home);
        assert_eq!(session.choice_index(), 5);
        assert!(matches!(session.handle(key(KeyCode::Esc)), Flow::Exit));
    }
}

#[test]
fn navigation_review_return_restores_the_source_scroll() {
    use super::session::Page;

    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(Page::Personal);
    session.scroll = 6;
    session.open_subpage(Page::DataPrivacy);
    session.handle(key(KeyCode::Esc));
    assert_eq!(session.page, Page::Personal);
    assert_eq!(session.scroll, 6);
    assert!(!session.view().follow_cursor);
}

#[test]
fn navigation_review_confirmation_footer_keeps_cancel_and_back_visible() {
    use super::session::Page;

    for (language, cancel, back) in [
        (Language::EnUs, "Entercancel", "Escback"),
        (Language::ZhCn, "Enter取消", "Esc返回"),
    ] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(language));
        session.enter(Page::DataClearConfirm);
        for (width, height) in [(40, 16), (80, 24)] {
            let text = rendered(&session, width, height);
            let footer: String = text.lines().skip(height as usize - 2).collect::<String>();
            let footer: String = footer.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(footer.contains(cancel) && footer.contains(back), "{text}");
        }
    }
}

#[test]
fn navigation_review_long_confirmation_cannot_select_write_before_review() {
    let mut status = login_status("/usr/bin/cosh", Some("/bin/bash"), "applied", true, false);
    status.username = "long-account-".repeat(16);
    status.original_bash = Some(format!("/opt/{}/bin/bash", "original/".repeat(32)));
    let (_guard, _temp, mut session) = login_session(status);
    session.handle(key(KeyCode::Enter));
    let text = draw_session(&mut session, 40, 16);
    assert!(text.contains("long-account"), "{text}");
    session.handle(key(KeyCode::Up));
    assert_eq!(session.choice_index(), 1);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::Continue
    ));
    assert_eq!(session.page, super::session::Page::Login);
    session.handle(key(KeyCode::Enter));
    draw_session(&mut session, 40, 16);
    for _ in 0..12 {
        session.handle(key(KeyCode::PageDown));
        draw_session(&mut session, 40, 16);
    }
    session.handle(key(KeyCode::Up));
    assert_eq!(session.choice_index(), 0);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginRestore
    ));
}

#[test]
fn navigation_review_quit_exits_read_only_pages() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::DataPrivacy);
    assert!(matches!(
        session.handle(key(KeyCode::Char('q'))),
        Flow::Exit
    ));
}

#[test]
fn navigation_review_ai_receipt_is_not_erased_by_quiet_browsing() {
    use super::result::{ActionKind, ChangeFact, TaskOutcome, Verification};
    use super::session::Page;

    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.select_home_entry(2);
    session.handle(key(KeyCode::Enter));
    let mut outcome = TaskOutcome::default();
    outcome.record(
        ActionKind::Configure,
        ChangeFact::Committed,
        Verification::Verified,
    );
    session.finish_ai_task(outcome);
    session.finish_ai_task(TaskOutcome::default());
    session.handle(key(KeyCode::Esc));
    assert_eq!(session.page, Page::Home);
    assert!(session
        .view()
        .message
        .as_deref()
        .is_some_and(|text| text.contains("saved")));
    session.handle(key(KeyCode::Enter));
    assert!(session
        .view()
        .message
        .as_deref()
        .is_some_and(|text| text.contains("AI service")));
    session.settle_ai_load();
}

#[test]
fn coordinator_review_home_keeps_unresolved_ai_outcomes_after_a_later_save() {
    use super::result::{ActionKind, ChangeFact, TaskOutcome, Verification};
    use super::session::Page;

    for (change, expected) in [
        (ChangeFact::Unknown, "AI service change unconfirmed."),
        (ChangeFact::Partial, "AI service change partly completed."),
        (ChangeFact::None, "AI service change not completed."),
    ] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.select_home_entry(2);
        session.handle(key(KeyCode::Enter));
        let mut earlier = TaskOutcome::default();
        if change == ChangeFact::None {
            earlier.record_failure(ActionKind::Configure, "earlier rejection".into());
        } else {
            earlier.record(ActionKind::Configure, change, Verification::Unconfirmed);
        }
        session.finish_ai_task(earlier);
        session.handle(key(KeyCode::Esc));
        session.handle(key(KeyCode::Enter));
        let mut later = TaskOutcome::default();
        later.record(
            ActionKind::TrialPreference,
            ChangeFact::Committed,
            Verification::Verified,
        );
        session.finish_ai_task(later);
        let receipt = session.ai_receipt().unwrap();
        assert_eq!(session.view().message.as_deref(), Some(receipt.as_str()));
        assert_eq!(receipt.matches("add or edit AI service").count(), 1);
        assert_eq!(receipt.matches("zero-config access setting").count(), 1);
        assert_eq!(
            session.blocked_writes.contains(&ActionKind::Configure),
            change == ChangeFact::Unknown
        );

        session.handle(key(KeyCode::Char('h')));
        assert_eq!(session.page, Page::Home);
        assert_eq!(session.choice_index(), 2);
        assert_eq!(session.view().message.as_deref(), Some(expected));
        session.handle(key(KeyCode::Enter));
        assert_eq!(session.view().message.as_deref(), Some(receipt.as_str()));
        assert!(matches!(
            session.handle(key(KeyCode::Char('q'))),
            Flow::Exit
        ));
        assert_eq!(session.ai_receipt().as_deref(), Some(receipt.as_str()));
        session.settle_ai_load();
    }
}

#[test]
fn coordinator_review_saved_configuration_survives_a_later_discard() {
    use super::result::{ActionKind, ChangeFact, EndReason, TaskOutcome, Verification};
    use super::session::Page;

    for (saved, home_key) in [(true, KeyCode::F(2)), (false, KeyCode::Char('h'))] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.select_home_entry(2);
        session.handle(key(KeyCode::Enter));
        if saved {
            let mut configure = TaskOutcome::default();
            configure.record(
                ActionKind::Configure,
                ChangeFact::Committed,
                Verification::Verified,
            );
            session.finish_ai_task(configure);
            session.handle(key(KeyCode::Esc));
            session.handle(key(KeyCode::Enter));
        }
        let mut discard = TaskOutcome::default();
        discard.reason = EndReason::Cancelled;
        session.finish_ai_task(discard);

        let expected = if saved {
            "This action was cancelled; saved changes are kept."
        } else {
            "Cancelled. No configuration changed."
        };
        let receipt = session.ai_receipt().unwrap();
        assert_eq!(session.view().message.as_deref(), Some(receipt.as_str()));
        assert_eq!(receipt.matches(expected).count(), 1, "{receipt}");
        assert_eq!(
            receipt.matches("Saved: add or edit AI service.").count(),
            usize::from(saved),
            "{receipt}"
        );
        if saved {
            assert!(!receipt.contains("No configuration changed"), "{receipt}");
        }

        session.handle(key(home_key));
        assert_eq!(session.page, Page::Home);
        assert_eq!(session.view().message.as_deref(), Some(expected));
        assert!(matches!(
            session.handle(key(KeyCode::Char('q'))),
            Flow::Exit
        ));
        assert_eq!(session.ai_receipt().as_deref(), Some(receipt.as_str()));
        session.settle_ai_load();
    }
}

#[test]
fn quick_guide_last_line_stays_reachable_at_40x16_in_both_languages() {
    for (language, tail) in [
        (Language::ZhCn, "会结束登录会话"),
        (Language::EnUs, "ends the login"),
    ] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(language));
        session.enter(super::session::Page::Start);
        session.scroll = usize::MAX;
        let text: String = rendered(&session, 40, 16)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            text.contains(&tail.replace(' ', "")),
            "{language:?}: {text}"
        );
    }
}

#[test]
fn login_benefit_and_action_share_the_first_screen_at_40x16() {
    for (language, benefit) in [
        (Language::ZhCn, "让每次登录直接进入"),
        (Language::EnUs, "UseCoshateverylogin"),
    ] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(language));
        session.snapshot.login = Ok(login_status(
            "/bin/bash",
            Some("/bin/bash"),
            "restored",
            false,
            false,
        ));
        session.enter(super::session::Page::Login);
        let text = rendered(&session, 40, 16).replace(' ', "");
        assert!(text.contains(benefit), "{language:?}: {text}");
        let action = session
            .i18n
            .t(crate::i18n::MessageId::GuideWizardLoginSet)
            .replace(' ', "");
        assert!(text.contains(&action), "{language:?}: {text}");
    }
}

#[test]
fn review_fix_login_requires_confirmation_and_defaults_to_cancel() {
    let (_guard, _temp, mut session) =
        login_session(login_status("/bin/bash", None, "unmanaged", false, false));
    session.select_choice_for_test(0);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::Continue
    ));
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::Continue
    ));
    assert_eq!(session.page, super::session::Page::Login);
    assert_eq!(session.choice_index(), 0);
    session.handle(key(KeyCode::Enter));
    render_confirmation(&mut session);
    session.select_choice_for_test(0);
    assert!(matches!(
        session.handle(key(KeyCode::Enter)),
        Flow::LoginSet
    ));
}

#[test]
fn review_fix_history_import_requires_consent_and_preserves_return_focus() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::Personal);
    session.select_choice_for_test(2);
    session.handle(key(KeyCode::Enter));
    assert_ne!(session.bash.saved, Some(true));
    let text = session
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::ui::guide::WizardRow::Text(value) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("AI service"), "{text}");
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, super::session::Page::Personal);
    assert_eq!(session.choice_index(), 2);
    assert_ne!(session.bash.saved, Some(true));
    session.handle(key(KeyCode::Enter));
    render_confirmation(&mut session);
    session.select_choice_for_test(0);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.bash.saved, Some(true));
    assert_eq!(session.choice_index(), 2);
    let message = session.view().message.unwrap();
    assert!(message.contains("new Cosh sessions"), "{message}");
    assert!(!message.contains("Stopping"), "{message}");
}

#[test]
fn review_fix_default_preferences_are_not_read_failures() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::Personal);
    let text = session
        .view()
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::ui::guide::WizardRow::Text(value) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("default"), "{text}");
    assert!(!text.contains("Unable to read"), "{text}");
}

#[test]
fn review_fix_import_failed_then_cancel_clears_feedback_like_escape() {
    for escape in [false, true] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.enter(super::session::Page::Personal);
        session.select_choice_for_test(2);
        session.handle(key(KeyCode::Enter));
        session.message = Some("Save failed".into());
        session.message_fresh = true;
        session.handle(key(if escape { KeyCode::Esc } else { KeyCode::Enter }));
        assert_eq!(session.page, super::session::Page::Personal);
        assert_eq!(session.choice_index(), 2);
        assert!(session.message.is_none());
        assert!(!session.message_fresh);
    }
}

#[test]
fn publish_fix_page_keys_read_all_long_result_facts() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.enter(super::session::Page::Ai);
    session.message = Some(
        (0..40)
            .map(|i| format!("result-{i:02}\n"))
            .collect::<String>(),
    );
    let before = rendered(&session, 40, 16);
    session.handle(key(KeyCode::PageDown));
    let after = rendered(&session, 40, 16);
    assert_ne!(before, after, "page key snapped back to the menu");
    let mut seen = before + &after;
    for _ in 0..5 {
        session.handle(key(KeyCode::PageDown));
        seen.push_str(&rendered(&session, 40, 16));
    }
    assert!(
        seen.contains("result-00") && seen.contains("result-39"),
        "{seen}"
    );
}

#[test]
fn publish_fix_unknown_login_and_read_failure_are_both_explained() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.finish_login(
        "set",
        crate::runtime::login::guide_protocol::Outcome::Unconfirmed,
    );
    session.snapshot.login = Err("state read failed".into());
    session.select_home_entry(1);
    session.handle(key(KeyCode::Enter));
    let text = session
        .view()
        .rows
        .into_iter()
        .filter_map(|r| match r {
            crate::ui::guide::WizardRow::Text(t) => Some(t),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("state read failed"), "{text}");
    assert!(text.contains("unconfirmed"), "{text}");
}

#[test]
fn publish_fix_cleanup_uncertainty_survives_enabling_and_reentry() {
    let (_guard, _temp, paths) = isolated_fixture();
    let root = paths.cosh_dir.as_ref().unwrap().join("recommendations");
    let store = crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
    store.initialize(status::now_hour_bucket()).unwrap();
    fs::create_dir(root.join("state.backup.json")).unwrap();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.select_home_entry(5);
    session.handle(key(KeyCode::Enter));
    session.select_choice_for_test(1);
    session.handle(key(KeyCode::Enter));
    session.select_choice_for_test(1);
    session.handle(key(KeyCode::Enter));
    render_confirmation(&mut session);
    session.select_choice_for_test(0);
    session.handle(key(KeyCode::Enter));
    assert_eq!(
        session.rec.user_enabled,
        super::status::Reading::Value(false)
    );
    fs::remove_dir(root.join("state.backup.json")).unwrap();
    session.select_choice_for_test(0);
    session.handle(key(KeyCode::Enter));
    assert_eq!(
        session.rec.user_enabled,
        super::status::Reading::Value(true)
    );
    session.handle(key(KeyCode::Esc));
    session.handle(key(KeyCode::Enter));
    let text = session
        .view()
        .rows
        .iter()
        .filter_map(|r| match r {
            crate::ui::guide::WizardRow::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("cleanup remains unconfirmed"), "{text}");
    let clear = session
        .personal_actions()
        .iter()
        .position(|x| *x == super::tips::PersonalAction::Clear)
        .unwrap();
    session.select_choice_for_test(clear);
    session.handle(key(KeyCode::Enter));
    render_confirmation(&mut session);
    session.select_choice_for_test(0);
    session.handle(key(KeyCode::Enter));
    assert!(!session.view().rows.iter().any(|r|matches!(r,crate::ui::guide::WizardRow::Text(t) if t.contains("cleanup remains unconfirmed"))));
}
