//! Deterministic validation for auth values before they mutate configuration.

use std::fmt;

use crate::protocol::AuthProvider;

use super::AuthResponse;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthConfigValidationError {
    MissingRequiredField(String),
    InvalidBaseUrl,
    InvalidProviderId,
}

impl fmt::Display for AuthConfigValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequiredField(field) => {
                write!(formatter, "missing required auth field: {field}")
            }
            Self::InvalidBaseUrl => formatter
                .write_str("invalid base_url: expected an http:// or https:// URL with a host"),
            Self::InvalidProviderId => formatter
                .write_str("invalid provider_id: expected letters, digits, '-' and '_' only"),
        }
    }
}

impl std::error::Error for AuthConfigValidationError {}

pub(super) fn validate_auth_response(
    template: &AuthProvider,
    response: &AuthResponse,
) -> Result<(), AuthConfigValidationError> {
    let uses_ecs_ram_role = template.id == "aliyun"
        && response.values.get("auth_source").map(String::as_str) == Some("ecs_ram_role");

    for field in &template.fields {
        if uses_ecs_ram_role && matches!(field.name.as_str(), "access_key_id" | "access_key_secret")
        {
            continue;
        }
        let missing = match response.values.get(&field.name) {
            Some(value) => value.trim().is_empty(),
            None => true,
        };
        if field.required && missing {
            return Err(AuthConfigValidationError::MissingRequiredField(
                field.name.clone(),
            ));
        }
    }

    let base_url = response
        .values
        .get("base_url")
        .map(String::as_str)
        .or(template.builtin_base_url.as_deref());
    if let Some(base_url) = base_url {
        validate_base_url(base_url)?;
    }
    Ok(())
}

pub(crate) fn is_valid_provider_id(provider_id: &str) -> bool {
    !provider_id.is_empty()
        && provider_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

pub(super) fn validate_base_url(base_url: &str) -> Result<(), AuthConfigValidationError> {
    let url =
        reqwest::Url::parse(base_url).map_err(|_| AuthConfigValidationError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AuthConfigValidationError::InvalidBaseUrl);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::auth::builtin_auth_providers;

    fn response(provider_type: &str, values: &[(&str, &str)]) -> AuthResponse {
        AuthResponse {
            provider_id: "test-provider".to_string(),
            provider_type: Some(provider_type.to_string()),
            values: values
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect::<HashMap<_, _>>(),
            persist: true,
        }
    }

    fn template(provider_type: &str) -> AuthProvider {
        builtin_auth_providers()
            .into_iter()
            .find(|provider| provider.id == provider_type)
            .unwrap()
    }

    #[test]
    fn rejects_malformed_base_url() {
        let template = template("openai_compat");
        let response = response(
            "openai_compat",
            &[
                (
                    "base_url",
                    "error-testhttps://dashscope.aliyuncs.com/compatible-mode/v1",
                ),
                ("api_key", "sk-test"),
                ("model", "qwen-test"),
            ],
        );

        assert_eq!(
            validate_auth_response(&template, &response),
            Err(AuthConfigValidationError::InvalidBaseUrl)
        );
    }

    #[test]
    fn rejects_non_http_base_url() {
        let template = template("openai_compat");
        let response = response(
            "openai_compat",
            &[
                ("base_url", "file:///tmp/openai"),
                ("api_key", "sk-test"),
                ("model", "qwen-test"),
            ],
        );

        assert_eq!(
            validate_auth_response(&template, &response),
            Err(AuthConfigValidationError::InvalidBaseUrl)
        );
    }

    #[test]
    fn rejects_base_url_without_host() {
        let template = template("openai_compat");
        let response = response(
            "openai_compat",
            &[
                ("base_url", "https://"),
                ("api_key", "sk-test"),
                ("model", "qwen-test"),
            ],
        );

        assert_eq!(
            validate_auth_response(&template, &response),
            Err(AuthConfigValidationError::InvalidBaseUrl)
        );
    }

    #[test]
    fn rejects_missing_required_credentials() {
        let template = template("openai_compat");
        let response = response(
            "openai_compat",
            &[
                ("base_url", "https://api.example.com/v1"),
                ("api_key", " "),
                ("model", "qwen-test"),
            ],
        );

        assert_eq!(
            validate_auth_response(&template, &response),
            Err(AuthConfigValidationError::MissingRequiredField(
                "api_key".to_string()
            ))
        );
    }

    #[test]
    fn rejects_missing_required_model() {
        let template = template("openai_compat");
        let response = response(
            "openai_compat",
            &[
                ("base_url", "https://api.example.com/v1"),
                ("api_key", "sk-test"),
                ("model", ""),
            ],
        );

        assert_eq!(
            validate_auth_response(&template, &response),
            Err(AuthConfigValidationError::MissingRequiredField(
                "model".to_string()
            ))
        );
    }

    #[test]
    fn accepts_aliyun_ecs_ram_role_without_manual_credentials() {
        let template = template("aliyun");
        let response = response("aliyun", &[("auth_source", "ecs_ram_role")]);

        assert_eq!(validate_auth_response(&template, &response), Ok(()));
    }
}
