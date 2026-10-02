use std::collections::HashMap;
use std::fmt;

use crate::config::{CoreConfig, ProviderConfig};
use crate::protocol::{AuthField, AuthProvider};

mod exchange;
mod preflight;
mod validation;

pub(crate) use exchange::request_validated_auth;
pub(crate) use preflight::AuthPreflightError;
pub(crate) use validation::is_valid_provider_id;
pub(crate) use validation::AuthConfigValidationError;
use validation::{validate_auth_response, validate_base_url};

const DEFAULT_PLAN_MODEL: &str = "qwen3.7-plus";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceSite {
    China,
    International,
}

impl ServiceSite {
    fn from_env() -> Self {
        std::env::var("COSH_SERVICE_SITE")
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or(Self::China)
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "china" | "cn" => Some(Self::China),
            "international" | "intl" | "global" => Some(Self::International),
            _ => None,
        }
    }
}

struct PlanEndpoint {
    base_url: &'static str,
    api_key_url: &'static str,
}

fn coding_plan_endpoint(site: ServiceSite) -> PlanEndpoint {
    match site {
        ServiceSite::China => PlanEndpoint {
            base_url: "https://coding.dashscope.aliyuncs.com/v1",
            api_key_url:
                "https://bailian.console.aliyun.com/?tab=coding-plan#/efm/coding-plan-detail",
        },
        ServiceSite::International => PlanEndpoint {
            base_url: "https://coding-intl.dashscope.aliyuncs.com/v1",
            api_key_url:
                "https://modelstudio.console.alibabacloud.com/?tab=dashboard#/efm/coding_plan",
        },
    }
}

fn token_plan_endpoint(site: ServiceSite) -> PlanEndpoint {
    match site {
        ServiceSite::China => PlanEndpoint {
            base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
            api_key_url:
                "https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/token-plan",
        },
        ServiceSite::International => PlanEndpoint {
            base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
            api_key_url: "https://www.alibabacloud.com/help/en/model-studio/token-plan-quickstart",
        },
    }
}

fn plan_auth_provider(
    id: &str,
    label: &str,
    description: &str,
    description_zh_cn: &str,
    endpoint: PlanEndpoint,
) -> AuthProvider {
    AuthProvider {
        id: id.to_string(),
        label: label.to_string(),
        description: Some(description.to_string()),
        description_zh_cn: Some(description_zh_cn.to_string()),
        fields: vec![
            AuthField {
                name: "api_key".to_string(),
                label: "API Key".to_string(),
                hint: Some(format!(
                    "Plan keys start with sk-sp-. Get yours: {}",
                    endpoint.api_key_url
                )),
                secret: true,
                required: true,
                placeholder: None,
            },
            AuthField {
                name: "model".to_string(),
                label: "Model".to_string(),
                hint: Some(format!("Default: {DEFAULT_PLAN_MODEL}")),
                secret: false,
                required: false,
                placeholder: Some(DEFAULT_PLAN_MODEL.to_string()),
            },
        ],
        builtin_base_url: Some(endpoint.base_url.to_string()),
        builtin_provider_type: id.to_string(),
        builtin_default_model: Some(DEFAULT_PLAN_MODEL.to_string()),
    }
}

/// Returns the builtin provider templates for the auth UI.
pub fn builtin_auth_providers() -> Vec<AuthProvider> {
    builtin_auth_providers_for_site(ServiceSite::from_env())
}

fn builtin_auth_providers_for_site(site: ServiceSite) -> Vec<AuthProvider> {
    vec![
        AuthProvider {
            id: "aliyun".to_string(),
            label: "Aliyun Authentication".to_string(),
            description: Some("Free with limited quota".to_string()),
            description_zh_cn: Some("提供有限的免费额度".to_string()),
            fields: vec![
                AuthField {
                    name: "access_key_id".to_string(),
                    label: "Access Key ID".to_string(),
                    hint: Some("获取地址: https://ram.console.aliyun.com/manage/ak".to_string()),
                    secret: true,
                    required: true,
                    placeholder: None,
                },
                AuthField {
                    name: "access_key_secret".to_string(),
                    label: "Access Key Secret".to_string(),
                    hint: None,
                    secret: true,
                    required: true,
                    placeholder: None,
                },
                AuthField {
                    name: "model".to_string(),
                    label: "Model".to_string(),
                    hint: Some("默认: qwen3.7-plus".to_string()),
                    secret: false,
                    required: false,
                    placeholder: Some("qwen3.7-plus".to_string()),
                },
            ],
            builtin_base_url: None,
            builtin_provider_type: "aliyun".to_string(),
            builtin_default_model: Some("qwen3.7-plus".to_string()),
        },
        plan_auth_provider(
            "coding_plan",
            "Coding Plan",
            "For individual developers • Weekly quota included",
            "面向个人开发者 • 包含每周额度",
            coding_plan_endpoint(site),
        ),
        plan_auth_provider(
            "token_plan",
            "Token Plan",
            "For teams and companies • Usage-based billing with dedicated capacity",
            "面向团队和企业 • 按用量计费，提供专属容量",
            token_plan_endpoint(site),
        ),
        AuthProvider {
            id: "dashscope".to_string(),
            label: "DashScope (百炼)".to_string(),
            description: Some("Connect with an existing Bailian API key".to_string()),
            description_zh_cn: Some("使用现有的百炼 API Key".to_string()),
            fields: vec![
                AuthField {
                    name: "api_key".to_string(),
                    label: "API Key".to_string(),
                    hint: Some(
                        "获取地址: https://bailian.console.aliyun.com/?tab=model#/api-key"
                            .to_string(),
                    ),
                    secret: true,
                    required: true,
                    placeholder: None,
                },
                AuthField {
                    name: "model".to_string(),
                    label: "Model".to_string(),
                    hint: Some("默认: qwen3.7-plus, e.g. qwen3.7-max, deepseek-v4-pro".to_string()),
                    secret: false,
                    required: false,
                    placeholder: Some("qwen3.7-plus".to_string()),
                },
            ],
            builtin_base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
            builtin_provider_type: "dashscope".to_string(),
            builtin_default_model: Some("qwen3.7-plus".to_string()),
        },
        AuthProvider {
            id: "openai_compat".to_string(),
            label: "OpenAI Compatible".to_string(),
            description: Some("Use an existing OpenAI-compatible Base URL and API key".to_string()),
            description_zh_cn: Some("使用现有的 OpenAI 兼容 Base URL 和 API Key".to_string()),
            fields: vec![
                AuthField {
                    name: "base_url".to_string(),
                    label: "Base URL".to_string(),
                    hint: Some("例如: https://api.openai.com/v1".to_string()),
                    secret: false,
                    required: true,
                    placeholder: Some("https://api.openai.com/v1".to_string()),
                },
                AuthField {
                    name: "api_key".to_string(),
                    label: "API Key".to_string(),
                    hint: Some("sk-...".to_string()),
                    secret: true,
                    required: true,
                    placeholder: None,
                },
                AuthField {
                    name: "model".to_string(),
                    label: "Model".to_string(),
                    hint: Some("e.g. qwen3.7-max, deepseek-v4-pro".to_string()),
                    secret: false,
                    required: true,
                    placeholder: None,
                },
            ],
            builtin_base_url: None,
            builtin_provider_type: "openai".to_string(),
            builtin_default_model: None,
        },
    ]
}

/// Response from the auth flow.
pub struct AuthResponse {
    pub provider_id: String,
    pub provider_type: Option<String>,
    pub values: HashMap<String, String>,
    pub persist: bool,
}

/// Applies validated auth credentials and rebuilds provider settings.
///
/// # Errors
///
/// Returns an error when a required credential field is missing or a supplied
/// base URL is malformed.
pub(crate) fn apply_auth_credentials(
    config: &mut CoreConfig,
    response: &AuthResponse,
) -> Result<(), AuthConfigValidationError> {
    if !is_valid_provider_id(&response.provider_id) {
        return Err(AuthConfigValidationError::InvalidProviderId);
    }
    let template_id = response
        .provider_type
        .as_deref()
        .unwrap_or(response.provider_id.as_str());
    let template = builtin_auth_providers()
        .into_iter()
        .find(|provider| provider.id == template_id);
    match template.as_ref() {
        Some(template) => validate_auth_response(template, response)?,
        None => {
            let base_url = response
                .values
                .get("base_url")
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    AuthConfigValidationError::MissingRequiredField("base_url".to_string())
                })?;
            validate_base_url(base_url)?;
        }
    }

    let (base_url, provider_type, default_model) = match template {
        Some(template) => (
            response
                .values
                .get("base_url")
                .cloned()
                .or(template.builtin_base_url)
                .unwrap_or_default(),
            template.builtin_provider_type,
            template.builtin_default_model,
        ),
        None => (
            response.values.get("base_url").cloned().unwrap_or_default(),
            "generic".to_string(),
            None,
        ),
    };

    let user_model = response
        .values
        .get("model")
        .filter(|m| !m.is_empty())
        .cloned();
    let final_model = user_model.or(default_model);

    let api_key = response.values.get("api_key").cloned().unwrap_or_default();

    // Aliyun provider uses AK/SK instead of API key
    let auth_source = response.values.get("auth_source").cloned();
    let is_ecs_ram_role = auth_source.as_deref() == Some("ecs_ram_role");
    let access_key_id = if is_ecs_ram_role {
        None
    } else {
        response.values.get("access_key_id").cloned()
    };
    let access_key_secret = if is_ecs_ram_role {
        None
    } else {
        response.values.get("access_key_secret").cloned()
    };
    let security_token = if is_ecs_ram_role {
        None
    } else {
        response.values.get("security_token").cloned()
    };

    // Preserve settings the auth response cannot carry, so a 401/403 re-auth
    // does not silently reset them to None (which persist_config_to_dir would
    // then omit): the user's explicit cache preference, and any SysOM endpoint
    // they configured -- losing the latter would quietly switch the client back
    // to probing.
    let existing = config.ai.providers.get(&response.provider_id);
    let existing_explicit_cache = existing.and_then(|p| p.explicit_cache);
    let existing_sysom_endpoint = existing.and_then(|p| p.sysom_endpoint.clone());

    config.ai.active_provider = Some(response.provider_id.clone());
    config.ai.active_model = final_model.clone();
    let provider = ProviderConfig {
        provider_type: Some(provider_type),
        auth_source,
        base_url: Some(base_url),
        sysom_endpoint: existing_sysom_endpoint,
        api_key: Some(api_key),
        model: final_model.clone(),
        extra_params: None,
        access_key_id,
        access_key_secret,
        security_token,
        explicit_cache: existing_explicit_cache,
    };
    config
        .ai
        .providers
        .insert(response.provider_id.clone(), provider.clone());
    if response.persist {
        config.user_ai.active_provider = Some(response.provider_id.clone());
        config.user_ai.active_model = final_model;
        config
            .user_ai
            .providers
            .insert(response.provider_id.clone(), provider);
    }
    Ok(())
}

/// Authentication configuration failures remain classified across the core/shell boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthConfigureError {
    Validation(AuthConfigValidationError),
    Preflight(AuthPreflightError),
    Persistence,
}

impl AuthConfigureError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Validation(AuthConfigValidationError::MissingRequiredField(field)) => {
                match field.as_str() {
                    "api_key" => "missing_credentials",
                    "access_key_id" => "missing_access_key_id",
                    "access_key_secret" => "missing_access_key_secret",
                    "model" => "missing_model",
                    "base_url" => "missing_base_url",
                    _ => "invalid_configuration",
                }
            }
            Self::Validation(AuthConfigValidationError::InvalidBaseUrl) => "invalid_base_url",
            Self::Validation(AuthConfigValidationError::InvalidProviderId) => "invalid_provider_id",
            Self::Preflight(error) => error.code(),
            Self::Persistence => "persistence_failed",
        }
    }
}

impl fmt::Display for AuthConfigureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::Preflight(error) => error.fmt(formatter),
            Self::Persistence => formatter.write_str(
                "Configuration was validated but could not be saved. Check config permissions and try again.",
            ),
        }
    }
}

impl std::error::Error for AuthConfigureError {}

impl From<AuthConfigValidationError> for AuthConfigureError {
    fn from(error: AuthConfigValidationError) -> Self {
        Self::Validation(error)
    }
}

impl From<AuthPreflightError> for AuthConfigureError {
    fn from(error: AuthPreflightError) -> Self {
        Self::Preflight(error)
    }
}

/// Builds and validates an auth candidate without mutating the active configuration.
pub(crate) async fn prepare_auth_candidate(
    current: &CoreConfig,
    response: &AuthResponse,
) -> Result<CoreConfig, AuthConfigureError> {
    let mut candidate = current.clone();
    apply_auth_credentials(&mut candidate, response)?;
    let mut resolved = candidate.resolve_provider();
    if response.provider_type.as_deref() == Some("openai_compat") {
        resolved.provider_type = "openai_compat".to_string();
    }
    preflight::preflight_auth(&resolved).await?;
    Ok(candidate)
}

/// Completes the auth transaction and returns a candidate safe to publish in memory.
pub(crate) async fn prepare_and_persist_auth_candidate<F>(
    current: &CoreConfig,
    response: &AuthResponse,
    persist: F,
) -> Result<CoreConfig, AuthConfigureError>
where
    F: FnOnce(&CoreConfig) -> Result<(), String>,
{
    let candidate = prepare_auth_candidate(current, response).await?;
    if response.persist {
        persist(&candidate).map_err(|_| AuthConfigureError::Persistence)?;
    }
    Ok(candidate)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoveAuthProviderError {
    NotFound,
    NotEditable,
}

impl fmt::Display for RemoveAuthProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("provider not found"),
            Self::NotEditable => formatter.write_str("provider is not removable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemovedAuthProvider {
    pub(crate) active_provider: Option<String>,
}

pub(crate) fn remove_auth_provider(
    config: &mut CoreConfig,
    provider_id: &str,
) -> Result<RemovedAuthProvider, RemoveAuthProviderError> {
    if !config.user_ai.providers.contains_key(provider_id) {
        return Err(if config.ai.providers.contains_key(provider_id) {
            RemoveAuthProviderError::NotEditable
        } else {
            RemoveAuthProviderError::NotFound
        });
    }

    let was_active = config.ai.active_provider.as_deref() == Some(provider_id);
    config.user_ai.providers.remove(provider_id);
    if config.user_ai.active_provider.as_deref() == Some(provider_id) {
        config.user_ai.active_provider = None;
        config.user_ai.active_model = None;
    }

    if let Some(system_provider) = config.system_ai.providers.get(provider_id).cloned() {
        config
            .ai
            .providers
            .insert(provider_id.to_string(), system_provider);
    } else {
        config.ai.providers.remove(provider_id);
    }

    if was_active {
        let active_provider = config
            .system_ai
            .active_provider
            .clone()
            .filter(|fallback| config.ai.providers.contains_key(fallback));
        config.ai.active_provider = active_provider.clone();
        config.ai.active_model = active_provider.as_ref().and_then(|fallback| {
            config
                .ai
                .providers
                .get(fallback)
                .and_then(|provider| provider.model.clone())
        });
    }

    Ok(RemovedAuthProvider {
        active_provider: config.ai.active_provider.clone(),
    })
}

/// Check if an error string indicates an auth failure (401/403).
pub fn is_auth_error(error: &str) -> bool {
    error.contains("401") || error.contains("403") || error.contains("Unauthorized")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;

    fn model_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let body = r#"{"id":"test-model","object":"model"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        format!("http://{address}/v1")
    }

    fn transaction_response(base_url: String) -> AuthResponse {
        AuthResponse {
            provider_id: "transaction-provider".to_string(),
            provider_type: Some("openai_compat".to_string()),
            values: HashMap::from([
                ("base_url".to_string(), base_url),
                ("api_key".to_string(), "sk-test".to_string()),
                ("model".to_string(), "test-model".to_string()),
            ]),
            persist: true,
        }
    }

    #[tokio::test]
    async fn successful_transaction_persists_candidate_before_publication() {
        let current = CoreConfig::default();
        let persisted = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&persisted);
        let candidate = prepare_and_persist_auth_candidate(
            &current,
            &transaction_response(model_server()),
            move |candidate| {
                assert_eq!(
                    candidate.ai.active_provider.as_deref(),
                    Some("transaction-provider")
                );
                observed.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .expect("transaction succeeds");

        assert!(persisted.load(Ordering::SeqCst));
        assert!(current.ai.providers.is_empty());
        assert!(candidate.ai.providers.contains_key("transaction-provider"));
    }

    #[tokio::test]
    async fn persistence_failure_leaves_runtime_config_unchanged() {
        let current = CoreConfig::default();
        let result = prepare_and_persist_auth_candidate(
            &current,
            &transaction_response(model_server()),
            |_| Err("read-only config".to_string()),
        )
        .await;

        assert!(matches!(result, Err(AuthConfigureError::Persistence)));
        assert!(current.ai.providers.is_empty());
        assert!(current.user_ai.providers.is_empty());
    }

    #[tokio::test]
    async fn local_validation_failure_never_invokes_persistence() {
        let current = CoreConfig::default();
        let invoked = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&invoked);
        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join("config.toml");
        let attempted_path = config_path.clone();
        let response = transaction_response("not-a-url".to_string());
        let result = prepare_and_persist_auth_candidate(&current, &response, move |_| {
            observed.store(true, Ordering::SeqCst);
            std::fs::write(&attempted_path, "should not exist").unwrap();
            Ok(())
        })
        .await;

        assert!(matches!(
            result,
            Err(AuthConfigureError::Validation(
                AuthConfigValidationError::InvalidBaseUrl
            ))
        ));
        assert!(!invoked.load(Ordering::SeqCst));
        assert!(!config_path.exists());
        assert!(current.ai.providers.is_empty());
    }

    #[test]
    fn aliyun_missing_credentials_preserve_field_codes() {
        for (field, expected) in [
            ("access_key_id", "missing_access_key_id"),
            ("access_key_secret", "missing_access_key_secret"),
        ] {
            let error = AuthConfigureError::Validation(
                AuthConfigValidationError::MissingRequiredField(field.to_string()),
            );
            assert_eq!(error.code(), expected);
        }
    }

    #[test]
    fn builtin_providers_have_correct_ids() {
        let providers = builtin_auth_providers();
        assert_eq!(providers.len(), 5);
        assert_eq!(providers[0].id, "aliyun");
        assert_eq!(providers[1].id, "coding_plan");
        assert_eq!(providers[2].id, "token_plan");
        assert_eq!(providers[3].id, "dashscope");
        assert_eq!(providers[4].id, "openai_compat");
    }

    #[test]
    fn plan_endpoints_match_service_site() {
        let coding_cn = coding_plan_endpoint(ServiceSite::China);
        let coding_intl = coding_plan_endpoint(ServiceSite::International);
        let token_cn = token_plan_endpoint(ServiceSite::China);
        let token_intl = token_plan_endpoint(ServiceSite::International);

        assert_eq!(
            coding_cn.base_url,
            "https://coding.dashscope.aliyuncs.com/v1"
        );
        assert_eq!(
            coding_intl.base_url,
            "https://coding-intl.dashscope.aliyuncs.com/v1"
        );
        assert_eq!(
            token_cn.base_url,
            "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(
            token_intl.base_url,
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn international_site_builds_plan_templates_with_international_urls() {
        let providers = builtin_auth_providers_for_site(ServiceSite::International);
        let coding_plan = providers
            .iter()
            .find(|provider| provider.id == "coding_plan")
            .unwrap();
        let token_plan = providers
            .iter()
            .find(|provider| provider.id == "token_plan")
            .unwrap();

        assert_eq!(
            coding_plan.builtin_base_url.as_deref(),
            Some("https://coding-intl.dashscope.aliyuncs.com/v1")
        );
        assert_eq!(
            coding_plan.description_zh_cn.as_deref(),
            Some("面向个人开发者 • 包含每周额度")
        );
        assert_eq!(
            coding_plan.description.as_deref(),
            Some("For individual developers • Weekly quota included")
        );
        assert_eq!(
            token_plan.builtin_base_url.as_deref(),
            Some("https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1")
        );
        assert_eq!(
            token_plan.description.as_deref(),
            Some("For teams and companies • Usage-based billing with dedicated capacity")
        );
        assert_eq!(
            token_plan.description_zh_cn.as_deref(),
            Some("面向团队和企业 • 按用量计费，提供专属容量")
        );
    }

    #[test]
    fn service_site_accepts_packaging_aliases() {
        assert_eq!(ServiceSite::parse("cn"), Some(ServiceSite::China));
        assert_eq!(
            ServiceSite::parse("global"),
            Some(ServiceSite::International)
        );
        assert_eq!(ServiceSite::parse("unknown"), None);
    }

    #[test]
    fn dashscope_has_builtin_base_url() {
        let providers = builtin_auth_providers();
        let ds = providers
            .iter()
            .find(|provider| provider.id == "dashscope")
            .unwrap();
        assert!(ds.builtin_base_url.is_some());
        assert_eq!(ds.fields.len(), 2);
        assert_eq!(ds.fields[0].name, "api_key");
        assert_eq!(ds.fields[1].name, "model");
    }

    #[test]
    fn openai_compat_has_no_builtin_base_url() {
        let providers = builtin_auth_providers();
        let oc = providers
            .iter()
            .find(|provider| provider.id == "openai_compat")
            .unwrap();
        assert!(oc.builtin_base_url.is_none());
        assert_eq!(oc.fields.len(), 3);
        assert_eq!(oc.fields[0].name, "base_url");
        assert_eq!(oc.fields[1].name, "api_key");
        assert_eq!(oc.fields[2].name, "model");
    }

    #[test]
    fn apply_dashscope_credentials() {
        let mut config = CoreConfig::default();
        let response = AuthResponse {
            provider_id: "dashscope".to_string(),
            provider_type: None,
            values: HashMap::from([("api_key".to_string(), "sk-test123".to_string())]),
            persist: true,
        };
        apply_auth_credentials(&mut config, &response).unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("dashscope"));
        let p = config.ai.providers.get("dashscope").unwrap();
        assert_eq!(p.api_key.as_deref(), Some("sk-test123"));
        assert_eq!(
            p.base_url.as_deref(),
            Some("https://dashscope.aliyuncs.com/compatible-mode/v1")
        );
        assert_eq!(p.provider_type.as_deref(), Some("dashscope"));
        assert_eq!(p.model.as_deref(), Some("qwen3.7-plus"));
    }

    #[test]
    fn apply_coding_plan_credentials_uses_preset_endpoint() {
        let mut config = CoreConfig::default();
        let response = AuthResponse {
            provider_id: "coding-plan".to_string(),
            provider_type: Some("coding_plan".to_string()),
            values: HashMap::from([("api_key".to_string(), "sk-sp-test".to_string())]),
            persist: true,
        };

        apply_auth_credentials(&mut config, &response).unwrap();

        let provider = config.ai.providers.get("coding-plan").unwrap();
        assert_eq!(provider.provider_type.as_deref(), Some("coding_plan"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://coding.dashscope.aliyuncs.com/v1")
        );
        assert_eq!(provider.model.as_deref(), Some(DEFAULT_PLAN_MODEL));
    }

    #[test]
    fn apply_openai_compat_credentials() {
        let mut config = CoreConfig::default();
        let response = AuthResponse {
            provider_id: "openai_compat".to_string(),
            provider_type: None,
            values: HashMap::from([
                (
                    "base_url".to_string(),
                    "https://api.openai.com/v1".to_string(),
                ),
                ("api_key".to_string(), "sk-openai".to_string()),
                ("model".to_string(), "gpt-4o".to_string()),
            ]),
            persist: false,
        };
        apply_auth_credentials(&mut config, &response).unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("openai_compat"));
        let p = config.ai.providers.get("openai_compat").unwrap();
        assert_eq!(p.api_key.as_deref(), Some("sk-openai"));
        assert_eq!(p.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(p.provider_type.as_deref(), Some("openai"));
    }

    #[test]
    fn apply_credentials_uses_provider_id_as_config_key_and_type_as_template() {
        let mut config = CoreConfig::default();
        let response = AuthResponse {
            provider_id: "qwen-prod".to_string(),
            provider_type: Some("dashscope".to_string()),
            values: HashMap::from([("api_key".to_string(), "sk-prod".to_string())]),
            persist: true,
        };

        apply_auth_credentials(&mut config, &response).unwrap();

        assert_eq!(config.ai.active_provider.as_deref(), Some("qwen-prod"));
        assert!(config.ai.providers.contains_key("qwen-prod"));
        assert!(!config.ai.providers.contains_key("dashscope"));
        let provider = config.ai.providers.get("qwen-prod").unwrap();
        assert_eq!(provider.provider_type.as_deref(), Some("dashscope"));
        assert_eq!(provider.api_key.as_deref(), Some("sk-prod"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://dashscope.aliyuncs.com/compatible-mode/v1")
        );
    }

    #[test]
    fn apply_unknown_provider_preserves_generic_fallback() {
        let mut config = CoreConfig::default();
        let response = AuthResponse {
            provider_id: "custom-provider".to_string(),
            provider_type: Some("custom-provider".to_string()),
            values: HashMap::from([
                (
                    "base_url".to_string(),
                    "https://api.example.com/v1".to_string(),
                ),
                ("api_key".to_string(), "sk-custom".to_string()),
                ("model".to_string(), "custom-model".to_string()),
            ]),
            persist: true,
        };

        apply_auth_credentials(&mut config, &response).unwrap();

        let provider = config.ai.providers.get("custom-provider").unwrap();
        assert_eq!(provider.provider_type.as_deref(), Some("generic"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(provider.model.as_deref(), Some("custom-model"));
    }

    #[test]
    fn apply_unknown_provider_rejects_invalid_base_url() {
        let mut config = CoreConfig::default();
        let response = AuthResponse {
            provider_id: "custom-provider".to_string(),
            provider_type: Some("custom-provider".to_string()),
            values: HashMap::from([(
                "base_url".to_string(),
                "file:///tmp/custom-provider".to_string(),
            )]),
            persist: true,
        };

        assert_eq!(
            apply_auth_credentials(&mut config, &response),
            Err(AuthConfigValidationError::InvalidBaseUrl)
        );
        assert!(config.ai.providers.is_empty());
        assert!(config.user_ai.providers.is_empty());
    }

    #[test]
    fn apply_unknown_provider_requires_base_url() {
        for values in [
            HashMap::new(),
            HashMap::from([("base_url".to_string(), " ".to_string())]),
        ] {
            let mut config = CoreConfig::default();
            let response = AuthResponse {
                provider_id: "custom-provider".to_string(),
                provider_type: Some("custom-provider".to_string()),
                values,
                persist: true,
            };

            assert_eq!(
                apply_auth_credentials(&mut config, &response),
                Err(AuthConfigValidationError::MissingRequiredField(
                    "base_url".to_string()
                ))
            );
            assert!(config.ai.providers.is_empty());
            assert!(config.user_ai.providers.is_empty());
        }
    }

    #[test]
    fn persisted_auth_credentials_update_user_layer_only_for_persistence() {
        let mut config = CoreConfig::default();
        config.ai.providers.insert(
            "system-provider".to_string(),
            ProviderConfig {
                provider_type: Some("dashscope".to_string()),
                api_key: Some("sk-system".to_string()),
                ..Default::default()
            },
        );
        let response = AuthResponse {
            provider_id: "user-provider".to_string(),
            provider_type: Some("dashscope".to_string()),
            values: HashMap::from([("api_key".to_string(), "sk-user".to_string())]),
            persist: true,
        };

        apply_auth_credentials(&mut config, &response).unwrap();

        assert!(config.ai.providers.contains_key("system-provider"));
        assert!(config.ai.providers.contains_key("user-provider"));
        assert!(!config.user_ai.providers.contains_key("system-provider"));
        assert_eq!(
            config.user_ai.active_provider.as_deref(),
            Some("user-provider")
        );
        assert_eq!(
            config
                .user_ai
                .providers
                .get("user-provider")
                .and_then(|provider| provider.api_key.as_deref()),
            Some("sk-user")
        );
    }

    #[test]
    fn auth_refresh_preserves_explicit_cache() {
        let mut config = CoreConfig::default();
        config.ai.providers.insert(
            "dashscope".to_string(),
            ProviderConfig {
                provider_type: Some("dashscope".to_string()),
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                api_key: Some("sk-old".to_string()),
                model: Some("qwen3.7-plus".to_string()),
                explicit_cache: Some(true),
                ..Default::default()
            },
        );

        // Simulate 401/403 re-auth: same provider_id, new api_key
        let response = AuthResponse {
            provider_id: "dashscope".to_string(),
            provider_type: None,
            values: HashMap::from([("api_key".to_string(), "sk-new".to_string())]),
            persist: true,
        };
        apply_auth_credentials(&mut config, &response).unwrap();

        let p = config.ai.providers.get("dashscope").unwrap();
        assert_eq!(p.api_key.as_deref(), Some("sk-new"));
        // explicit_cache must survive re-auth
        assert_eq!(p.explicit_cache, Some(true));
    }

    #[test]
    fn auth_refresh_preserves_sysom_endpoint() {
        let mut config = CoreConfig::default();
        config.ai.providers.insert(
            "aliyun".to_string(),
            ProviderConfig {
                provider_type: Some("aliyun".to_string()),
                sysom_endpoint: Some("https://sysom.cn-shanghai.aliyuncs.com".to_string()),
                access_key_id: Some("old-ak".to_string()),
                model: Some("qwen3.7-plus".to_string()),
                ..Default::default()
            },
        );

        let response = AuthResponse {
            provider_id: "aliyun".to_string(),
            provider_type: None,
            values: HashMap::from([
                ("access_key_id".to_string(), "new-ak".to_string()),
                ("access_key_secret".to_string(), "new-sk".to_string()),
            ]),
            persist: true,
        };
        apply_auth_credentials(&mut config, &response).unwrap();

        let p = config.ai.providers.get("aliyun").unwrap();
        assert_eq!(p.access_key_id.as_deref(), Some("new-ak"));
        // Losing this would silently switch the client back to probing.
        assert_eq!(
            p.sysom_endpoint.as_deref(),
            Some("https://sysom.cn-shanghai.aliyuncs.com")
        );
    }

    #[test]
    fn remove_auth_provider_drops_user_credentials_and_active_selection() {
        let provider = ProviderConfig {
            provider_type: Some("dashscope".to_string()),
            api_key: Some("sk-user".to_string()),
            model: Some("user-model".to_string()),
            ..Default::default()
        };
        let mut config = CoreConfig::default();
        config.ai.active_provider = Some("user-provider".to_string());
        config.ai.active_model = Some("user-model".to_string());
        config
            .ai
            .providers
            .insert("user-provider".to_string(), provider.clone());
        config.user_ai.active_provider = Some("user-provider".to_string());
        config.user_ai.active_model = Some("user-model".to_string());
        config
            .user_ai
            .providers
            .insert("user-provider".to_string(), provider);

        let removed = remove_auth_provider(&mut config, "user-provider").unwrap();

        assert_eq!(removed.active_provider, None);
        assert_eq!(config.ai.active_provider, None);
        assert_eq!(config.ai.active_model, None);
        assert_eq!(config.user_ai.active_model, None);
        assert!(!config.ai.providers.contains_key("user-provider"));
        assert!(!config.user_ai.providers.contains_key("user-provider"));
    }

    #[test]
    fn remove_auth_provider_reveals_system_provider_with_same_id() {
        let system_provider = ProviderConfig {
            provider_type: Some("dashscope".to_string()),
            api_key: Some("sk-system".to_string()),
            model: Some("system-model".to_string()),
            ..Default::default()
        };
        let user_provider = ProviderConfig {
            provider_type: Some("openai".to_string()),
            api_key: Some("sk-user".to_string()),
            model: Some("user-model".to_string()),
            ..Default::default()
        };
        let mut config = CoreConfig::default();
        config.ai.active_provider = Some("shared".to_string());
        config
            .ai
            .providers
            .insert("shared".to_string(), user_provider.clone());
        config
            .system_ai
            .providers
            .insert("shared".to_string(), system_provider);
        config.system_ai.active_provider = Some("shared".to_string());
        config.user_ai.active_provider = Some("shared".to_string());
        config
            .user_ai
            .providers
            .insert("shared".to_string(), user_provider);

        let removed = remove_auth_provider(&mut config, "shared").unwrap();

        assert_eq!(removed.active_provider.as_deref(), Some("shared"));
        assert_eq!(
            config
                .ai
                .providers
                .get("shared")
                .and_then(|provider| provider.api_key.as_deref()),
            Some("sk-system")
        );
        assert_eq!(config.ai.active_model.as_deref(), Some("system-model"));
        assert_eq!(config.user_ai.active_provider, None);
    }

    #[test]
    fn remove_auth_provider_rejects_system_provider() {
        let mut config = CoreConfig::default();
        config
            .ai
            .providers
            .insert("system-provider".to_string(), ProviderConfig::default());

        assert_eq!(
            remove_auth_provider(&mut config, "system-provider"),
            Err(RemoveAuthProviderError::NotEditable)
        );
    }

    #[test]
    fn is_auth_error_detects_401() {
        assert!(is_auth_error("API error 401: invalid api key"));
        assert!(is_auth_error("HTTP 403 Forbidden"));
        assert!(is_auth_error("Unauthorized access"));
        assert!(!is_auth_error("API error 500: internal server error"));
    }
}
