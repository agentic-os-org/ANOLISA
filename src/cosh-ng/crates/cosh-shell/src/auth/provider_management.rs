//! Provider state and registry operations for the `/auth` management flow.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::adapter::{AdapterInstance, CoshCoreAdapter};
use crate::runtime::prelude::{AuthProviderInfo, AuthResponse};

/// A configured provider displayed by the provider-management panel.
#[derive(Debug, Clone)]
pub(crate) struct ExistingProvider {
    pub(crate) name: String,
    pub(crate) provider_type: String,
    pub(crate) label: String,
    pub(crate) model: String,
    pub(crate) is_active: bool,
    pub(crate) editable: bool,
    pub(crate) source: String,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key_mask: Option<String>,
    pub(crate) access_key_id_mask: Option<String>,
    pub(crate) access_key_secret_mask: Option<String>,
    pub(crate) security_token_mask: Option<String>,
    pub(crate) auth_source: Option<String>,
}

impl ExistingProvider {
    pub(super) fn deletable(&self) -> bool {
        self.source == "user"
    }
}

pub(super) struct CoreAuthState {
    pub(super) templates: Vec<AuthProviderInfo>,
    pub(super) existing_providers: Vec<ExistingProvider>,
}

#[derive(Debug, Deserialize)]
struct RegistryAuthState {
    templates: Vec<AuthProviderInfo>,
    #[serde(default)]
    saved_providers: Vec<CoreSavedProvider>,
}

#[derive(Debug, Deserialize)]
struct CoreSavedProvider {
    provider_id: String,
    provider_type: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    auth_source: Option<String>,
    active: bool,
    #[serde(default = "default_provider_source")]
    source: String,
    #[serde(default = "default_provider_editable")]
    editable: bool,
    #[serde(default)]
    api_key_len: usize,
    #[serde(default)]
    access_key_id_len: usize,
    #[serde(default)]
    access_key_secret_len: usize,
    #[serde(default)]
    security_token_len: usize,
}

fn default_provider_source() -> String {
    "user".to_string()
}

fn default_provider_editable() -> bool {
    true
}

fn secret_mask(len: usize) -> String {
    "•".repeat(len)
}

fn label_for_provider_type(provider_type: &str) -> &'static str {
    match provider_type {
        "dashscope" => "DashScope (\u{767e}\u{70bc})",
        "coding_plan" => "Coding Plan",
        "token_plan" => "Token Plan",
        "aliyun" => "Aliyun Authentication",
        _ => "OpenAI Compatible",
    }
}

impl From<CoreSavedProvider> for ExistingProvider {
    fn from(provider: CoreSavedProvider) -> Self {
        let provider_type = provider
            .provider_type
            .unwrap_or_else(|| "openai_compat".to_string());
        let provider_type = match provider_type.as_str() {
            "openai" | "generic" => "openai_compat".to_string(),
            _ => provider_type,
        };
        let model = provider.model.unwrap_or_default();
        Self {
            name: provider.provider_id,
            label: label_for_provider_type(&provider_type).to_string(),
            provider_type,
            model,
            is_active: provider.active,
            editable: provider.editable,
            source: provider.source,
            base_url: provider.base_url,
            api_key_mask: (provider.api_key_len > 0).then(|| secret_mask(provider.api_key_len)),
            access_key_id_mask: (provider.access_key_id_len > 0)
                .then(|| secret_mask(provider.access_key_id_len)),
            access_key_secret_mask: (provider.access_key_secret_len > 0)
                .then(|| secret_mask(provider.access_key_secret_len)),
            security_token_mask: (provider.security_token_len > 0)
                .then(|| secret_mask(provider.security_token_len)),
            auth_source: provider.auth_source,
        }
    }
}

fn restore_plan_template(provider: &mut ExistingProvider, templates: &[AuthProviderInfo]) {
    if !matches!(
        provider.provider_type.as_str(),
        "openai_compat" | "openai" | "generic" | "dashscope"
    ) {
        return;
    }
    let Some(base_url) = provider.base_url.as_deref() else {
        return;
    };
    let Some(template) = templates.iter().find(|template| {
        matches!(template.id.as_str(), "coding_plan" | "token_plan")
            && template
                .builtin_base_url
                .as_deref()
                .is_some_and(|template_url| {
                    template_url.trim_end_matches('/') == base_url.trim_end_matches('/')
                })
    }) else {
        return;
    };
    provider.provider_type.clone_from(&template.id);
    provider.label = label_for_provider_type(&template.id).to_string();
}

pub(super) fn load_core_auth_state(cosh_core: &CoshCoreAdapter) -> Result<CoreAuthState, String> {
    let value = cosh_core.registry_query("auth", "state", Value::Null)?;
    let state: RegistryAuthState =
        serde_json::from_value(value).map_err(|error| format!("invalid auth state: {error}"))?;
    let mut existing_providers: Vec<ExistingProvider> = state
        .saved_providers
        .into_iter()
        .map(|provider| {
            let mut provider = ExistingProvider::from(provider);
            restore_plan_template(&mut provider, &state.templates);
            provider
        })
        .collect();
    existing_providers.sort_by(|left, right| {
        right
            .is_active
            .cmp(&left.is_active)
            .then(left.name.cmp(&right.name))
    });
    Ok(CoreAuthState {
        templates: state.templates,
        existing_providers,
    })
}

pub(super) fn core_auth_activate(
    adapter: &AdapterInstance,
    provider_id: &str,
) -> Result<(), String> {
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        return Err("auth registry requires cosh-core backend".to_string());
    };
    cosh_core
        .registry_query("auth", "activate", json!({ "provider_id": provider_id }))
        .map(|_| ())
}

pub(super) fn core_auth_delete(adapter: &AdapterInstance, provider_id: &str) -> Result<(), String> {
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        return Err("auth registry requires cosh-core backend".to_string());
    };
    cosh_core
        .registry_query("auth", "delete", json!({ "provider_id": provider_id }))
        .map(|_| ())
}

pub(super) fn core_auth_configure(
    adapter: &AdapterInstance,
    response: &AuthResponse,
) -> Result<(), AuthConfigureFailure> {
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        return Err(AuthConfigureFailure {
            message: "auth registry requires cosh-core backend".to_string(),
            code: None,
        });
    };
    cosh_core
        .registry_query_classified(
            "auth",
            "configure",
            json!({
                "provider_id": response.provider_id,
                "provider_type": response.provider_type,
                "values": response.values,
            }),
        )
        .map(|_| ())
        .map_err(auth_configure_failure)
}

fn auth_configure_failure(error: crate::adapter::RegistryQueryError) -> AuthConfigureFailure {
    match error {
        crate::adapter::RegistryQueryError::Transport(message) => AuthConfigureFailure {
            message,
            code: None,
        },
        crate::adapter::RegistryQueryError::Response { message, code } => {
            AuthConfigureFailure { message, code }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AuthConfigureFailure {
    pub(super) message: String,
    pub(super) code: Option<String>,
}

impl AuthConfigureFailure {
    pub(super) fn focus_field(&self, provider_template_id: &str) -> Option<&'static str> {
        match self.code.as_deref() {
            Some("invalid_provider_id") => Some("provider_id"),
            Some("missing_access_key_id") => Some("access_key_id"),
            Some("missing_access_key_secret") => Some("access_key_secret"),
            Some("invalid_credentials" | "missing_credentials" | "permission_denied")
                if provider_template_id == "aliyun" =>
            {
                Some("access_key_id")
            }
            Some("invalid_credentials" | "missing_credentials" | "permission_denied") => {
                Some("api_key")
            }
            Some("model_unavailable" | "missing_model") => Some("model"),
            Some("invalid_base_url" | "missing_base_url" | "endpoint_unreachable" | "timeout") => {
                Some("base_url")
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod auth_configure_failure_tests {
    use super::{auth_configure_failure, AuthConfigureFailure};
    use crate::adapter::RegistryQueryError;

    #[test]
    fn saved_openai_protocol_types_recover_the_auth_template_identity() {
        for provider_type in ["openai", "generic", "openai_compat"] {
            let saved: super::CoreSavedProvider = serde_json::from_value(serde_json::json!({
                "provider_id": "prod",
                "provider_type": provider_type,
                "active": false
            }))
            .unwrap();
            let provider = super::ExistingProvider::from(saved);
            assert_eq!(provider.provider_type, "openai_compat");
        }
    }

    fn failure(code: &str) -> AuthConfigureFailure {
        AuthConfigureFailure {
            message: "safe message".to_string(),
            code: Some(code.to_string()),
        }
    }

    #[test]
    fn classified_failures_focus_actionable_fields() {
        for code in [
            "invalid_credentials",
            "permission_denied",
            "missing_credentials",
        ] {
            assert_eq!(failure(code).focus_field("dashscope"), Some("api_key"));
            assert_eq!(failure(code).focus_field("aliyun"), Some("access_key_id"));
        }
        assert_eq!(
            failure("missing_access_key_id").focus_field("aliyun"),
            Some("access_key_id")
        );
        assert_eq!(
            failure("missing_access_key_secret").focus_field("aliyun"),
            Some("access_key_secret")
        );
        for code in ["model_unavailable", "missing_model"] {
            assert_eq!(failure(code).focus_field("dashscope"), Some("model"));
        }
        for code in [
            "invalid_base_url",
            "missing_base_url",
            "endpoint_unreachable",
            "timeout",
        ] {
            assert_eq!(failure(code).focus_field("openai_compat"), Some("base_url"));
        }
        assert_eq!(
            failure("provider_unavailable").focus_field("dashscope"),
            None
        );
        for code in ["service_not_ready", "credential_source_unavailable"] {
            assert_eq!(failure(code).focus_field("aliyun"), None);
        }
    }

    #[test]
    fn shell_core_transport_failures_do_not_impersonate_provider_network_errors() {
        let failure = auth_configure_failure(RegistryQueryError::Transport(
            "cosh-core output stream disconnected".to_string(),
        ));

        assert_eq!(failure.code, None);
        assert_eq!(failure.focus_field("dashscope"), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderAction {
    Activate,
    Edit,
    Delete,
    Cancel,
}

impl ProviderAction {
    fn label(self) -> &'static str {
        match self {
            Self::Activate => "Set as active provider",
            Self::Edit => "Edit configuration",
            Self::Delete => "Delete provider",
            Self::Cancel => "Cancel",
        }
    }
}

pub(super) fn provider_actions(
    is_active: bool,
    editable: bool,
    deletable: bool,
) -> Vec<ProviderAction> {
    let mut actions = Vec::new();
    if !is_active {
        actions.push(ProviderAction::Activate);
    }
    if editable {
        actions.push(ProviderAction::Edit);
    }
    if deletable {
        actions.push(ProviderAction::Delete);
    }
    actions.push(ProviderAction::Cancel);
    actions
}

pub(super) fn provider_action_options(
    is_active: bool,
    editable: bool,
    deletable: bool,
) -> Vec<String> {
    provider_actions(is_active, editable, deletable)
        .into_iter()
        .map(|action| action.label().to_string())
        .collect()
}

pub(super) fn provider_action_choice(
    is_active: bool,
    editable: bool,
    deletable: bool,
    selected: usize,
) -> ProviderAction {
    provider_actions(is_active, editable, deletable)
        .get(selected)
        .copied()
        .unwrap_or(ProviderAction::Cancel)
}

#[cfg(test)]
mod tests {
    use super::{
        label_for_provider_type, provider_action_choice, provider_action_options,
        restore_plan_template, ExistingProvider, ProviderAction,
    };
    use crate::runtime::prelude::AuthProviderInfo;

    #[test]
    fn labels_plan_provider_types() {
        assert_eq!(label_for_provider_type("coding_plan"), "Coding Plan");
        assert_eq!(label_for_provider_type("token_plan"), "Token Plan");
    }

    #[test]
    fn restores_plan_only_for_the_selected_site_endpoint() {
        let templates = vec![AuthProviderInfo {
            id: "coding_plan".to_string(),
            label: "Coding Plan".to_string(),
            description: None,
            description_zh_cn: None,
            builtin_base_url: Some("https://coding.dashscope.aliyuncs.com/v1".to_string()),
            fields: Vec::new(),
        }];
        let mut china = existing_openai_provider("https://coding.dashscope.aliyuncs.com/v1/");
        let mut international =
            existing_openai_provider("https://coding-intl.dashscope.aliyuncs.com/v1");

        restore_plan_template(&mut china, &templates);
        restore_plan_template(&mut international, &templates);

        assert_eq!(china.provider_type, "coding_plan");
        assert_eq!(china.label, "Coding Plan");
        assert_eq!(international.provider_type, "openai");
        assert_eq!(international.label, "OpenAI Compatible");
    }

    fn existing_openai_provider(base_url: &str) -> ExistingProvider {
        ExistingProvider {
            name: "legacy".to_string(),
            provider_type: "openai".to_string(),
            label: "OpenAI Compatible".to_string(),
            model: "qwen3.7-plus".to_string(),
            is_active: false,
            editable: true,
            source: "user".to_string(),
            base_url: Some(base_url.to_string()),
            api_key_mask: None,
            access_key_id_mask: None,
            access_key_secret_mask: None,
            security_token_mask: None,
            auth_source: None,
        }
    }

    #[test]
    fn user_provider_actions_include_delete() {
        assert_eq!(
            provider_action_options(false, true, true),
            vec![
                "Set as active provider",
                "Edit configuration",
                "Delete provider",
                "Cancel",
            ]
        );
        assert_eq!(
            provider_action_choice(false, true, true, 2),
            ProviderAction::Delete
        );
    }

    #[test]
    fn system_provider_actions_exclude_edit_and_delete() {
        assert_eq!(
            provider_action_options(false, false, false),
            vec!["Set as active provider", "Cancel"]
        );
        assert_eq!(
            provider_action_choice(false, false, false, 1),
            ProviderAction::Cancel
        );
    }
}
