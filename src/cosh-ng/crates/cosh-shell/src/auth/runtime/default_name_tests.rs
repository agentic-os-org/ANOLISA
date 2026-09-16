use super::*;
use crate::auth::navigation::step_back;
use crate::auth::retry::restore_after_failed_submission_at;
use crate::auth::validation::{record_field_submission, FieldSubmission};

fn selecting(id: &str) -> InlineState {
    let mut auth = slash_auth_state(&[id], SysomMenu::on_manual());
    auth.phase = AuthPhase::SelectingProvider;
    auth.providers[0].fields = vec![
        field("provider_id", "Provider ID", false),
        field("base_url", "Base URL", false),
        field("model", "Model", false),
        field("api_key", "API Key", true),
    ];
    let mut state = InlineState::default();
    state.auth.state = Some(auth);
    state
}

#[test]
fn default_name_is_skipped_in_forward_and_backward_navigation() {
    let mut state = selecting("openai_compat");
    answer_selected_row(&mut state);
    handle_auth_answer(
        &adapter_without_registry(),
        &mut state,
        "auth-slash",
        "https://example.invalid",
        &mut Vec::new(),
    )
    .unwrap();
    let auth = state.auth.state.as_mut().unwrap();
    assert_eq!(auth.current_field_info().unwrap().name, "model");
    step_back(auth);
    assert_eq!(auth.current_field_info().unwrap().name, "base_url");
    step_back(auth);
    assert_eq!(auth.phase, AuthPhase::SelectingProvider);
    assert_eq!(auth.collected_values["provider_id"], "openai_compat");
}

#[test]
fn default_name_is_preserved_by_generic_and_field_failure_recovery() {
    for focus in [None, Some("model"), Some("missing_field")] {
        let mut state = selecting("openai_compat");
        answer_selected_row(&mut state);
        let auth = state.auth.state.as_mut().unwrap();
        auth.collected_values
            .insert("api_key".into(), "secret".into());
        restore_after_failed_submission_at(auth, focus);
        assert_eq!(
            auth.collected_values.get("provider_id").map(String::as_str),
            Some("openai_compat")
        );
        assert_eq!(
            auth.current_field_info().unwrap().name,
            if focus == Some("model") {
                "model"
            } else {
                "base_url"
            }
        );
        assert!(!auth.collected_values.contains_key("api_key"));
        while auth.phase == AuthPhase::FillingField {
            assert_ne!(auth.current_field_info().unwrap().name, "provider_id");
            step_back(auth);
        }
    }
}

#[test]
fn default_name_survives_a_real_submission_failure() {
    let mut state = selecting("dashscope");
    state.auth.state.as_mut().unwrap().providers[0]
        .fields
        .truncate(2);
    answer_selected_row(&mut state);
    let mut output = Vec::new();
    handle_auth_answer(
        &adapter_without_registry(),
        &mut state,
        "auth-slash",
        "https://example.invalid",
        &mut output,
    )
    .unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Credentials were not saved"));
    let auth = state.auth.state.as_ref().unwrap();
    assert_eq!(auth.current_field_info().unwrap().name, "base_url");
    assert_eq!(auth.collected_values["provider_id"], "dashscope");
}

#[test]
fn each_source_blocks_same_type_or_cross_type_name_reuse() {
    for id in [
        "aliyun",
        "dashscope",
        "coding_plan",
        "token_plan",
        "openai_compat",
    ] {
        for source in ["user", "system", "runtime"] {
            for (name, provider_type) in [("prod", id), (id, "other_type")] {
                let mut state = selecting(id);
                let auth = state.auth.state.as_mut().unwrap();
                auth.existing_providers = vec![ExistingProvider {
                    name: name.into(),
                    provider_type: provider_type.into(),
                    source: source.into(),
                    ..saved_dashscope()
                }];
                answer_selected_row(&mut state);
                let auth = state.auth.state.as_mut().unwrap();
                assert_eq!(auth.current_field_info().unwrap().name, "provider_id");
                assert!(!auth.collected_values.contains_key("provider_id"));
                let field = auth.current_field_info().cloned().unwrap();
                assert_eq!(
                    record_field_submission(auth, Some(&field), name.into()),
                    FieldSubmission::Rejected,
                    "{id}/{source}/{name}"
                );
                assert!(auth.field_error.as_deref().unwrap().contains("already"));
                assert_eq!(
                    record_field_submission(auth, Some(&field), "new-name".into()),
                    FieldSubmission::Accepted
                );
            }
        }
    }
}

#[test]
fn occupied_default_name_is_not_offered_as_a_placeholder() {
    let mut state = selecting("dashscope");
    let auth = state.auth.state.as_mut().unwrap();
    auth.providers[0].fields[0].placeholder = Some("dashscope".into());
    auth.existing_providers = vec![ExistingProvider {
        name: "dashscope".into(),
        provider_type: "other_type".into(),
        ..saved_dashscope()
    }];
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_ref().unwrap();
    assert_eq!(auth.current_field_info().unwrap().placeholder, None);
    assert!(auth.field_error.as_deref().unwrap().contains("already"));
}

#[test]
fn typed_template_name_remains_editable_when_same_type_already_exists() {
    let mut state = selecting("dashscope");
    state.auth.state.as_mut().unwrap().existing_providers = vec![saved_dashscope()];
    answer_selected_row(&mut state);
    handle_auth_answer(
        &adapter_without_registry(),
        &mut state,
        "auth-slash",
        "dashscope",
        &mut Vec::new(),
    )
    .unwrap();
    let auth = state.auth.state.as_mut().unwrap();
    step_back(auth);
    assert_eq!(auth.current_field_info().unwrap().name, "provider_id");
    assert_eq!(auth.field_input, "dashscope");
}

#[test]
fn template_switch_discards_previous_identity_and_secrets() {
    let mut state = selecting("dashscope");
    let mut other = state.auth.state.as_ref().unwrap().providers[0].clone();
    other.id = "coding_plan".into();
    state.auth.state.as_mut().unwrap().providers.push(other);
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_mut().unwrap();
    auth.collected_values
        .insert("api_key".into(), "secret".into());
    step_back(auth);
    assert_eq!(auth.phase, AuthPhase::SelectingProvider);
    auth.selected_provider = 1;
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_mut().unwrap();
    assert_eq!(auth.collected_values["provider_id"], "coding_plan");
    assert!(!auth.collected_values.contains_key("api_key"));
    step_back(auth);
    auth.existing_providers = vec![saved_dashscope()];
    auth.selected_provider = 0;
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_ref().unwrap();
    assert_eq!(auth.current_field_info().unwrap().name, "provider_id");
    assert!(auth.collected_values.is_empty());
}

#[test]
fn sysom_shortcut_name_field_returns_to_management() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    auth.providers[0].fields = vec![field("provider_id", "Provider ID", false)];
    auth.existing_providers = vec![ExistingProvider {
        provider_type: "aliyun".into(),
        ..saved_dashscope()
    }];
    assert!(begin_sysom_shortcut(&mut auth));
    step_back(&mut auth);
    assert_eq!(auth.phase, AuthPhase::ManagingProviders);
    assert_eq!(
        management_entry(
            &auth.sysom,
            auth.existing_providers.len(),
            auth.selected_provider
        ),
        AuthManagementEntry::SysomShortcut
    );
}

#[test]
fn template_picker_default_aliyun_still_applies_prepare() {
    let mut state = selecting("aliyun");
    state.auth.state.as_mut().unwrap().sysom = SysomMenu::on_ecs(ecs_prepare());
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_ref().unwrap();
    assert!(matches!(auth.phase, AuthPhase::AliyunEcsChallenge { .. }));
    assert_eq!(auth.collected_values["provider_id"], "aliyun");
    assert_eq!(auth.collected_values["auth_source"], "ecs_ram_role");
}

#[test]
fn default_aliyun_without_cached_prepare_does_not_silently_use_manual_fields() {
    let mut state = selecting("aliyun");
    state.auth.state.as_mut().unwrap().sysom = SysomMenu::default();
    assert!(handle_auth_answer(
        &adapter_without_registry(),
        &mut state,
        "auth-slash",
        "",
        &mut Vec::new()
    )
    .unwrap());
    let auth = state.auth.state.as_ref().unwrap();
    assert_eq!(auth.phase, AuthPhase::AliyunEcsPreparing);
    assert_eq!(auth.collected_values["provider_id"], "aliyun");
}

#[test]
fn default_name_without_an_id_field_does_not_skip_a_credential() {
    let mut state = selecting("dashscope");
    state.auth.state.as_mut().unwrap().providers[0]
        .fields
        .remove(0);
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_ref().unwrap();
    assert_eq!(auth.current_field, 0);
    assert_eq!(auth.current_field_info().unwrap().name, "base_url");
    assert_eq!(
        auth.collected_values.get("provider_id").map(String::as_str),
        Some("dashscope")
    );
}

#[test]
fn empty_template_submits_after_resolving_identity() {
    let mut state = selecting("dashscope");
    state.auth.state.as_mut().unwrap().providers[0]
        .fields
        .clear();
    let mut output = Vec::new();
    handle_auth_answer(
        &adapter_without_registry(),
        &mut state,
        "auth-slash",
        "",
        &mut output,
    )
    .unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Credentials were not saved"));
    assert_eq!(
        state.auth.state.as_ref().unwrap().collected_values["provider_id"],
        "dashscope"
    );
}

#[test]
fn only_identity_failure_reopens_default_naming() {
    let mut state = selecting("dashscope");
    answer_selected_row(&mut state);
    let auth = state.auth.state.as_mut().unwrap();
    restore_after_failed_submission_at(auth, Some("provider_id"));
    assert_eq!(auth.current_field_info().unwrap().name, "provider_id");
    let field = auth.current_field_info().cloned().unwrap();
    assert_eq!(
        record_field_submission(auth, Some(&field), "new-name".into()),
        FieldSubmission::Accepted
    );
    restore_after_failed_submission_at(auth, None);
    assert_eq!(auth.current_field_info().unwrap().name, "provider_id");
}
