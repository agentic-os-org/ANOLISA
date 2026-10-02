use cosh_shell::adapter::{
    CoshCoreAdapter, SessionHealth, SessionRecoveryState, SessionRuntimeState,
};
use cosh_shell::{I18n, Language, MessageId};

#[test]
fn recovery_states_and_health_have_stable_user_labels() {
    assert_eq!(SessionRecoveryState::None.label(), "none");
    assert_eq!(SessionRecoveryState::Selected.label(), "selected");
    assert_eq!(SessionRecoveryState::Restoring.label(), "restoring");
    assert_eq!(SessionRecoveryState::Active.label(), "active");
    assert_eq!(SessionRecoveryState::Failed.label(), "failed");

    assert!(SessionHealth::Ready.can_resume());
    assert!(!SessionHealth::Corrupt.can_resume());
    assert!(!SessionHealth::Incompatible.can_resume());
    assert!(!SessionHealth::ScopeMismatch.can_resume());
}

#[test]
fn active_and_selected_provider_sessions_are_both_protected() {
    let active = "00000000-0000-4000-8000-000000000000".to_string();
    let selected = "11111111-1111-4111-8111-111111111111".to_string();
    let mut session = SessionRuntimeState::with_active(active.clone(), "/tmp");
    session.recovery.state = SessionRecoveryState::Selected;
    session.recovery.selected_session_id = Some(selected.clone());
    session.recovery.selected_workspace_scope = Some("/tmp".to_string());
    let adapter = CoshCoreAdapter::new("unused", false);
    *adapter.session.lock().unwrap() = session;

    assert_eq!(adapter.protected_session_ids(), vec![active, selected]);
}

#[test]
fn picker_footer_keeps_resume_and_clear_semantics_distinct() {
    let en = I18n::new(Language::EnUs).t(MessageId::SessionPickerFooter);
    assert!(en.contains("Enter resume"), "{en}");
    assert!(en.contains("Space toggle clear mark"), "{en}");
    assert!(en.contains("d review clear"), "{en}");
    assert!(en.contains("Esc cancel"), "{en}");
    assert!(!en.contains("Space mark for clear"), "{en}");

    let marked = I18n::new(Language::EnUs).t(MessageId::SessionPickerMarkedFooter);
    assert!(marked.contains("Enter review clear"), "{marked}");
    assert!(marked.contains("Space toggle clear mark"), "{marked}");
    assert!(marked.contains("d review clear"), "{marked}");
    assert!(marked.contains("Esc cancel"), "{marked}");
    assert!(!marked.contains("Enter resume"), "{marked}");

    let zh = I18n::new(Language::ZhCn).t(MessageId::SessionPickerFooter);
    assert!(zh.contains("Enter 恢复"), "{zh}");
    assert!(zh.contains("Space 切换清理标记"), "{zh}");
    assert!(zh.contains("d 打开清理确认"), "{zh}");
    assert!(zh.contains("Esc 取消"), "{zh}");

    let marked_zh = I18n::new(Language::ZhCn).t(MessageId::SessionPickerMarkedFooter);
    assert!(marked_zh.contains("Enter 打开清理确认"), "{marked_zh}");
    assert!(marked_zh.contains("Space 切换清理标记"), "{marked_zh}");
    assert!(marked_zh.contains("d 打开清理确认"), "{marked_zh}");
    assert!(marked_zh.contains("Esc 取消"), "{marked_zh}");

    // The confirmation-phase footer stays a separate message: y/Enter only
    // deletes after `d` has opened the confirmation.
    let confirm = I18n::new(Language::EnUs).t(MessageId::SessionClearConfirmFooter);
    assert!(confirm.contains("Enter or y confirms"), "{confirm}");
}
