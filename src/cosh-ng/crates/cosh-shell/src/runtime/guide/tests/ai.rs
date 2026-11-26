use super::*;
use crate::adapter::GuideAuthSummary;
use crate::ui::guide::WizardRow;

#[test]
fn guide_ai_late_summary_preserves_the_selected_action() {
    for management in [false, true] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(Language::EnUs));
        session.ai_loading = true;
        session.enter(super::super::session::Page::Ai);
        if management {
            session.handle(key(KeyCode::Down));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        session.ai_rx = Some(receiver);
        sender
            .send((
                session.ai_generation,
                Reading::Value(GuideAuthSummary {
                    configured: true,
                    source: Some("user".into()),
                    auto_use: Some(true),
                    ..Default::default()
                }),
            ))
            .unwrap();
        assert!(session.tick());
        let view = session.view();
        let expected = if management {
            "Manage AI services"
        } else {
            "Return to terminal to ask"
        };
        assert!(
            matches!(&view.rows[view.cursor], WizardRow::Choice {label, ..} if label == expected)
        );
        let flow = session.handle(key(KeyCode::Enter));
        assert!(if management {
            matches!(flow, Flow::AiManage)
        } else {
            matches!(flow, Flow::Ask)
        });
    }
}

#[test]
fn guide_ai_legacy_identity_keeps_preference_failures_and_disabled_use_visible() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    for (auto_use, preference_failed, expected) in [
        (None, true, "暂无法读取"),
        (Some(false), false, "已关闭"),
        (Some(true), false, "免配置自动接入，可直接提问"),
    ] {
        session.ai = Reading::Value(GuideAuthSummary {
            configured: true,
            legacy_ecs_identity: true,
            auto_use,
            preference_failed,
            service: Some("ecs-free-trial-2".into()),
            ..Default::default()
        });
        let value = session.ai_value();
        assert!(value.contains(expected), "{value}");
    }
}

#[test]
fn guide_ai_incomplete_route_without_service_never_claims_configured() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    session.ai = Reading::Value(GuideAuthSummary {
        route: Some("existing".into()),
        ..Default::default()
    });
    let value = session.ai_value();
    assert!(value.contains("配置不完整"), "{value}");
    assert!(!value.contains("已配置"), "{value}");
}

#[test]
fn home_opens_the_quick_guide_and_ai_page_offers_reading_before_management() {
    for language in [Language::EnUs, Language::ZhCn] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(language));
        assert!(matches!(
            session.handle(key(KeyCode::Enter)),
            Flow::Continue
        ));
        assert_eq!(session.page, super::super::session::Page::Start);
        session.enter(super::super::session::Page::Ai);
        assert!(matches!(
            session.handle(key(KeyCode::Enter)),
            Flow::Continue
        ));
        assert_eq!(session.page, super::super::session::Page::Start);
        session.enter(super::super::session::Page::Ai);
        session.handle(key(KeyCode::Down));
        assert!(matches!(
            session.handle(key(KeyCode::Enter)),
            Flow::AiManage
        ));
    }
}

#[test]
fn guide_ai_home_label_remains_visible_at_supported_terminal_sizes() {
    use ratatui::{backend::TestBackend, Terminal};

    for (language, label) in [(Language::EnUs, "AI service"), (Language::ZhCn, "AI 服务")] {
        let (_guard, _temp, paths) = isolated_fixture();
        let mut session = Session::new(paths, I18n::new(language));
        session.ai = Reading::Value(GuideAuthSummary {
            route: Some("ecs_identity".into()),
            identity_status: Some("not_checked".into()),
            auto_use: Some(true),
            ..Default::default()
        });
        let view = session.view();
        for (width, height) in [(40, 16), (80, 24), (100, 42)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui::guide::render(frame, &view);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let lines = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            assert!(
                lines
                    .iter()
                    .any(|line| line.replace(' ', "").contains(&label.replace(' ', ""))),
                "{width}x{height}: {lines:?}"
            );
        }
    }
}

#[test]
fn guide_ai_states_preserve_route_and_verification_boundaries() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    for (route, configured, auto_use, preference_failed, identity, expected) in [
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "supported",
            "免配置自动接入，可直接提问",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "not_checked",
            "免配置自动接入，可直接提问",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "unknown",
            "免配置自动接入，可直接提问",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "unsupported",
            "需要配置 AI 服务",
        ),
        (
            "ecs_identity",
            false,
            Some(true),
            false,
            "failed",
            "暂无法确认 ECS 接入",
        ),
        ("disabled", false, Some(false), false, "supported", "已关闭"),
        ("disabled", false, None, true, "not_checked", "暂无法读取"),
        (
            "existing",
            true,
            Some(false),
            false,
            "not_checked",
            "连接未验证",
        ),
        ("existing", true, None, true, "failed", "连接未验证"),
        (
            "existing",
            false,
            Some(true),
            false,
            "supported",
            "配置不完整",
        ),
    ] {
        session.ai = Reading::Value(GuideAuthSummary {
            route: Some(route.into()),
            configured,
            auto_use,
            preference_failed,
            identity_status: Some(identity.into()),
            ..Default::default()
        });
        let value = session.ai_value();
        assert!(value.contains(expected), "{route}/{identity}: {value}");
        assert!(!value.contains("已验证"), "{value}");
    }
    session.ai = Reading::Unknown;
    assert!(session.ai_value().contains("暂无法读取"));
}

#[test]
fn guide_ai_runtime_source_is_not_user_configuration() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::ZhCn));
    session.ai = Reading::Value(GuideAuthSummary {
        configured: true,
        source: Some("runtime".into()),
        ..Default::default()
    });
    session.enter(super::super::session::Page::Ai);
    let rows = session.view().rows;
    assert!(rows
        .iter()
        .any(|row| matches!(row, WizardRow::Text(text) if text.contains("运行时配置"))));
    assert!(!rows
        .iter()
        .any(|row| matches!(row, WizardRow::Text(text) if text.contains("用户配置"))));
}

#[test]
fn guide_ai_refresh_discards_obsolete_results() {
    let (_guard, _temp, paths) = isolated_fixture();
    let mut session = Session::new(paths, I18n::new(Language::EnUs));
    let (old_sender, old_receiver) = std::sync::mpsc::channel();
    session.ai_rx = Some(old_receiver);
    let (new_sender, new_receiver) = std::sync::mpsc::channel();
    session.ai_rx = Some(new_receiver);
    assert!(old_sender
        .send((
            session.ai_generation,
            Reading::Value(GuideAuthSummary::default()),
        ))
        .is_err());
    new_sender
        .send((
            session.ai_generation,
            Reading::Value(GuideAuthSummary {
                configured: true,
                service: Some("current".into()),
                ..Default::default()
            }),
        ))
        .unwrap();
    assert!(session.tick());
    assert!(session.ai_value().contains("current"));
}
