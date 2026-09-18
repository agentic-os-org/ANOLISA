//! Input-capture identities and routing for the multi-step auth flow.

use crate::runtime::prelude::RawInputCapture;
use crate::runtime::state::InlineState;

use super::delete_confirm::DELETE_CONFIRM_OPTION_COUNT;
use super::menu::management_entry_count;
use super::provider_management::{provider_action_options, ExistingProvider};
use super::runtime::{AuthPhase, RuntimeAuthState};

pub(super) fn auth_capture_id(auth: &RuntimeAuthState) -> String {
    let scope = match &auth.phase {
        AuthPhase::ManagingProviders => "manage".to_string(),
        AuthPhase::ProviderAction { provider_idx } => format!("action-{provider_idx}"),
        AuthPhase::ConfirmDelete { provider_idx } => format!("delete-{provider_idx}"),
        AuthPhase::SelectingProvider => "select".to_string(),
        AuthPhase::FillingField => format!(
            "field-{}-{}",
            auth.current_field, auth.field_capture_revision
        ),
        AuthPhase::PreparingMenu
        | AuthPhase::AliyunEcsPreparing
        | AuthPhase::AliyunEcsChallenge { .. } => {
            format!("aliyun-challenge-{}", auth.field_capture_revision)
        }
    };
    format!("{}@{scope}", auth.id)
}

pub(super) fn matches_auth_capture(auth: &RuntimeAuthState, capture_id: &str) -> bool {
    capture_id == auth.id || capture_id == auth_capture_id(auth)
}

pub(crate) fn pending_auth_capture(state: &InlineState) -> Option<RawInputCapture> {
    let auth = state.auth.state.as_ref()?;
    match &auth.phase {
        AuthPhase::ManagingProviders => Some(RawInputCapture::Question {
            id: auth_capture_id(auth),
            option_count: management_entry_count(&auth.sysom, auth.existing_providers.len()),
            selected: auth.selected_provider,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
        AuthPhase::ProviderAction { provider_idx } => {
            let existing = auth.existing_providers.get(*provider_idx);
            let option_count = provider_action_options(
                existing.is_some_and(|provider| provider.is_active),
                existing.map(|provider| provider.editable).unwrap_or(true),
                existing.is_some_and(ExistingProvider::deletable),
            )
            .len();
            Some(RawInputCapture::Question {
                id: auth_capture_id(auth),
                option_count,
                selected: auth.selected_provider,
                allow_free_text: false,
                multiple: false,
                secret: false,
            })
        }
        AuthPhase::ConfirmDelete { .. } => Some(RawInputCapture::Question {
            id: auth_capture_id(auth),
            option_count: DELETE_CONFIRM_OPTION_COUNT,
            selected: auth.selected_provider,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
        AuthPhase::SelectingProvider => Some(RawInputCapture::Question {
            id: auth_capture_id(auth),
            option_count: auth.providers.len(),
            selected: auth.selected_provider,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
        AuthPhase::FillingField => {
            let secret = auth
                .providers
                .get(auth.selected_provider)
                .and_then(|provider| provider.fields.get(auth.current_field))
                .is_some_and(|field| field.secret);
            Some(RawInputCapture::TextQuestion {
                id: auth_capture_id(auth),
                initial_text: auth.field_input.clone(),
                secret,
            })
        }
        AuthPhase::PreparingMenu
        | AuthPhase::AliyunEcsPreparing
        | AuthPhase::AliyunEcsChallenge { .. } => Some(RawInputCapture::Question {
            id: auth_capture_id(auth),
            option_count: super::ecs_poll::option_count(state),
            selected: 0,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
    }
}
