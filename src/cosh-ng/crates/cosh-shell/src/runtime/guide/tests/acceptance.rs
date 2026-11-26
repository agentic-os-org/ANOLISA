use super::*;
use crate::adapter::GuideAuthSummary;
use crate::runtime::guide::result::{ActionKind, ChangeFact, TaskOutcome, Verification};
use crate::runtime::guide::session::Page;
use crate::runtime::guide::tips::PersonalAction;
use crate::ui::guide::{DrawnView, WizardRow};
use ratatui::{backend::TestBackend, Terminal};
use std::collections::BTreeMap;

fn compact(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn frame(session: &mut Session, cols: u16, rows: u16) -> (Vec<String>, bool) {
    let view = session.view();
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    let mut drawn = None;
    terminal
        .draw(|screen| drawn = Some(crate::ui::guide::render(screen, &view)))
        .unwrap();
    let drawn = drawn.unwrap();
    let ready = drawn.review.is_some_and(|review| review.ready);
    session.apply_draw(drawn);
    let buffer = terminal.backend().buffer();
    let lines = (0..rows)
        .map(|y| (0..cols).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    (lines, ready)
}

fn observe(
    session: &Session,
    lines: &[String],
    cols: u16,
    visible: &mut BTreeMap<usize, String>,
) -> String {
    let width = usize::from(cols.saturating_sub(2).min(76));
    let top = 2 + crate::ui::display_width(&session.view().title).div_ceil(width);
    let footer_start = lines.len() - 2;
    let last_body = compact(&lines[footer_start - 1]);
    let indicator = last_body
        == compact(
            session
                .i18n
                .t(crate::i18n::MessageId::GuideWizardReviewHint),
        )
        || last_body.split_once('/').is_some_and(|(current, total)| {
            !current.is_empty()
                && !total.is_empty()
                && current.bytes().all(|character| character.is_ascii_digit())
                && total.bytes().all(|character| character.is_ascii_digit())
        });
    let body_end = footer_start - usize::from(indicator);
    for (offset, text) in lines[top..body_end].iter().enumerate() {
        let index = session.scroll + offset;
        if let Some(previous) = visible.insert(index, text.clone()) {
            assert_eq!(previous, *text, "visible row {index} changed during review");
        }
    }
    assert!(
        visible.keys().copied().eq(0..visible.len()),
        "review skipped rows"
    );
    compact(&visible.values().cloned().collect::<String>())
}

#[test]
fn goal_acceptance_login_disclosure_is_visible_before_write() {
    for (language, scope, sudo) in [
        (
            Language::EnUs,
            "Changes affect new logins only. Your current session continues unchanged.",
            "After confirming, verify privileges in this terminal as sudo prompts.",
        ),
        (
            Language::ZhCn,
            "仅影响新登录，已有会话保持不变。",
            "确认后在当前终端按 sudo 提示验证权限。",
        ),
    ] {
        for (cols, rows) in [(40, 16), (80, 24)] {
            for restore in [false, true] {
                let (_guard, _temp, paths) = isolated_fixture();
                let account = format!("user-{}account-tail", "long-account-".repeat(16));
                assert!(account.len() <= 256);
                let current = if restore {
                    "/usr/bin/cosh".to_string()
                } else {
                    format!(
                        "/opt/{}current-tail/bash",
                        "current-shell-segment/".repeat(64)
                    )
                };
                let target = if restore {
                    format!(
                        "/opt/{}target-tail/bash",
                        "original-shell-segment/".repeat(64)
                    )
                } else {
                    "/usr/bin/cosh".to_string()
                };
                let mut login = login_status(
                    &current,
                    restore.then_some(target.as_str()),
                    if restore { "applied" } else { "unmanaged" },
                    restore,
                    false,
                );
                login.username = account.clone();
                login.uid = 4_000_000_123;
                let mut session = Session::new(paths.clone(), I18n::new(language));
                session.snapshot.login = Ok(login.clone());
                session.enter(Page::Login);
                assert!(matches!(
                    session.handle(key(KeyCode::Enter)),
                    Flow::Continue
                ));
                assert_eq!(session.page, Page::LoginConfirm(restore));
                assert_eq!(session.choice_index(), 1);
                session.scroll = usize::MAX;
                let (first, ready) = frame(&mut session, cols, rows);
                assert_eq!(
                    session.scroll, 0,
                    "confirmation must start at the summary top"
                );
                assert!(!ready);
                assert!(
                    compact(&first[3]).contains("user-long-account-"),
                    "{first:?}"
                );
                assert!(matches!(session.handle(key(KeyCode::Up)), Flow::Continue));
                assert_eq!(session.choice_index(), 1);
                assert!(matches!(
                    session.handle(key(KeyCode::Enter)),
                    Flow::Continue
                ));
                assert_eq!(session.page, Page::Login);

                session.handle(key(KeyCode::Enter));
                let required = [
                    account.as_str(),
                    "UID4000000123",
                    current.as_str(),
                    target.as_str(),
                    scope,
                    sudo,
                ];
                let mut visible = BTreeMap::new();
                let mut steps = 0;
                loop {
                    let (lines, ready) = frame(&mut session, cols, rows);
                    let footer = compact(&lines[usize::from(rows) - 2..].join("\n"));
                    assert!(
                        footer.contains(if language == Language::EnUs {
                            "EntercancelEscback"
                        } else {
                            "Enter取消Esc返回"
                        }),
                        "{lines:?}"
                    );
                    let seen = observe(&session, &lines, cols, &mut visible);
                    let complete = required.iter().all(|text| seen.contains(&compact(text)));
                    if ready {
                        assert!(
                            complete,
                            "write unlocked before the full disclosure was visible: {seen}"
                        );
                        break;
                    }
                    assert!(matches!(session.handle(key(KeyCode::Up)), Flow::Continue));
                    assert_eq!(session.choice_index(), 1);
                    let mut probe = Session::new(paths.clone(), I18n::new(language));
                    probe.snapshot.login = Ok(login.clone());
                    probe.enter(Page::Login);
                    probe.handle(key(KeyCode::Enter));
                    probe.apply_draw(DrawnView {
                        scroll: session.scroll,
                        review: session.view().review,
                    });
                    probe.handle(key(KeyCode::Up));
                    assert!(matches!(probe.handle(key(KeyCode::Enter)), Flow::Continue));
                    assert_eq!(probe.page, Page::Login);
                    let previous = session.scroll;
                    session.handle(key(KeyCode::PageDown));
                    steps += 1;
                    assert!(steps < 256, "disclosure never completed");
                    assert!(session.scroll > previous);
                }
                assert!(
                    steps > 1,
                    "long disclosures must exercise several actual viewports"
                );
                session.handle(key(KeyCode::Up));
                assert_eq!(session.choice_index(), 0);
                let flow = session.handle(key(KeyCode::Enter));
                assert!(if restore {
                    matches!(flow, Flow::LoginRestore)
                } else {
                    matches!(flow, Flow::LoginSet)
                });
                assert_eq!(session.snapshot.login.as_ref().unwrap(), &login);
            }
        }
    }
}

#[test]
fn goal_acceptance_resize_restarts_review_and_cancel_restores_source() {
    for language in [Language::EnUs, Language::ZhCn] {
        for (from, to) in [((80, 24), (40, 16)), ((40, 16), (80, 24))] {
            let (_guard, _temp, paths) = isolated_fixture();
            let target = format!(
                "/opt/{}target-tail/bash",
                "original-shell-segment/".repeat(64)
            );
            let mut session = Session::new(paths, I18n::new(language));
            session.snapshot.login = Ok(login_status(
                "/usr/bin/cosh",
                Some(&target),
                "applied",
                true,
                false,
            ));
            session.select_home_entry(1);
            session.handle(key(KeyCode::Enter));
            session.handle(key(KeyCode::PageDown));
            let (source, _) = frame(&mut session, to.0, to.1);
            let source_scroll = session.scroll;
            assert!(source_scroll > 0);
            session.handle(key(KeyCode::Enter));
            let mut read = BTreeMap::new();
            loop {
                let (lines, ready) = frame(&mut session, from.0, from.1);
                let seen = observe(&session, &lines, from.0, &mut read);
                if ready {
                    assert!(seen.contains(&target));
                    break;
                }
                assert!(read.len() < 512);
                session.handle(key(KeyCode::PageDown));
            }
            session.handle(key(KeyCode::Up));
            assert_eq!(session.choice_index(), 0);
            session.on_resize();
            let (resized, ready) = frame(&mut session, to.0, to.1);
            assert!(!ready, "old read state must not cross a width change");
            assert_eq!(session.choice_index(), 1);
            assert_eq!(session.scroll, 0);
            assert!(compact(&resized[3]).contains("root"));
            session.handle(key(KeyCode::Up));
            assert_eq!(session.choice_index(), 1);
            assert!(matches!(
                session.handle(key(KeyCode::Enter)),
                Flow::Continue
            ));
            assert_eq!(session.page, Page::Login);
            assert_eq!(session.choice_index(), 0);
            assert_eq!(session.scroll, source_scroll);
            assert_eq!(frame(&mut session, to.0, to.1).0, source);
        }
    }
}

#[test]
fn goal_acceptance_privacy_returns_to_source_or_home_without_writes() {
    for shortcut in [KeyCode::F(2), KeyCode::Char('h')] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.select_home_entry(5);
        session.handle(key(KeyCode::Enter));
        session.select_choice_for_test(1);
        session.handle(key(KeyCode::Enter));
        let privacy = session
            .personal_actions()
            .iter()
            .position(|action| *action == PersonalAction::Privacy)
            .unwrap();
        session.select_choice_for_test(privacy);
        session.handle(key(KeyCode::PageDown));
        let (source, _) = frame(&mut session, 40, 16);
        let source_scroll = session.scroll;
        session.handle(key(KeyCode::Enter));
        assert_eq!(session.page, Page::DataPrivacy);
        session.handle(key(KeyCode::PageDown));
        assert!(matches!(session.handle(key(KeyCode::Esc)), Flow::Continue));
        assert_eq!(session.page, Page::Personal);
        assert_eq!(session.choice_index(), privacy);
        assert_eq!(session.scroll, source_scroll);
        assert_eq!(frame(&mut session, 40, 16).0, source);
        session.handle(key(KeyCode::Enter));
        session.handle(key(shortcut));
        assert_eq!(session.page, Page::Home);
        assert_eq!(session.choice_index(), 5);
        assert_eq!(session.navigation.depth(), 0);
        assert!(session.ai_receipt().is_none());
        assert!(matches!(session.handle(key(KeyCode::Esc)), Flow::Exit));
    }
}

#[test]
fn goal_acceptance_data_confirmations_disclose_and_cancel_without_changes() {
    for (language, history, credentials, server, import, quarantine) in [
        (
            Language::EnUs,
            "Source Bash history",
            "credentials",
            "server-side records",
            "saving does not import history now",
            "quarantine copy",
        ),
        (
            Language::ZhCn,
            "源 Bash 历史",
            "认证配置",
            "服务端记录",
            "本次保存不导入历史",
            "隔离副本",
        ),
    ] {
        for action in [
            PersonalAction::Clear,
            PersonalAction::Recommend(false),
            PersonalAction::Import,
            PersonalAction::Reset,
        ] {
            for exit in [
                KeyCode::Enter,
                KeyCode::Esc,
                KeyCode::F(2),
                KeyCode::Char('h'),
            ] {
                let (_guard, _temp, paths) = isolated_fixture();
                crate::config::write_bash_history_checked(false).unwrap();
                let root = paths.cosh_dir.as_ref().unwrap().join("recommendations");
                let store =
                    crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
                store
                    .set_user_enabled(true, status::now_hour_bucket())
                    .unwrap();
                if action == PersonalAction::Reset {
                    private_state::atomic_write(&root.join("state.json"), b"damaged", 4096)
                        .unwrap();
                }
                let data_before = snapshot_dir(&root);
                let config = root.parent().unwrap().parent().unwrap().join("config.toml");
                let config_before = fs::read(&config).unwrap();
                let mut session = Session::new(paths, I18n::new(language));
                session.select_home_entry(5);
                session.handle(key(KeyCode::Enter));
                session.select_choice_for_test(1);
                session.handle(key(KeyCode::Enter));
                let choice = session
                    .personal_actions()
                    .iter()
                    .position(|candidate| *candidate == action)
                    .expect("actual action must exist");
                session.select_choice_for_test(choice);
                session.handle(key(KeyCode::Enter));
                assert_eq!(session.choice_index(), 1);
                let expected_page = match action {
                    PersonalAction::Clear => Page::DataClearConfirm,
                    PersonalAction::Recommend(false) => Page::PersonalOffConfirm,
                    PersonalAction::Import => Page::DataImportConfirm,
                    PersonalAction::Reset => Page::DataResetConfirm,
                    _ => unreachable!(),
                };
                assert_eq!(session.page, expected_page);
                let markers = if action == PersonalAction::Import {
                    if language == Language::EnUs {
                        vec![
                            import,
                            "AI service",
                            "The first sync only builds a baseline",
                            "redaction cannot cover every secret",
                        ]
                    } else {
                        vec![
                            import,
                            "AI 服务",
                            "首次同步只建立读取基线",
                            "脱敏不能保证覆盖全部敏感内容",
                        ]
                    }
                } else {
                    let mut markers = vec![history, credentials, server];
                    if action == PersonalAction::Reset {
                        markers.push(quarantine);
                    }
                    markers
                };
                let mut read = BTreeMap::new();
                let mut ready = false;
                for _ in 0..64 {
                    let (lines, current_ready) = frame(&mut session, 40, 16);
                    let footer = compact(&lines[14..].join("\n"));
                    assert!(
                        footer.contains(if language == Language::EnUs {
                            "EntercancelEscback"
                        } else {
                            "Enter取消Esc返回"
                        }),
                        "{lines:?}"
                    );
                    let seen = observe(&session, &lines, 40, &mut read);
                    if current_ready {
                        for marker in &markers {
                            assert!(seen.contains(&compact(marker)), "{expected_page:?}: {seen}");
                        }
                        ready = true;
                        break;
                    }
                    session.handle(key(KeyCode::PageDown));
                }
                assert!(ready, "disclosure never fully appeared");
                assert_eq!(session.choice_index(), 1);
                assert!(matches!(session.handle(key(exit)), Flow::Continue));
                if matches!(exit, KeyCode::F(2) | KeyCode::Char('h')) {
                    assert_eq!(session.page, Page::Home);
                    assert_eq!(session.choice_index(), 5);
                } else {
                    assert_eq!(session.page, Page::Personal);
                    assert_eq!(session.choice_index(), choice);
                }
                assert_eq!(snapshot_dir(&root), data_before);
                assert_eq!(fs::read(&config).unwrap(), config_before);
            }
        }
    }
}

#[test]
fn goal_acceptance_ai_results_follow_destinations_and_language() {
    for (change, en, zh, home_en, home_zh) in [
        (
            ChangeFact::Committed,
            "Saved: add or edit AI service.",
            "已保存：新增或编辑 AI 服务。",
            "AI service changes saved.",
            "已保存 AI 服务设置。",
        ),
        (
            ChangeFact::Partial,
            "Model configuration saved, but the session record was not saved.",
            "模型配置已保存，会话记录未保存。",
            "AI service change partly completed.",
            "AI 服务操作部分完成。",
        ),
        (
            ChangeFact::Unknown,
            "Unconfirmed: add or edit AI service.",
            "结果未确认：新增或编辑 AI 服务。",
            "AI service change unconfirmed.",
            "AI 服务操作结果未确认。",
        ),
        (
            ChangeFact::None,
            "Not completed: add or edit AI service: synthetic rejection",
            "未完成：新增或编辑 AI 服务: synthetic rejection",
            "AI service change not completed.",
            "AI 服务操作未完成。",
        ),
    ] {
        let (_guard, _temp, paths) = isolated_fixture();
        std::env::set_var("COSH_SHELL_LANG", "en-US");
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.select_home_entry(2);
        session.handle(key(KeyCode::Enter));
        let mut outcome = TaskOutcome::default();
        if change == ChangeFact::None {
            outcome.record_failure(ActionKind::Configure, "synthetic rejection".into());
        } else {
            outcome.record(
                ActionKind::Configure,
                change,
                if change == ChangeFact::Committed {
                    Verification::Verified
                } else {
                    Verification::Unconfirmed
                },
            );
        }
        session.finish_ai_task(outcome);
        session.settle_ai_load();
        let mut later = TaskOutcome::default();
        later.record(
            ActionKind::TrialPreference,
            ChangeFact::Committed,
            Verification::Verified,
        );
        session.finish_ai_task(later);
        session.settle_ai_load();
        session.finish_ai_task(TaskOutcome::default());
        session.settle_ai_load();
        assert_eq!(session.ai_results.len(), 2);
        for (language, lang, detail, home, shortcut) in [
            (Language::EnUs, "en-US", en, home_en, KeyCode::F(2)),
            (Language::ZhCn, "zh-CN", zh, home_zh, KeyCode::Char('h')),
        ] {
            std::env::set_var("COSH_SHELL_LANG", lang);
            session.reload();
            assert_eq!(session.language(), language);
            let receipt = session.ai_receipt().unwrap();
            assert!(receipt.contains(detail), "{receipt}");
            assert_eq!(
                receipt.matches("synthetic rejection").count(),
                usize::from(change == ChangeFact::None)
            );
            assert_eq!(session.view().message.as_deref(), Some(receipt.as_str()));
            session.handle(key(shortcut));
            assert_eq!(session.page, Page::Home);
            assert_eq!(session.choice_index(), 2);
            assert_eq!(session.view().message.as_deref(), Some(home));
            for (cols, rows) in [(40, 16), (80, 24)] {
                assert!(
                    compact(&frame(&mut session, cols, rows).0.join("\n")).contains(&compact(home))
                );
            }
            session.handle(key(KeyCode::Enter));
            assert_eq!(session.page, Page::Ai);
            assert_eq!(session.view().message.as_deref(), Some(receipt.as_str()));
            assert_eq!(
                session.blocked_writes.contains(&ActionKind::Configure),
                change == ChangeFact::Unknown
            );
            assert!(!session
                .blocked_writes
                .contains(&ActionKind::TrialPreference));
            assert!(matches!(
                session.handle(key(KeyCode::Char('q'))),
                Flow::Exit
            ));
            assert_eq!(session.ai_receipt().as_deref(), Some(receipt.as_str()));
        }
    }
}

#[test]
fn goal_acceptance_ecs_routes_keep_direct_question_and_explicit_priority() {
    for (route, configured, auto_use, preference_failed, identity, ask, en, zh) in [
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "not_checked",
            true,
            "Zero-config automatic access; ask directly",
            "免配置自动接入，可直接提问",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "unknown",
            true,
            "Zero-config automatic access; ask directly",
            "免配置自动接入，可直接提问",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "supported",
            true,
            "Zero-config automatic access; ask directly",
            "免配置自动接入，可直接提问",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "failed",
            true,
            "ECS access could not be confirmed; try asking again",
            "暂无法确认 ECS 接入，可重试提问",
        ),
        (
            "existing",
            true,
            Some(false),
            true,
            "failed",
            true,
            "chosen-service · Connection unverified",
            "chosen-service · 连接未验证",
        ),
        (
            "existing",
            false,
            Some(true),
            false,
            "supported",
            false,
            "chosen-service · Configuration incomplete",
            "chosen-service · 配置不完整",
        ),
        (
            "disabled",
            false,
            Some(false),
            false,
            "supported",
            false,
            "Automatic ECS access is off",
            "ECS 自动接入已关闭",
        ),
        (
            "ecs_identity",
            false,
            None,
            true,
            "not_checked",
            false,
            "Unable to read AI access status",
            "暂无法读取 AI 接入状态",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "unsupported",
            false,
            "AI service configuration needed",
            "需要配置 AI 服务",
        ),
    ] {
        for (language, expected) in [(Language::EnUs, en), (Language::ZhCn, zh)] {
            let (_guard, _temp, paths) = isolated_fixture();
            let mut session = Session::new(paths, I18n::new(language));
            session.ai = Reading::Value(GuideAuthSummary {
                route: Some(route.into()),
                configured,
                auto_use,
                preference_failed,
                identity_status: Some(identity.into()),
                service: (route == "existing").then(|| "chosen-service".into()),
                source: (route == "existing").then(|| "user".into()),
                ..Default::default()
            });
            session.enter(Page::Ai);
            assert_eq!(session.ai_value(), expected);
            assert_eq!(
                session.choice_index(),
                usize::from(identity == "unsupported" || (route == "existing" && !configured))
            );
            if route == "existing" && !configured {
                assert!(session.view().rows.iter().any(|row| matches!(row, WizardRow::Text(text) if text.contains(if language == Language::EnUs { "No automatic trial fallback." } else { "不会自动切换试用接入。" }))));
            }
            session.select_choice_for_test(0);
            let flow = session.handle(key(KeyCode::Enter));
            if ask {
                assert!(matches!(flow, Flow::Ask));
                assert_eq!(session.page, Page::Ai);
            } else {
                assert!(matches!(flow, Flow::Continue));
                assert_eq!(session.page, Page::Start);
            }
            assert!(session.ai_receipt().is_none());
        }
    }
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    session.select_home_entry(2);
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.ai_value(), "Unable to read AI access status");
    session.handle(key(KeyCode::Enter));
    assert_eq!(session.page, Page::Start);
    session.handle(key(KeyCode::Esc));
    session.handle(key(KeyCode::Char('h')));
    assert_eq!(session.page, Page::Home);
    assert_eq!(session.choice_index(), 2);
    assert!(session.ai_receipt().is_none());
    assert!(matches!(
        session.handle(key(KeyCode::Char('q'))),
        Flow::Exit
    ));
}
