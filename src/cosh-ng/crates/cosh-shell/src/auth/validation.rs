//! Inline validation for the fields collected by the `/auth` flow.
//!
//! cosh-shell has no dependency on cosh-core, so the Provider ID rule is mirrored here to
//! reject bad input the moment it is submitted instead of after every field was collected.
//! cosh-core keeps its own check as the authoritative gate — this module only shortens the
//! feedback loop.

use crate::runtime::prelude::AuthFieldInfo;

use super::runtime::{AuthBackend, RuntimeAuthState};

/// Hint rendered under the Provider ID prompt; states the character rule up front.
pub(super) const PROVIDER_ID_HINT: &str =
    "Config name (not model name; letters, digits, '-' and '_' only), e.g. qwen-prod";

const PROVIDER_ID_FIELD: &str = "provider_id";

const PROVIDER_ID_EMPTY_ERROR: &str = "Provider ID cannot be empty.";

const PROVIDER_ID_CHARSET_ERROR: &str =
    "Provider ID allows letters, digits, '-' and '_' only (no '.').";

pub(super) const PROVIDER_ID_OCCUPIED_ERROR: &str =
    "Provider ID is already in use. Choose a different name.";

/// Outcome of submitting the value of the field currently being filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldSubmission {
    /// Value stored; the caller may advance to the next field.
    Accepted,
    /// Value rejected; the caller must stay on the current field and re-render.
    Rejected,
}

/// Mirrors cosh-core's `is_valid_provider_id`: ASCII letters, digits, `-` and `_`.
///
/// The character set is intentionally narrow because a provider id becomes a TOML table
/// key (`[ai.providers.<id>]`), where a `.` would be parsed as table nesting.
fn provider_id_error(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some(PROVIDER_ID_EMPTY_ERROR);
    }
    let valid = value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    (!valid).then_some(PROVIDER_ID_CHARSET_ERROR)
}

/// Returns the user-facing error for `value`, or `None` when the field accepts it.
pub(super) fn field_error(field_name: &str, value: &str) -> Option<&'static str> {
    match field_name {
        PROVIDER_ID_FIELD => provider_id_error(value),
        _ => None,
    }
}

/// Applies an in-progress edit to the active field, dropping any stale inline error.
pub(super) fn record_field_edit(auth: &mut RuntimeAuthState, text: &str) {
    auth.field_input = text.to_string();
    auth.field_error = None;
}

/// Stores a submitted field value, or rejects it inline without touching `collected_values`.
///
/// On rejection the raw input stays in `field_input` so the user can fix it in place, and
/// `field_error` carries the reason for `render_current_auth_panel` to display.
pub(super) fn record_field_submission(
    auth: &mut RuntimeAuthState,
    field: Option<&AuthFieldInfo>,
    value: String,
) -> FieldSubmission {
    let Some(field) = field else {
        auth.field_error = None;
        return FieldSubmission::Accepted;
    };
    let error = field_error(&field.name, &value).or_else(|| {
        (field.name == PROVIDER_ID_FIELD
            && auth.backend == AuthBackend::CoreRegistry
            && auth.editing_provider_name.is_none()
            && auth.provider_name_is_taken(&value))
        .then_some(PROVIDER_ID_OCCUPIED_ERROR)
    });
    if let Some(error) = error {
        auth.field_input = value;
        auth.field_error = Some(error.to_string());
        auth.field_capture_revision = auth.field_capture_revision.wrapping_add(1);
        return FieldSubmission::Rejected;
    }
    auth.field_error = None;
    auth.collected_values.insert(field.name.clone(), value);
    FieldSubmission::Accepted
}

#[cfg(test)]
mod tests {
    use super::{field_error, PROVIDER_ID_HINT};

    #[test]
    fn provider_id_rejects_dotted_and_non_ascii_values() {
        for value in ["qwen3.7-max", "bad.provider", "claude-3.5-sonnet", ""] {
            assert!(
                field_error("provider_id", value).is_some(),
                "expected rejection for {value:?}"
            );
        }
        assert!(field_error("provider_id", "\u{6a21}\u{578b}").is_some());
        assert!(field_error("provider_id", "qwen prod").is_some());
    }

    #[test]
    fn provider_id_accepts_letters_digits_dash_and_underscore() {
        for value in ["qwen-prod", "qwen_prod", "Qwen37", "q"] {
            assert_eq!(
                field_error("provider_id", value),
                None,
                "expected {value:?} to be accepted"
            );
        }
    }

    #[test]
    fn dotted_error_names_the_allowed_characters() {
        let error = field_error("provider_id", "qwen3.7-max").expect("dotted id rejected");
        assert!(error.contains("letters, digits"), "{error}");
        assert!(error.contains('.'), "{error}");
    }

    #[test]
    fn other_fields_are_not_constrained_by_the_provider_id_rule() {
        assert_eq!(
            field_error(
                "base_url",
                "https://dashscope.aliyuncs.com/compatible-mode/v1"
            ),
            None
        );
        assert_eq!(field_error("model", "qwen3.7-max"), None);
        assert_eq!(field_error("api_key", ""), None);
    }

    #[test]
    fn provider_id_hint_states_the_character_rule() {
        assert!(
            PROVIDER_ID_HINT.contains("letters, digits"),
            "{PROVIDER_ID_HINT}"
        );
        assert!(PROVIDER_ID_HINT.contains("'-'"), "{PROVIDER_ID_HINT}");
        assert!(PROVIDER_ID_HINT.contains('_'), "{PROVIDER_ID_HINT}");
    }
}
