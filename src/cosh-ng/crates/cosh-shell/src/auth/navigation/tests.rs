//! Unit tests for ESC back-navigation through the `/auth` form.

use super::{step_back, BackOutcome};
use crate::auth::menu::SysomMenu;
use crate::auth::provider_management::ExistingProvider;
use crate::auth::runtime::{AuthBackend, AuthPhase, RuntimeAuthState};
use crate::runtime::prelude::{AuthFieldInfo, AuthProviderInfo};

fn field(name: &str, secret: bool) -> AuthFieldInfo {
    AuthFieldInfo {
        name: name.to_string(),
        label: name.to_string(),
        hint: None,
        secret,
        required: true,
        placeholder: None,
    }
}

/// The `openai_compat` template as slash auth presents it: Provider ID injected in front.
fn openai_compat_template() -> AuthProviderInfo {
    AuthProviderInfo {
        id: "openai_compat".to_string(),
        label: "OpenAI Compatible".to_string(),
        description: None,
        description_zh_cn: None,
        builtin_base_url: None,
        fields: vec![
            field("provider_id", false),
            field("base_url", false),
            field("model", false),
            field("api_key", true),
        ],
    }
}

fn filling_state(current_field: usize, collected: &[(&str, &str)]) -> RuntimeAuthState {
    let mut auth = RuntimeAuthState {
        id: "auth-slash".to_string(),
        request_id: "slash".to_string(),
        phase: AuthPhase::FillingField,
        providers: vec![openai_compat_template()],
        selected_provider: 0,
        current_field,
        collected_values: collected
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        field_input: String::new(),
        field_error: None,
        field_capture_revision: 0,
        existing_providers: Vec::new(),
        editing_provider_name: None,
        default_provider_id: false,
        from_sysom_shortcut: false,
        error_message: None,
        backend: AuthBackend::CoreRegistry,
        sysom: SysomMenu::default(),
    };
    auth.load_current_field_input();
    auth
}

fn saved_provider(name: &str) -> ExistingProvider {
    ExistingProvider {
        name: name.to_string(),
        provider_type: "openai_compat".to_string(),
        label: "OpenAI Compatible".to_string(),
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

/// The whole point of #1760: one field of backtracking must not cost the fields before it.
#[test]
fn esc_on_a_middle_field_steps_back_one_and_keeps_submitted_values() {
    let mut auth = filling_state(
        2,
        &[
            ("provider_id", "qwen-prod"),
            ("base_url", "https://example.invalid/v1"),
        ],
    );

    assert_eq!(step_back(&mut auth), BackOutcome::Redraw);

    assert_eq!(auth.phase, AuthPhase::FillingField);
    assert_eq!(auth.current_field, 1);
    assert_eq!(
        auth.collected_values.get("provider_id").map(String::as_str),
        Some("qwen-prod")
    );
    assert_eq!(
        auth.collected_values.get("base_url").map(String::as_str),
        Some("https://example.invalid/v1")
    );
}

/// Landing on the earlier field has to re-project its value, or the panel shows the input of a
/// field the cursor no longer sits on.
#[test]
fn stepping_back_reloads_the_earlier_value_and_clears_the_validation_error() {
    let mut auth = filling_state(
        2,
        &[
            ("provider_id", "qwen-prod"),
            ("base_url", "https://example.invalid/v1"),
        ],
    );
    auth.field_error = Some("Model cannot be empty.".to_string());

    step_back(&mut auth);

    assert_eq!(
        auth.current_field_info().map(|field| field.name.as_str()),
        Some("base_url")
    );
    assert_eq!(auth.field_input, "https://example.invalid/v1");
    // The error described the field being left; carrying it back would blame the wrong prompt.
    assert!(auth.field_error.is_none());
}

/// A draft the user never submitted is not part of the form, and a secret draft least of all:
/// stepping back must not park a plaintext credential in the panel or in `collected_values`.
#[test]
fn an_unsubmitted_draft_does_not_survive_the_step_back() {
    let mut auth = filling_state(
        3,
        &[
            ("provider_id", "qwen-prod"),
            ("base_url", "https://example.invalid/v1"),
            ("model", "qwen3.7-plus"),
        ],
    );
    assert!(auth.current_field_info().is_some_and(|field| field.secret));
    auth.field_input = "sk-typed-but-never-submitted".to_string();

    step_back(&mut auth);

    assert_eq!(auth.field_input, "qwen3.7-plus");
    assert!(
        !auth
            .collected_values
            .values()
            .any(|value| value == "sk-typed-but-never-submitted"),
        "unsubmitted secret leaked into the form: {:?}",
        auth.collected_values
    );
}

/// The first field of a new provider is reached from the template picker, so that is where ESC
/// goes — cancelling only once the user is back at a menu.
#[test]
fn esc_on_the_first_field_of_a_new_provider_returns_to_the_template_picker() {
    let mut auth = filling_state(0, &[]);

    assert_eq!(step_back(&mut auth), BackOutcome::Redraw);

    assert_eq!(auth.phase, AuthPhase::SelectingProvider);
    // The template stays selected so the picker reopens on the row the user came from.
    assert_eq!(auth.selected_provider, 0);
}

/// ESC at the template picker is the end of the road; the caller cancels from here.
#[test]
fn esc_at_the_template_picker_reports_cancel() {
    let mut auth = filling_state(0, &[]);
    auth.phase = AuthPhase::SelectingProvider;

    assert_eq!(step_back(&mut auth), BackOutcome::Cancel);
}

/// An edit may not step back onto Provider ID: `send_auth_response` takes the identity from
/// `editing_provider_name`, so anything typed there is silently discarded.
#[test]
fn esc_on_the_first_editable_field_of_an_edit_returns_to_its_action_menu() {
    let mut auth = filling_state(1, &[("provider_id", "qwen-prod")]);
    auth.editing_provider_name = Some("qwen-prod".to_string());
    auth.existing_providers = vec![saved_provider("qwen-prod")];

    assert_eq!(step_back(&mut auth), BackOutcome::Redraw);

    assert_eq!(auth.phase, AuthPhase::ProviderAction { provider_idx: 0 });
    // `selected_provider` becomes the action row: an active provider offers no Activate, so
    // [Edit configuration, Delete provider, Cancel] puts Edit at row 0.
    assert_eq!(auth.selected_provider, 0);
}

/// The action menu is addressed by index, so an edit whose provider is not in the list has
/// nowhere to return to and must cancel instead of pointing at an unrelated row.
#[test]
fn an_edit_without_a_matching_saved_provider_reports_cancel() {
    let mut auth = filling_state(1, &[("provider_id", "qwen-prod")]);
    auth.editing_provider_name = Some("qwen-prod".to_string());
    auth.existing_providers = vec![saved_provider("other-provider")];
    let before = auth.clone();

    assert_eq!(step_back(&mut auth), BackOutcome::Cancel);

    // A cancelling outcome leaves the state alone, so the caller cancels the flow the user
    // actually pressed ESC on.
    assert_eq!(auth.phase, before.phase);
    assert_eq!(auth.current_field, before.current_field);
    assert_eq!(auth.collected_values, before.collected_values);
}

/// Leaving the form for a menu has no `load_current_field_input` to overwrite the draft, so the
/// abandoned field must be cleared explicitly — a secret typed into the first editable field of
/// an edit would otherwise stay in memory across the action, delete and management panels.
#[test]
fn leaving_an_edit_from_a_secret_field_does_not_keep_the_draft() {
    let mut auth = filling_state(1, &[("provider_id", "qwen-prod")]);
    auth.providers[0].fields = vec![field("provider_id", false), field("api_key", true)];
    auth.editing_provider_name = Some("qwen-prod".to_string());
    auth.existing_providers = vec![saved_provider("qwen-prod")];
    assert!(auth.current_field_info().is_some_and(|field| field.secret));
    auth.field_input = "sk-typed-but-never-submitted".to_string();

    assert_eq!(step_back(&mut auth), BackOutcome::Redraw);

    assert!(
        auth.field_input.is_empty(),
        "secret draft survived the exit to the action menu: {:?}",
        auth.field_input
    );
    assert!(
        !auth
            .collected_values
            .values()
            .any(|value| value == "sk-typed-but-never-submitted"),
        "{:?}",
        auth.collected_values
    );
}

/// The same applies to a new provider: an `AuthRequired` template has no Provider ID injected in
/// front, so field 0 can itself be the secret.
#[test]
fn leaving_a_new_provider_from_a_secret_field_does_not_keep_the_draft() {
    let mut auth = filling_state(0, &[]);
    auth.providers[0].fields = vec![field("api_key", true)];
    auth.field_input = "sk-typed-but-never-submitted".to_string();

    assert_eq!(step_back(&mut auth), BackOutcome::Redraw);

    assert_eq!(auth.phase, AuthPhase::SelectingProvider);
    assert!(
        auth.field_input.is_empty(),
        "secret draft survived the exit to the picker: {:?}",
        auth.field_input
    );
}

/// An inactive provider's action menu starts with "Set as active provider", so returning to row 0
/// would make the next Enter switch providers instead of resuming the edit.
#[test]
fn returning_from_an_inactive_providers_edit_lands_on_the_edit_row() {
    let mut auth = filling_state(1, &[("provider_id", "qwen-standby")]);
    auth.editing_provider_name = Some("qwen-standby".to_string());
    auth.existing_providers = vec![ExistingProvider {
        is_active: false,
        ..saved_provider("qwen-standby")
    }];

    assert_eq!(step_back(&mut auth), BackOutcome::Redraw);

    assert_eq!(auth.phase, AuthPhase::ProviderAction { provider_idx: 0 });
    // [Set as active provider, Edit configuration, Delete provider, Cancel].
    assert_eq!(auth.selected_provider, 1);
}

/// Phases outside the form keep the cancel they already had, so this fix cannot change how the
/// ECS challenge or the management menu respond to ESC.
#[test]
fn phases_outside_the_form_still_cancel() {
    for phase in [
        AuthPhase::ManagingProviders,
        AuthPhase::ProviderAction { provider_idx: 0 },
        AuthPhase::ConfirmDelete { provider_idx: 0 },
        AuthPhase::AliyunEcsChallenge {
            instance_id: "i-test-1".to_string(),
            console_url: "https://example.invalid/guide".to_string(),
        },
    ] {
        let mut auth = filling_state(0, &[]);
        auth.phase = phase.clone();

        assert_eq!(step_back(&mut auth), BackOutcome::Cancel, "phase={phase:?}");
        assert_eq!(auth.phase, phase, "step_back mutated a cancelling phase");
    }
}

/// Repeated ESC has to walk the form back field by field and land on the picker exactly once,
/// which is the sequence #1760 asks for: Model -> API Key ... -> Provider ID -> picker.
#[test]
fn consecutive_esc_walks_the_form_back_to_the_picker() {
    let mut auth = filling_state(
        3,
        &[
            ("provider_id", "qwen-prod"),
            ("base_url", "https://example.invalid/v1"),
            ("model", "qwen3.7-plus"),
        ],
    );

    let mut visited = Vec::new();
    for _ in 0..4 {
        assert_eq!(step_back(&mut auth), BackOutcome::Redraw);
        visited.push(match auth.phase {
            AuthPhase::FillingField => auth
                .current_field_info()
                .map(|field| field.name.clone())
                .unwrap_or_default(),
            AuthPhase::SelectingProvider => "picker".to_string(),
            ref other => panic!("unexpected phase: {other:?}"),
        });
    }

    assert_eq!(visited, ["model", "base_url", "provider_id", "picker"]);
    assert_eq!(step_back(&mut auth), BackOutcome::Cancel);
    // Nothing was thrown away on the way out; re-answering the picker is what resets the form.
    assert_eq!(auth.collected_values.len(), 3);
}

#[test]
fn editing_without_a_name_field_keeps_the_first_credential_editable() {
    let mut auth = filling_state(1, &[]);
    auth.providers[0].fields.remove(0);
    auth.editing_provider_name = Some("qwen-prod".into());
    auth.existing_providers = vec![saved_provider("qwen-prod")];

    step_back(&mut auth);

    assert_eq!(auth.phase, AuthPhase::FillingField);
    assert_eq!(auth.current_field_info().unwrap().name, "base_url");
    step_back(&mut auth);
    assert_eq!(auth.phase, AuthPhase::ProviderAction { provider_idx: 0 });
}

#[test]
fn editing_skips_identity_by_field_name_not_position() {
    let mut auth = filling_state(2, &[]);
    auth.providers[0].fields.swap(0, 1);
    auth.editing_provider_name = Some("qwen-prod".into());

    step_back(&mut auth);

    assert_eq!(auth.phase, AuthPhase::FillingField);
    assert_eq!(auth.current_field_info().unwrap().name, "base_url");
}
