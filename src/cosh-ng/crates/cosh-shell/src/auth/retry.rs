//! State restoration after an auth configuration submission fails.

use std::collections::HashSet;

use super::menu::ECS_RAM_ROLE_AUTH_SOURCE;
use super::runtime::{AuthPhase, RuntimeAuthState};

#[cfg(test)]
pub(super) fn restore_after_failed_submission(auth: &mut RuntimeAuthState) {
    restore_after_failed_submission_at(auth, None);
}

pub(super) fn restore_after_failed_submission_at(
    auth: &mut RuntimeAuthState,
    field_name: Option<&str>,
) {
    auth.phase = AuthPhase::FillingField;
    auth.field_error = None;
    if field_name == Some("provider_id") && auth.editing_provider_name.is_none() {
        auth.default_provider_id = false;
    }
    let Some(provider_id) = auth.editing_provider_name.clone() else {
        let fields = &auth.providers[auth.selected_provider].fields;
        let secret_fields: HashSet<_> = fields
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.name.as_str())
            .collect();
        auth.collected_values
            .retain(|name, _| !secret_fields.contains(name.as_str()));
        auth.current_field = retry_field(auth, field_name);
        auth.load_current_field_input();
        return;
    };

    let fields = &auth.providers[auth.selected_provider].fields;
    let secret_fields: HashSet<String> = fields
        .iter()
        .filter(|field| field.secret)
        .map(|field| field.name.clone())
        .collect();

    // An edit retry keeps what the user already re-confirmed, so a single rejected field does
    // not force retyping the whole provider. Secrets are the exception: only the bullet mask
    // survives (cosh-core swaps it back for the stored credential), while a real secret typed
    // into the failed attempt is dropped rather than kept in memory for another round.
    auth.collected_values
        .retain(|name, value| !secret_fields.contains(name) || is_bullet_mask(value));
    auth.collected_values
        .insert("provider_id".to_string(), provider_id);
    clear_ecs_auth_source(&mut auth.collected_values);
    auth.current_field = retry_field(auth, field_name);
    auth.load_current_field_input();
}

fn retry_field(auth: &RuntimeAuthState, field_name: Option<&str>) -> usize {
    field_name
        .and_then(|name| {
            auth.current_provider()
                .fields
                .iter()
                .position(|field| field.name == name)
        })
        .filter(|&index| auth.field_is_editable(index))
        .unwrap_or_else(|| auth.first_editable_field())
}

/// Drops the ECS RAM-role marker, which the restored phase contradicts.
///
/// `auth_source` is not a template field, so the retain above keeps it — but the retry always
/// comes back to the manual `FillingField` prompts, and cosh-core reads
/// `auth_source=ecs_ram_role` as "credentials come from the instance metadata service": it
/// skips the AK/SK required-field check and then stores `None` for them. Leaving the marker in
/// place would make the retry ask for an Access Key pair and silently save nothing.
///
/// Only this key is removed. Other values with no matching field — a masked `security_token`, a
/// custom `base_url` — describe the provider rather than contradict the phase, so they survive.
fn clear_ecs_auth_source(values: &mut std::collections::HashMap<String, String>) {
    if values.get("auth_source").map(String::as_str) == Some(ECS_RAM_ROLE_AUTH_SOURCE) {
        values.remove("auth_source");
    }
}

/// Mirrors cosh-core's `preserve_masked_secret`: a value made only of `•` is the display mask
/// for a credential already on disk, not a credential the user typed.
fn is_bullet_mask(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch == '\u{2022}')
}
