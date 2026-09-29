//! Unit tests for the `/auth` slash-command state machine.

#[path = "default_name_tests.rs"]
mod default_name_tests;

use super::{
    apply_aliyun_prepare, auth_validation_body, begin_sysom_shortcut,
    clear_ecs_auth_source_for_manual_aliyun_edit, clear_observed_model_after_provider_change,
    clear_observed_model_after_provider_delete, ecs_ram_role_prepare, handle_auth_answer,
    management_entry, render_auth_card_actions, restore_after_configure_failure,
    should_apply_aliyun_prepare_after_field, should_apply_aliyun_prepare_for_edit,
    should_apply_aliyun_prepare_on_provider_selection, AuthBackend, AuthFieldInfo,
    AuthManagementEntry, AuthPhase, AuthProviderInfo, CoreAuthPrepare, DeleteConfirmationOutcome,
    EcsRamRolePrepare, ExistingProvider, InlineState, RuntimeAuthState, ShellEvent, SysomMenu,
};
use crate::adapter::{AdapterInstance, FakeAgentAdapter};
use crate::auth::capture::auth_capture_id;
use std::collections::HashMap;

/// Any registry round-trip through this adapter fails, so reaching the ECS metadata
/// service a second time turns into a visible error instead of a silent probe.
fn adapter_without_registry() -> AdapterInstance {
    AdapterInstance::Fake(FakeAgentAdapter)
}

#[test]
fn validation_notice_explains_work_and_openai_fallback_cost() {
    let ordinary = auth_validation_body("deepseek", None);
    assert!(ordinary[0].contains("endpoint, credentials, and model"));
    assert_eq!(ordinary.len(), 1);

    let compatible = auth_validation_body("openai_compat", None);
    assert!(compatible
        .iter()
        .any(|line| line.contains("very small charge")));
    assert!(compatible.iter().any(|line| line.contains("max_tokens=1")));

    let dashscope = auth_validation_body("dashscope", None);
    assert!(dashscope
        .iter()
        .any(|line| line.contains("very small charge")));

    for plan in ["coding_plan", "token_plan"] {
        let notice = auth_validation_body(plan, None);
        assert!(notice.iter().any(|line| line.contains("amount of quota")));
        assert!(notice.iter().any(|line| line.contains("max_tokens=1")));
    }

    let manual_aliyun = auth_validation_body("aliyun", None);
    assert!(manual_aliyun
        .iter()
        .any(|line| line.contains("very small charge")));
    let ecs_aliyun = auth_validation_body("aliyun", Some("ecs_ram_role"));
    assert_eq!(ecs_aliyun.len(), 1);
}

fn template(id: &str) -> AuthProviderInfo {
    AuthProviderInfo {
        id: id.to_string(),
        label: id.to_string(),
        description: None,
        description_zh_cn: None,
        builtin_base_url: None,
        fields: Vec::new(),
    }
}

fn ecs_prepare() -> EcsRamRolePrepare {
    EcsRamRolePrepare {
        instance_id: "i-test-1".to_string(),
        console_url: "https://example.invalid/guide/cosh?instance=i-test-1".to_string(),
        values: HashMap::from([("auth_source".to_string(), "ecs_ram_role".to_string())]),
    }
}

fn slash_auth_state(templates: &[&str], sysom: SysomMenu) -> RuntimeAuthState {
    RuntimeAuthState {
        id: "auth-slash".to_string(),
        request_id: "slash".to_string(),
        phase: AuthPhase::ManagingProviders,
        providers: templates.iter().copied().map(template).collect(),
        selected_provider: 0,
        current_field: 0,
        collected_values: HashMap::new(),
        field_input: String::new(),
        field_error: None,
        field_capture_revision: 0,
        existing_providers: Vec::new(),
        editing_provider_name: None,
        default_provider_id: false,
        from_sysom_shortcut: false,
        error_message: None,
        backend: AuthBackend::CoreRegistry,
        sysom,
    }
}

#[test]
fn manual_prepare_mode_is_not_an_ecs_challenge() {
    assert!(ecs_ram_role_prepare(CoreAuthPrepare {
        mode: "manual".to_string(),
        instance_id: None,
        console_url: None,
        values: HashMap::new(),
    })
    .is_none());

    let prepare = ecs_ram_role_prepare(CoreAuthPrepare {
        mode: "ecs_ram_role".to_string(),
        instance_id: Some("i-test-1".to_string()),
        console_url: Some("https://example.invalid/guide".to_string()),
        values: HashMap::from([("auth_source".to_string(), "ecs_ram_role".to_string())]),
    })
    .expect("ecs mode is a challenge");
    assert_eq!(prepare.instance_id, "i-test-1");
    assert_eq!(
        prepare.values.get("auth_source").map(String::as_str),
        Some("ecs_ram_role")
    );

    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_manual());
    assert!(!apply_aliyun_prepare(&mut auth));
    assert_eq!(auth.phase, AuthPhase::ManagingProviders);
}

#[test]
fn sysom_shortcut_requires_name_for_an_existing_aliyun_type() {
    let mut auth = slash_auth_state(
        &["dashscope", "openai_compat", "aliyun"],
        SysomMenu::on_ecs(ecs_prepare()),
    );
    auth.providers[2].fields = vec![field("provider_id", "Provider ID", false)];
    auth.existing_providers = vec![ExistingProvider {
        name: "prod".to_string(),
        provider_type: "aliyun".to_string(),
        ..saved_dashscope()
    }];

    assert!(begin_sysom_shortcut(&mut auth));

    // The aliyun template is reused as-is; SysOM is not a provider type of its own.
    assert_eq!(auth.current_provider().id, "aliyun");
    assert_eq!(auth.phase, AuthPhase::FillingField);
    // Field 0 is the injected Provider ID, so the shortcut cannot overwrite a config.
    assert_eq!(auth.current_field, 0);
    assert!(auth.editing_provider_name.is_none());
    assert!(auth.collected_values.is_empty());
}

#[test]
fn sysom_shortcut_without_an_aliyun_template_reports_failure() {
    let mut auth = slash_auth_state(&["dashscope"], SysomMenu::on_ecs(ecs_prepare()));

    assert!(!begin_sysom_shortcut(&mut auth));
    assert_eq!(auth.phase, AuthPhase::ManagingProviders);
}

#[test]
fn first_sysom_provider_skips_the_name_field() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    auth.providers[0].fields = vec![
        field("provider_id", "Provider ID", false),
        field("access_key_id", "Access Key ID", true),
    ];

    assert!(begin_sysom_shortcut(&mut auth));
    assert_eq!(
        auth.collected_values.get("provider_id").map(String::as_str),
        Some("aliyun")
    );
    assert_eq!(auth.current_field, 1);
}

#[test]
fn every_first_provider_uses_its_own_default_name() {
    for id in [
        "aliyun",
        "dashscope",
        "coding_plan",
        "token_plan",
        "openai_compat",
    ] {
        let mut auth = slash_auth_state(&[id], SysomMenu::on_manual());
        auth.phase = AuthPhase::SelectingProvider;
        auth.providers[0].fields = vec![
            field("provider_id", "Provider ID", false),
            field("api_key", "API Key", true),
        ];
        let mut state = InlineState::default();
        state.auth.state = Some(auth);

        answer_selected_row(&mut state);

        let auth = state.auth.state.as_ref().unwrap();
        assert_eq!(
            auth.collected_values.get("provider_id").map(String::as_str),
            Some(id)
        );
        assert_eq!(
            auth.current_field_info().map(|field| field.name.as_str()),
            Some("api_key")
        );
    }
}

#[test]
fn same_type_custom_name_and_cross_type_collision_require_naming() {
    for (name, provider_type) in [("prod", "aliyun"), ("aliyun", "dashscope")] {
        let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_manual());
        auth.phase = AuthPhase::SelectingProvider;
        auth.providers[0].fields = vec![field("provider_id", "Provider ID", false)];
        auth.existing_providers = vec![ExistingProvider {
            name: name.to_string(),
            provider_type: provider_type.to_string(),
            ..saved_dashscope()
        }];
        let mut state = InlineState::default();
        state.auth.state = Some(auth);

        answer_selected_row(&mut state);

        let auth = state.auth.state.as_ref().unwrap();
        assert_eq!(auth.current_field, 0);
        assert!(!auth.collected_values.contains_key("provider_id"));
    }
}

#[test]
fn active_run_selection_preserves_the_first_credential_field() {
    for name in ["api_key", "base_url", "access_key_id"] {
        let mut auth = slash_auth_state(&["openai_compat"], SysomMenu::default());
        auth.backend = AuthBackend::ActiveRun;
        auth.phase = AuthPhase::SelectingProvider;
        auth.providers[0].fields = vec![field(name, name, true)];
        let mut state = InlineState::default();
        state.auth.state = Some(auth);

        answer_selected_row(&mut state);

        let auth = state.auth.state.as_ref().unwrap();
        assert_eq!(
            auth.current_field_info().map(|field| field.name.as_str()),
            Some(name)
        );
        assert!(!auth.collected_values.contains_key("provider_id"));
    }
}

#[test]
fn prefetched_challenge_is_applied_without_probing_ecs_again() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    auth.collected_values
        .insert("provider_id".to_string(), "sysom-trial".to_string());
    auth.collected_values
        .insert("access_key_id".to_string(), "stale-ak".to_string());

    let applied = apply_aliyun_prepare(&mut auth);

    assert!(applied);
    assert_eq!(
        auth.phase,
        AuthPhase::AliyunEcsChallenge {
            instance_id: "i-test-1".to_string(),
            console_url: "https://example.invalid/guide/cosh?instance=i-test-1".to_string(),
        }
    );
    assert_eq!(
        auth.collected_values.get("auth_source").map(String::as_str),
        Some("ecs_ram_role")
    );
    assert!(!auth.collected_values.contains_key("access_key_id"));
    assert_eq!(
        auth.collected_values.get("provider_id").map(String::as_str),
        Some("sysom-trial")
    );
}

#[test]
fn waiting_ecs_authorization_does_not_offer_a_confirmation_action() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    assert!(apply_aliyun_prepare(&mut auth));
    let mut state = InlineState::default();
    state.auth.state = Some(auth);
    assert!(matches!(
        crate::auth::capture::pending_auth_capture(&state),
        Some(crate::runtime::prelude::RawInputCapture::Question {
            option_count: 0,
            ..
        })
    ));
    let mut output = Vec::new();
    crate::auth::prompt::render_current_auth_panel(&mut state, &mut output).unwrap();
    assert!(!String::from_utf8(output)
        .unwrap()
        .contains("I have authorized"));
}

#[test]
fn unavailable_ecs_credentials_keep_the_authorization_challenge() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    auth.phase = AuthPhase::AliyunEcsChallenge {
        instance_id: "i-test-1".to_string(),
        console_url: "https://example.invalid/guide".to_string(),
    };

    restore_after_configure_failure(&mut auth, Some("credential_source_unavailable"), None);

    assert!(matches!(auth.phase, AuthPhase::AliyunEcsChallenge { .. }));
}

/// Slash-auth state on an ECS host with one saved DashScope provider.
fn ecs_state_with_saved_dashscope() -> InlineState {
    let mut auth = slash_auth_state(
        &["dashscope", "openai_compat", "aliyun"],
        SysomMenu::on_ecs(ecs_prepare()),
    );
    auth.existing_providers = vec![saved_dashscope()];
    let mut state = InlineState::default();
    state.auth.state = Some(auth);
    state
}

fn saved_dashscope() -> ExistingProvider {
    ExistingProvider {
        name: "qwen-prod".to_string(),
        provider_type: "dashscope".to_string(),
        label: "DashScope".to_string(),
        model: "qwen3.7-plus".to_string(),
        is_active: true,
        editable: true,
        source: "user".to_string(),
        base_url: None,
        api_key_mask: Some("\u{2022}\u{2022}".to_string()),
        access_key_id_mask: None,
        access_key_secret_mask: None,
        security_token_mask: None,
        auth_source: None,
    }
}

/// Answers the panel with the row currently selected, discarding the rendered output.
fn answer_selected_row(state: &mut InlineState) {
    let id = state.auth.state.as_ref().expect("auth state").id.clone();
    let mut output = Vec::new();
    assert!(
        handle_auth_answer(&adapter_without_registry(), state, &id, "", &mut output)
            .expect("panel answer"),
        "answer was not routed to the auth panel"
    );
}

#[test]
fn menu_rows_below_the_shortcut_still_open_their_provider_action_menu() {
    let mut state = ecs_state_with_saved_dashscope();
    // Row 2 of [SysOM, qwen-prod, + Add new provider].
    state
        .auth
        .state
        .as_mut()
        .expect("auth state")
        .selected_provider = 1;

    answer_selected_row(&mut state);

    let auth = state.auth.state.as_ref().expect("auth state");
    assert_eq!(auth.phase, AuthPhase::ProviderAction { provider_idx: 0 });
}

#[test]
fn the_last_menu_row_still_adds_a_new_provider() {
    let mut state = ecs_state_with_saved_dashscope();
    state
        .auth
        .state
        .as_mut()
        .expect("auth state")
        .selected_provider = 2;

    answer_selected_row(&mut state);

    let auth = state.auth.state.as_ref().expect("auth state");
    assert_eq!(auth.phase, AuthPhase::SelectingProvider);
    assert_eq!(auth.selected_provider, 0);
}

#[test]
fn the_first_menu_row_starts_the_sysom_shortcut() {
    let mut state = ecs_state_with_saved_dashscope();

    answer_selected_row(&mut state);

    let auth = state.auth.state.as_ref().expect("auth state");
    assert!(matches!(auth.phase, AuthPhase::AliyunEcsChallenge { .. }));
    assert_eq!(auth.current_provider().id, "aliyun");
    assert_eq!(
        auth.collected_values.get("provider_id").map(String::as_str),
        Some("aliyun")
    );
}

#[test]
fn cancelling_a_provider_action_returns_to_the_row_it_came_from() {
    let mut state = ecs_state_with_saved_dashscope();
    {
        let auth = state.auth.state.as_mut().expect("auth state");
        auth.phase = AuthPhase::ProviderAction { provider_idx: 0 };
        // An active provider offers Edit, Delete, Cancel.
        auth.selected_provider = 2;
    }

    answer_selected_row(&mut state);

    let auth = state.auth.state.as_ref().expect("auth state");
    assert_eq!(auth.phase, AuthPhase::ManagingProviders);
    // Row 2, not row 1: the SysOM shortcut occupies the first slot.
    assert_eq!(auth.selected_provider, 1);
}

#[test]
fn deleting_the_promoted_provider_restores_the_shortcut_row() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    auth.existing_providers = vec![ExistingProvider {
        name: "sysom-trial".to_string(),
        provider_type: "aliyun".to_string(),
        auth_source: Some("ecs_ram_role".to_string()),
        ..saved_dashscope()
    }];
    auth.sysom.sync(&mut auth.existing_providers);
    assert!(auth.sysom.promoted());

    // What `submit_delete_confirmation` does after reloading the saved providers.
    auth.existing_providers = vec![saved_dashscope()];
    auth.sysom.sync(&mut auth.existing_providers);

    assert!(!auth.sysom.promoted());
    assert_eq!(
        management_entry(&auth.sysom, auth.existing_providers.len(), 0),
        AuthManagementEntry::SysomShortcut
    );
}

#[test]
fn without_a_prefetched_challenge_prepare_is_queued_without_registry_io() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::default());
    assert!(apply_aliyun_prepare(&mut auth));
    assert_eq!(auth.phase, AuthPhase::AliyunEcsPreparing);
}

#[test]
fn first_ecs_check_does_not_display_the_authorization_link() {
    let mut auth = slash_auth_state(&["aliyun"], SysomMenu::on_ecs(ecs_prepare()));
    assert!(apply_aliyun_prepare(&mut auth));
    let mut state = InlineState::default();
    state.auth.state = Some(auth);
    let mut output = Vec::new();
    crate::auth::prompt::render_current_auth_panel(&mut state, &mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();
    assert!(
        !rendered.contains("https://example.invalid/guide"),
        "{rendered}"
    );
    assert!(rendered.contains("Checking ECS RAM Role"), "{rendered}");
}

#[test]
fn core_registry_aliyun_add_waits_for_provider_id_before_prepare() {
    assert!(!should_apply_aliyun_prepare_on_provider_selection(
        AuthBackend::CoreRegistry
    ));
    assert!(should_apply_aliyun_prepare_after_field(
        AuthBackend::CoreRegistry,
        false,
        "aliyun",
        Some("provider_id"),
    ));
}

#[test]
fn active_run_aliyun_selection_can_prepare_without_provider_id_field() {
    assert!(should_apply_aliyun_prepare_on_provider_selection(
        AuthBackend::ActiveRun
    ));
    assert!(!should_apply_aliyun_prepare_after_field(
        AuthBackend::ActiveRun,
        false,
        "aliyun",
        Some("provider_id"),
    ));
}

#[test]
fn provider_change_discards_the_previous_observed_model() {
    let mut state = InlineState::default();
    state.personalization.foreground_model = Some("previous-provider-model".to_string());

    clear_observed_model_after_provider_change(&mut state);

    assert_eq!(state.personalization.foreground_model, None);
}

#[test]
fn deleting_the_active_provider_with_a_fallback_discards_the_observed_model() {
    let mut state = InlineState::default();
    state.personalization.foreground_model = Some("deleted-provider-model".to_string());
    let outcome = DeleteConfirmationOutcome::Deleted {
        provider_name: "deleted-provider".to_string(),
        needs_reselection: false,
    };

    clear_observed_model_after_provider_delete(&mut state, true, &outcome);

    assert_eq!(state.personalization.foreground_model, None);
}

#[test]
fn deleting_the_active_provider_without_a_fallback_discards_the_observed_model() {
    let mut state = InlineState::default();
    state.personalization.foreground_model = Some("deleted-provider-model".to_string());
    let outcome = DeleteConfirmationOutcome::Deleted {
        provider_name: "deleted-provider".to_string(),
        needs_reselection: true,
    };

    clear_observed_model_after_provider_delete(&mut state, true, &outcome);

    assert_eq!(state.personalization.foreground_model, None);
}

#[test]
fn cancelling_or_deleting_an_inactive_provider_keeps_the_observed_model() {
    let mut state = InlineState::default();
    state.personalization.foreground_model = Some("active-provider-model".to_string());

    clear_observed_model_after_provider_delete(
        &mut state,
        true,
        &DeleteConfirmationOutcome::Cancelled,
    );
    assert_eq!(
        state.personalization.foreground_model.as_deref(),
        Some("active-provider-model")
    );

    clear_observed_model_after_provider_delete(
        &mut state,
        false,
        &DeleteConfirmationOutcome::Deleted {
            provider_name: "inactive-provider".to_string(),
            needs_reselection: false,
        },
    );
    assert_eq!(
        state.personalization.foreground_model.as_deref(),
        Some("active-provider-model")
    );
}

#[test]
fn manual_aliyun_edit_does_not_apply_ecs_prepare() {
    let manual = ExistingProvider {
        name: "aliyun-manual".to_string(),
        provider_type: "aliyun".to_string(),
        label: "Aliyun Authentication".to_string(),
        model: "qwen3.7-plus".to_string(),
        is_active: true,
        editable: true,
        source: "user".to_string(),
        base_url: None,
        api_key_mask: None,
        access_key_id_mask: Some("••••".to_string()),
        access_key_secret_mask: Some("••••••".to_string()),
        security_token_mask: None,
        auth_source: None,
    };
    let ecs = ExistingProvider {
        auth_source: Some("ecs_ram_role".to_string()),
        access_key_id_mask: None,
        access_key_secret_mask: None,
        ..manual.clone()
    };

    assert!(!should_apply_aliyun_prepare_for_edit(&manual));
    assert!(should_apply_aliyun_prepare_for_edit(&ecs));
}

#[test]
fn ecs_aliyun_manual_fallback_clears_auth_source() {
    let ecs = ExistingProvider {
        name: "aliyun-ecs".to_string(),
        provider_type: "aliyun".to_string(),
        label: "Aliyun Authentication".to_string(),
        model: "qwen3.7-plus".to_string(),
        is_active: true,
        editable: true,
        source: "user".to_string(),
        base_url: None,
        api_key_mask: None,
        access_key_id_mask: None,
        access_key_secret_mask: None,
        security_token_mask: None,
        auth_source: Some("ecs_ram_role".to_string()),
    };
    let manual = ExistingProvider {
        auth_source: None,
        ..ecs.clone()
    };
    let mut ecs_values = HashMap::from([
        ("auth_source".to_string(), "ecs_ram_role".to_string()),
        ("access_key_id".to_string(), "manual-ak".to_string()),
        ("access_key_secret".to_string(), "manual-sk".to_string()),
        ("security_token".to_string(), "manual-token".to_string()),
    ]);
    let mut manual_values = ecs_values.clone();

    clear_ecs_auth_source_for_manual_aliyun_edit(&ecs, &mut ecs_values);
    clear_ecs_auth_source_for_manual_aliyun_edit(&manual, &mut manual_values);

    assert!(!ecs_values.contains_key("auth_source"));
    assert_eq!(
        ecs_values.get("access_key_id").map(String::as_str),
        Some("manual-ak")
    );
    assert_eq!(
        ecs_values.get("access_key_secret").map(String::as_str),
        Some("manual-sk")
    );
    assert_eq!(
        ecs_values.get("security_token").map(String::as_str),
        Some("manual-token")
    );
    assert_eq!(
        manual_values.get("auth_source").map(String::as_str),
        Some("ecs_ram_role")
    );
}

fn field(name: &str, label: &str, secret: bool) -> AuthFieldInfo {
    AuthFieldInfo {
        name: name.to_string(),
        label: label.to_string(),
        hint: None,
        secret,
        required: true,
        placeholder: None,
    }
}

/// An `/auth` edit already sitting on Base URL with the saved value pre-filled — the state a
/// user sees right before pressing Enter to keep it.
fn editing_base_url_state() -> InlineState {
    let mut template = template("openai_compat");
    template.fields = vec![
        field("provider_id", "Provider ID", false),
        field("base_url", "Base URL", false),
        field("model", "Model", false),
        field("api_key", "API Key", true),
    ];
    let mut auth = slash_auth_state(&[], SysomMenu::default());
    auth.providers = vec![template];
    auth.phase = AuthPhase::FillingField;
    auth.editing_provider_name = Some("qwen-prod".to_string());
    auth.collected_values = HashMap::from([
        ("provider_id".to_string(), "qwen-prod".to_string()),
        (
            "base_url".to_string(),
            "https://example.invalid/v1".to_string(),
        ),
        ("model".to_string(), "qwen3.7-plus".to_string()),
        (
            "api_key".to_string(),
            "\u{2022}\u{2022}\u{2022}".to_string(),
        ),
    ]);
    auth.current_field = 1;
    auth.field_input = "https://example.invalid/v1".to_string();
    let mut state = InlineState::default();
    state.auth.state = Some(auth);
    state
}

/// The event an empty Enter produces on a non-secret capture: the scoped capture id, not the
/// typed text, rides in `input`.
fn empty_submission(capture_id: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("session", capture_id);
    event.component = Some("card".to_string());
    event.message = Some("question_submit_empty".to_string());
    event.input = Some(capture_id.to_string());
    event
}

fn relay_card_event(state: &mut InlineState, event: ShellEvent) -> String {
    let mut output = Vec::new();
    render_auth_card_actions(&[event], &adapter_without_registry(), state, &mut output, 0)
        .expect("relay card event");
    String::from_utf8(output).expect("utf8 panel output")
}

/// #1833: Enter on a pre-filled non-secret field kept the panel frozen, because the empty
/// submission never reached the auth dispatcher.
#[test]
fn empty_enter_keeps_the_prefilled_value_and_advances_the_field() {
    let mut state = editing_base_url_state();
    let capture_id = auth_capture_id(state.auth.state.as_ref().expect("auth state"));

    relay_card_event(&mut state, empty_submission(&capture_id));

    let auth = state.auth.state.as_ref().expect("auth state");
    assert_eq!(
        auth.collected_values.get("base_url").map(String::as_str),
        Some("https://example.invalid/v1")
    );
    assert_eq!(auth.current_field, 2);
    // The cursor moved to Model, so its saved value becomes the editable projection.
    assert_eq!(auth.field_input, "qwen3.7-plus");
}

/// The scoped capture id is the guard: an empty submission left over from the field the user
/// already passed must not push the live field forward a second time.
#[test]
fn a_stale_empty_submission_does_not_advance_the_live_field() {
    let mut state = editing_base_url_state();
    let stale_id = {
        let auth = state.auth.state.as_mut().expect("auth state");
        auth.current_field = 0;
        let stale = auth_capture_id(auth);
        auth.current_field = 1;
        stale
    };

    relay_card_event(&mut state, empty_submission(&stale_id));

    let auth = state.auth.state.as_ref().expect("auth state");
    assert_eq!(auth.current_field, 1);
    assert_eq!(auth.field_input, "https://example.invalid/v1");
}

/// The event ESC produces on a question capture: the scoped capture id rides in `input`.
fn cancel_event(capture_id: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("session", capture_id);
    event.component = Some("card".to_string());
    event.message = Some("question_cancel".to_string());
    event.input = Some(capture_id.to_string());
    event
}

/// A brand-new `openai_compat` provider mid-form: Provider ID and Base URL are confirmed and the
/// cursor sits on Model.
fn new_provider_on_model_state() -> InlineState {
    let mut template = template("openai_compat");
    template.fields = vec![
        field("provider_id", "Provider ID", false),
        field("base_url", "Base URL", false),
        field("model", "Model", false),
    ];
    let mut auth = slash_auth_state(&[], SysomMenu::default());
    auth.providers = vec![template];
    auth.phase = AuthPhase::FillingField;
    auth.collected_values = HashMap::from([
        ("provider_id".to_string(), "qwen-prod".to_string()),
        (
            "base_url".to_string(),
            "https://example.invalid/v1".to_string(),
        ),
    ]);
    auth.current_field = 2;
    auth.load_current_field_input();
    let mut state = InlineState::default();
    state.auth.state = Some(auth);
    state
}

fn press_esc(state: &mut InlineState) -> String {
    let capture_id = auth_capture_id(state.auth.state.as_ref().expect("auth state"));
    relay_card_event(state, cancel_event(&capture_id))
}

fn press_ctrl_c(state: &mut InlineState) -> String {
    let capture_id = auth_capture_id(state.auth.state.as_ref().expect("auth state"));
    let mut event = cancel_event(&capture_id);
    event.message = Some("question_abort".to_string());
    relay_card_event(state, event)
}

/// Making ESC step back must not cost the user their interrupt: Ctrl+C arrives as a separate
/// `question_abort` and still abandons the whole form from any field.
#[test]
fn ctrl_c_mid_form_abandons_the_whole_flow() {
    let mut state = new_provider_on_model_state();
    let auth_id = state.auth.state.as_ref().expect("auth state").id.clone();

    let rendered = press_ctrl_c(&mut state);

    assert!(
        state.auth.state.is_none(),
        "Ctrl+C stepped back instead of abandoning the form"
    );
    assert!(rendered.contains("Auth cancelled"), "{rendered}");
    assert!(state.auth.completed_ids.contains(&auth_id));
}

/// #1760: ESC in the middle of the form abandoned every collected field. It now redraws the
/// previous prompt, and the flow stays pending.
#[test]
fn esc_on_a_middle_field_redraws_the_earlier_field_instead_of_cancelling() {
    let mut state = new_provider_on_model_state();

    let rendered = press_esc(&mut state);

    let auth = state.auth.state.as_ref().expect("auth flow still pending");
    assert_eq!(auth.current_field, 1);
    assert_eq!(auth.field_input, "https://example.invalid/v1");
    assert!(rendered.contains("Base URL"), "{rendered}");
    assert!(!rendered.contains("Auth cancelled"), "{rendered}");
    assert!(
        state.auth.completed_ids.is_empty(),
        "a step back must not mark the request answered"
    );
}

/// The first field steps out to the template picker, which is still a live panel — so the flow
/// is not finished and the id must stay open for the answer that follows.
#[test]
fn esc_on_the_first_field_returns_to_the_picker_without_cancelling() {
    let mut state = new_provider_on_model_state();
    {
        let auth = state.auth.state.as_mut().expect("auth state");
        auth.current_field = 0;
        auth.load_current_field_input();
    }

    let rendered = press_esc(&mut state);

    let auth = state.auth.state.as_ref().expect("auth flow still pending");
    assert_eq!(auth.phase, AuthPhase::SelectingProvider);
    assert!(!rendered.contains("Auth cancelled"), "{rendered}");
    assert!(state.auth.completed_ids.is_empty());
}

/// Only the ESC that finds no earlier step ends the flow, and that one still reports the
/// cancellation and closes the request out.
#[test]
fn a_second_esc_at_the_picker_ends_the_flow() {
    let mut state = new_provider_on_model_state();
    let auth_id = {
        let auth = state.auth.state.as_mut().expect("auth state");
        auth.phase = AuthPhase::SelectingProvider;
        auth.id.clone()
    };

    let rendered = press_esc(&mut state);

    assert!(state.auth.state.is_none(), "flow should be over");
    assert!(rendered.contains("Auth cancelled"), "{rendered}");
    assert!(state.auth.completed_ids.contains(&auth_id));
}

/// The scoped capture id guards the step back exactly as it guards a submission: an ESC left
/// over from a field the user already passed must not walk the live field backwards.
#[test]
fn a_stale_field_esc_does_not_move_the_live_field() {
    let mut state = new_provider_on_model_state();
    let stale_id = {
        let auth = state.auth.state.as_mut().expect("auth state");
        auth.current_field = 0;
        let stale = auth_capture_id(auth);
        auth.current_field = 2;
        stale
    };

    relay_card_event(&mut state, cancel_event(&stale_id));

    let auth = state.auth.state.as_ref().expect("auth flow still pending");
    assert_eq!(auth.current_field, 2);
    assert!(state.auth.completed_ids.is_empty());
}

/// Secret fields already worked: an empty Enter there is delivered as an empty `answer`.
#[test]
fn empty_enter_on_a_secret_field_still_keeps_the_masked_value() {
    let mut state = editing_base_url_state();
    {
        let auth = state.auth.state.as_mut().expect("auth state");
        auth.current_field = 3;
        auth.load_current_field_input();
        assert_eq!(auth.field_input, "\u{2022}\u{2022}\u{2022}");
    }
    let capture_id = auth_capture_id(state.auth.state.as_ref().expect("auth state"));
    let mut event = empty_submission(&capture_id);
    event.component = Some("card_secret".to_string());
    event.message = Some("answer".to_string());
    event.input = Some(String::new());

    relay_card_event(&mut state, event);

    // All fields collected: the submission is attempted and the fake adapter rejects it, so
    // the edit is restored instead of completed — with the mask intact for another try.
    let auth = state.auth.state.as_ref().expect("auth state");
    assert_eq!(
        auth.collected_values.get("api_key").map(String::as_str),
        Some("\u{2022}\u{2022}\u{2022}")
    );
}
