use super::display::display_agent_error;
use crate::i18n::I18n;

#[test]
fn trial_failures_show_model_setup_and_credit_guidance() {
    let zh = I18n::new(crate::Language::ZhCn);
    assert_eq!(
        display_agent_error("ModelNotConfigured", &zh),
        "尚未配置模型，可用 /auth 配置。"
    );
    assert_eq!(
        display_agent_error("provider_access:ModelNotConfigured", &zh),
        "尚未配置模型，可用 /auth 配置。"
    );
    assert!(
        display_agent_error("provider_access:Sysom.CreditsExhausted", &zh).contains("额度不足")
    );
    let en = I18n::new(crate::Language::EnUs);
    assert!(display_agent_error("provider_access:trial_cancelled", &en)
        .contains("question is retained"));
}

#[test]
fn access_copy_quota_is_generic_and_offers_console_or_other_provider() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let text = super::display::display_agent_error(
            "provider_access:Sysom.CreditsExhausted",
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
