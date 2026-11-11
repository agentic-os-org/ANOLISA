use super::display::display_agent_error;
use crate::i18n::I18n;

#[test]
fn trial_failures_show_model_setup_and_credit_guidance() {
    let zh = I18n::new(crate::Language::ZhCn);
    assert_eq!(
        display_agent_error("ModelNotConfigured", None, &zh),
        "尚未配置模型，可用 /auth 配置。"
    );
    assert_eq!(
        display_agent_error("provider_access:ModelNotConfigured", None, &zh),
        "尚未配置模型，可用 /auth 配置。"
    );
    assert!(
        display_agent_error("provider_access:Sysom.CreditsExhausted", None, &zh)
            .contains("额度不足")
    );
    let en = I18n::new(crate::Language::EnUs);
    assert!(
        display_agent_error("provider_access:trial_cancelled", None, &en)
            .contains("question is retained")
    );
}

#[test]
fn access_copy_quota_is_generic_and_offers_console_or_other_provider() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let text = super::display::display_agent_error(
            "provider_access:Sysom.CreditsExhausted",
            None,
            &crate::i18n::I18n::new(language),
        );
        assert!(text.contains("https://console.aliyun.com/alinux"));
        assert!(text.contains("/auth"));
        assert!(!text.contains("免费"));
        assert!(!text.to_ascii_lowercase().contains("free"));
        assert!(!text.to_ascii_lowercase().contains("month"));
    }
}

#[test]
fn kongche_hidden_trial_cancellation_keeps_terminal_facts_and_failed_cleanup() {
    let (mut run, _approval) = super::run::test_support::test_active_run_with_id("owner");
    super::events::record_trial_cancellation(
        &mut run,
        crate::types::AgentEvent::AgentFailed {
            run_id: "owner".into(),
            error: "provider_access:trial_cancelled".into(),
            error_code: Some("trial_cancelled".into()),
            max_turns: None,
        },
    );
    assert!(run.deferred_events.is_empty());
    assert!(
        matches!(run.governed_events.as_slice(), [event] if matches!(event.event, crate::types::AgentEvent::AgentFailed { .. }))
    );
    assert!(super::finish::failed_run_has_no_recovery(&run));
}

#[test]
fn trial_i18n_structured_failure_localizes_both_display_paths() {
    for (code, raw, expected) in [
        (
            "ModelNotConfigured",
            "no model configured; use /auth to configure or re-enable a model",
            "尚未配置模型，可用 /auth 配置。",
        ),
        (
            "trial_preferences_invalid",
            "automatic access preferences are unavailable; automatic model access is disabled; use /auth",
            "无法读取免配置接入设置，本次未自动接入。可用 /auth 重试或配置模型。",
        ),
        ("unknown_failure", "precise upstream failure", "precise upstream failure"),
    ] {
        let event = crate::types::AgentEvent::AgentFailed {
            run_id: "owner".into(),
            error: raw.into(),
            error_code: Some(code.into()),
            max_turns: None,
        };
        let governed = super::governance::govern_agent_events_with_language(
            &[event],
            &crate::types::Policy::default(),
            crate::Language::ZhCn,
        );
        assert_eq!(governed.events[0].display_text, expected);
        let (mut run, _approval) = super::run::test_support::test_active_run_with_id("owner");
        run.language = crate::Language::ZhCn;
        super::heartbeat::remember_agent_activity(&mut run, &governed.events);
        assert_eq!(run.current_message, expected);
    }
}

#[test]
fn trial_i18n_error_codes_preserve_legacy_text_and_unknown_failures() {
    for (language, expected) in [
        (crate::Language::ZhCn, "尚未配置模型，可用 /auth 配置。"),
        (
            crate::Language::EnUs,
            "No model configured. Use /auth to configure a model.",
        ),
    ] {
        let i18n = I18n::new(language);
        assert_eq!(
            display_agent_error("upstream wording", Some("ModelNotConfigured"), &i18n),
            expected
        );
        for code in [None, Some("unknown_failure")] {
            assert_eq!(
                display_agent_error("provider_access:ModelNotConfigured", code, &i18n),
                expected
            );
            assert_eq!(
                display_agent_error("precise upstream failure", code, &i18n),
                "precise upstream failure"
            );
        }
    }
}
