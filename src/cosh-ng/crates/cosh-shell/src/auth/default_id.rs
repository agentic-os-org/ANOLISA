//! Provider identity and editable-field policy for auth forms.

use crate::runtime::prelude::{AuthFieldInfo, AuthProviderInfo};

use super::runtime::{AuthBackend, AuthPhase, RuntimeAuthState};
use super::validation::{PROVIDER_ID_HINT, PROVIDER_ID_OCCUPIED_ERROR};

pub(super) fn providers_with_provider_id_field(
    providers: Vec<AuthProviderInfo>,
) -> Vec<AuthProviderInfo> {
    providers
        .into_iter()
        .map(|mut provider| {
            provider.fields.insert(
                0,
                AuthFieldInfo {
                    name: "provider_id".to_string(),
                    label: "Provider ID".to_string(),
                    hint: Some(PROVIDER_ID_HINT.to_string()),
                    secret: false,
                    required: true,
                    placeholder: Some(provider.id.clone()),
                },
            );
            provider
        })
        .collect()
}

fn reset_new_provider(auth: &mut RuntimeAuthState) {
    auth.editing_provider_name = None;
    auth.default_provider_id = false;
    auth.from_sysom_shortcut = false;
    auth.current_field = 0;
    auth.collected_values.clear();
    auth.field_input.clear();
    auth.field_error = None;
}

pub(super) fn begin_new_provider(auth: &mut RuntimeAuthState) {
    reset_new_provider(auth);
    auth.selected_provider = 0;
    auth.phase = AuthPhase::SelectingProvider;
}

pub(super) fn begin_provider_fields(auth: &mut RuntimeAuthState) {
    reset_new_provider(auth);
    auth.phase = AuthPhase::FillingField;
    if auth.backend == AuthBackend::CoreRegistry {
        let id = auth.current_provider().id.clone();
        let occupied = auth.provider_name_is_taken(&id);
        if !occupied
            && !auth
                .existing_providers
                .iter()
                .any(|provider| provider.provider_type == id)
        {
            auth.default_provider_id = true;
            auth.collected_values.insert("provider_id".to_string(), id);
        } else if occupied {
            auth.field_error = Some(PROVIDER_ID_OCCUPIED_ERROR.to_string());
            for field in &mut auth.providers[auth.selected_provider].fields {
                if field.name == "provider_id" {
                    field.placeholder = None;
                }
            }
        }
    }
    auth.current_field = auth.first_editable_field();
    auth.load_current_field_input();
}

pub(super) fn begin_sysom_shortcut(auth: &mut RuntimeAuthState) -> bool {
    let Some(template_idx) = auth
        .providers
        .iter()
        .position(|provider| provider.id == "aliyun")
    else {
        return false;
    };
    auth.selected_provider = template_idx;
    begin_provider_fields(auth);
    auth.from_sysom_shortcut = true;
    true
}

impl RuntimeAuthState {
    pub(super) fn provider_name_is_taken(&self, name: &str) -> bool {
        self.existing_providers
            .iter()
            .any(|provider| provider.name == name)
    }

    pub(super) fn field_is_editable(&self, index: usize) -> bool {
        self.current_provider()
            .fields
            .get(index)
            .is_some_and(|field| {
                field.name != "provider_id"
                    || (!self.default_provider_id && self.editing_provider_name.is_none())
            })
    }

    pub(super) fn editable_field_at_or_after(&self, start: usize) -> usize {
        (start..self.current_provider().fields.len())
            .find(|&index| self.field_is_editable(index))
            .unwrap_or(self.current_provider().fields.len())
    }

    pub(super) fn first_editable_field(&self) -> usize {
        self.editable_field_at_or_after(0)
    }
}
