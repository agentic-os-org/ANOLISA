//! Authentication registry commands and credential update rules.

use super::*;
use crate::provider::sysom::{CredentialStatus, ProbeError};

fn ecs_probe_response(
    request_id: &str,
    result: Result<CredentialStatus, ProbeError>,
) -> OutputMessage {
    let (success, data, error) = match result {
        Ok(CredentialStatus::Ready) => (true, serde_json::json!({ "status": "ready" }), None),
        Ok(CredentialStatus::NotReady(reason)) => (
            true,
            serde_json::json!({ "status": "not_ready", "reason": reason.as_str() }),
            None,
        ),
        Err(error) => (
            false,
            serde_json::json!({ "error_code": error.code() }),
            Some(error.to_string()),
        ),
    };
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success,
        data: Some(data),
        error,
    }
}

fn ecs_prepare_response(
    request_id: &str,
    result: Result<Option<crate::provider::sysom::EcsAuthChallenge>, ProbeError>,
) -> OutputMessage {
    let data = match result {
        Ok(Some(challenge)) => serde_json::json!({
            "mode": "ecs_ram_role",
            "instance_id": challenge.instance_id,
            "console_url": challenge.console_url,
            "values": { "auth_source": "ecs_ram_role" }
        }),
        Ok(None) => serde_json::json!({ "mode": "manual" }),
        Err(error) => return ecs_probe_response(request_id, Err(error)),
    };
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success: true,
        data: Some(data),
        error: None,
    }
}

pub(super) async fn handle_auth(
    request_id: &str,
    action: &str,
    params: &Value,
    config: &mut CoreConfig,
) -> OutputMessage {
    match action {
        "state" => {
            let templates: Vec<Value> = crate::auth::builtin_auth_providers()
                .into_iter()
                .map(|provider| {
                    serde_json::json!({
                        "id": provider.id,
                        "provider_type": provider.id,
                        "label": provider.label,
                        "description": provider.description,
                        "description_zh_cn": provider.description_zh_cn,
                        "fields": provider.fields,
                        "builtin_base_url": provider.builtin_base_url,
                        "builtin_default_model": provider.builtin_default_model,
                    })
                })
                .collect();
            let active_provider = config.ai.active_provider.clone();
            let effective_auth_required = config.resolve_provider().auth_required();
            let saved_providers: Vec<Value> = config
                .ai
                .providers
                .iter()
                .map(|(provider_id, provider)| {
                    let source = if config.user_ai.providers.contains_key(provider_id) {
                        "user"
                    } else if config.system_ai.providers.contains_key(provider_id) {
                        "system"
                    } else {
                        "runtime"
                    };
                    let editable = source == "user";
                    serde_json::json!({
                        "provider_id": provider_id,
                        "provider_type": provider.provider_type,
                        "source": source,
                        "editable": editable,
                        "auth_source": provider.auth_source,
                        "model": provider.model,
                        "base_url": provider.base_url,
                        "api_key_len": provider.api_key.as_ref().map(|v| v.chars().count()).unwrap_or(0),
                        "access_key_id_len": provider.access_key_id.as_ref().map(|v| v.chars().count()).unwrap_or(0),
                        "access_key_secret_len": provider.access_key_secret.as_ref().map(|v| v.chars().count()).unwrap_or(0),
                        "security_token_len": provider.security_token.as_ref().map(|v| v.chars().count()).unwrap_or(0),
                        "active": Some(provider_id) == active_provider.as_ref(),
                        "has_api_key": provider.api_key.as_ref().is_some_and(|v| !v.is_empty()),
                        "has_access_key_id": provider.access_key_id.as_ref().is_some_and(|v| !v.is_empty()),
                        "has_access_key_secret": provider.access_key_secret.as_ref().is_some_and(|v| !v.is_empty()),
                    })
                })
                .collect();
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({
                    "templates": templates,
                    "saved_providers": saved_providers,
                    "active_provider": active_provider,
                    "effective_auth_required": effective_auth_required,
                })),
                error: None,
            }
        }
        "activate" => {
            let provider_id = params
                .get("provider_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if provider_id.is_empty() || !config.ai.providers.contains_key(provider_id) {
                return registry_error(request_id, "provider not found");
            }
            let mut candidate = config.clone();
            candidate.ai.active_provider = Some(provider_id.to_string());
            candidate.user_ai.active_provider = Some(provider_id.to_string());
            let model = candidate
                .ai
                .providers
                .get(provider_id)
                .and_then(|provider| provider.model.clone());
            candidate.ai.active_model.clone_from(&model);
            candidate.user_ai.active_model = model;
            if let Err(e) = crate::config::persist_config(&candidate) {
                return registry_error(request_id, &format!("failed to persist config: {e}"));
            }
            *config = candidate;
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({ "active_provider": provider_id })),
                error: None,
            }
        }
        "delete" => {
            let provider_id = params
                .get("provider_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if provider_id.is_empty() {
                return registry_error(request_id, "missing provider_id");
            }
            let mut candidate = config.clone();
            let removed = match crate::auth::remove_auth_provider(&mut candidate, provider_id) {
                Ok(removed) => removed,
                Err(error) => return registry_error(request_id, &error.to_string()),
            };
            if let Err(error) = crate::config::persist_config(&candidate) {
                return registry_error(request_id, &format!("failed to persist config: {error}"));
            }
            *config = candidate;
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({
                    "deleted_provider": provider_id,
                    "active_provider": removed.active_provider,
                })),
                error: None,
            }
        }
        "prepare" => {
            let provider_type = params
                .get("provider_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if provider_type.is_empty() {
                return registry_error(request_id, "missing provider_type");
            }
            let result = if provider_type == "aliyun" {
                crate::provider::sysom::detect_ecs_auth_challenge().await
            } else {
                Ok(None)
            };
            ecs_prepare_response(request_id, result)
        }
        "verify" => {
            let provider_type = params
                .get("provider_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let auth_source = params
                .get("auth_source")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if provider_type == "aliyun" && auth_source == "ecs_ram_role" {
                ecs_probe_response(
                    request_id,
                    crate::provider::sysom::probe_ecs_ram_role().await,
                )
            } else {
                OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: true,
                    data: Some(serde_json::json!({
                        "authorized": true
                    })),
                    error: None,
                }
            }
        }
        "configure" => {
            let provider_id = params
                .get("provider_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let provider_type = params
                .get("provider_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if provider_id.is_empty() || provider_type.is_empty() {
                return registry_error(request_id, "missing provider_id or provider_type");
            }
            if config.ai.providers.contains_key(provider_id)
                && !config.user_ai.providers.contains_key(provider_id)
            {
                return registry_error(request_id, "provider is not editable");
            }
            let mut values: std::collections::HashMap<String, String> = params
                .get("values")
                .and_then(|v| v.as_object())
                .map(|object| {
                    object
                        .iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|s| (key.clone(), s.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(existing) = config.ai.providers.get(provider_id) {
                preserve_masked_secret(&mut values, "api_key", existing.api_key.as_deref());
                preserve_masked_secret(
                    &mut values,
                    "access_key_id",
                    existing.access_key_id.as_deref(),
                );
                preserve_masked_secret(
                    &mut values,
                    "access_key_secret",
                    existing.access_key_secret.as_deref(),
                );
                preserve_masked_secret(
                    &mut values,
                    "security_token",
                    existing.security_token.as_deref(),
                );
            }
            let response = crate::auth::AuthResponse {
                provider_id: provider_id.to_string(),
                provider_type: Some(provider_type.to_string()),
                values,
                persist: true,
            };
            let candidate = match crate::auth::prepare_and_persist_auth_candidate(
                config,
                &response,
                crate::config::persist_config,
            )
            .await
            {
                Ok(candidate) => candidate,
                Err(error) => return auth_registry_error(request_id, &error),
            };
            *config = candidate;
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({ "provider_id": provider_id })),
                error: None,
            }
        }
        _ => registry_error(
            request_id,
            &format!("unsupported action for auth: {action}"),
        ),
    }
}

fn auth_registry_error(request_id: &str, error: &crate::auth::AuthConfigureError) -> OutputMessage {
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success: false,
        data: Some(serde_json::json!({ "error_code": error.code() })),
        error: Some(error.to_string()),
    }
}

fn preserve_masked_secret(
    values: &mut std::collections::HashMap<String, String>,
    key: &str,
    existing: Option<&str>,
) {
    let Some(value) = values.get(key) else {
        return;
    };
    if !value.is_empty() && value.chars().all(|ch| ch == '•') {
        if let Some(existing) = existing {
            values.insert(key.to_string(), existing.to_string());
        }
    }
}

#[cfg(test)]
mod ecs_tests {
    use super::*;
    use crate::provider::sysom::{EcsAuthChallenge, NotReadyReason};

    #[test]
    fn ecs_metadata_registry_prepare_encodes_ecs_and_manual() {
        for (challenge, expected) in [
            (None, serde_json::json!({"mode": "manual"})),
            (
                Some(EcsAuthChallenge {
                    instance_id: "i-test".to_string(),
                    console_url:
                        "https://alinux.console.aliyun.com/cn-shanghai/guide/cosh?instance=i-test"
                            .to_string(),
                }),
                serde_json::json!({"mode": "ecs_ram_role", "instance_id": "i-test", "console_url": "https://alinux.console.aliyun.com/cn-shanghai/guide/cosh?instance=i-test", "values": {"auth_source": "ecs_ram_role"}}),
            ),
        ] {
            let OutputMessage::RegistryResponse {
                success,
                data,
                error,
                ..
            } = ecs_prepare_response("prepare-id", Ok(challenge))
            else {
                panic!("registry response")
            };
            assert!(success);
            assert_eq!(data, Some(expected));
            assert_eq!(error, None);
        }
    }

    #[test]
    fn ecs_metadata_registry_prepare_never_masks_safe_errors_as_manual() {
        for probe_error in [
            ProbeError::AccessDenied,
            ProbeError::InvalidResponse,
            ProbeError::Http,
        ] {
            let OutputMessage::RegistryResponse {
                success,
                data,
                error,
                ..
            } = ecs_prepare_response("prepare-id", Err(probe_error))
            else {
                panic!("registry response")
            };
            assert!(!success);
            assert_eq!(
                data,
                Some(serde_json::json!({"error_code": probe_error.code()}))
            );
            assert_eq!(error, Some(probe_error.to_string()));
        }
    }

    #[test]
    fn ecs_metadata_registry_encodes_ready_and_not_ready() {
        for (status, expected) in [
            (
                CredentialStatus::Ready,
                serde_json::json!({"status": "ready"}),
            ),
            (
                CredentialStatus::NotReady(NotReadyReason::RoleMissing),
                serde_json::json!({"status": "not_ready", "reason": "role_missing"}),
            ),
            (
                CredentialStatus::NotReady(NotReadyReason::CredentialsExpired),
                serde_json::json!({"status": "not_ready", "reason": "credentials_expired"}),
            ),
        ] {
            let OutputMessage::RegistryResponse {
                request_id,
                success,
                data,
                error,
            } = ecs_probe_response("probe-id", Ok(status))
            else {
                panic!("expected registry response")
            };
            assert_eq!(request_id, "probe-id");
            assert!(success);
            assert_eq!(data, Some(expected));
            assert_eq!(error, None);
        }
    }

    #[test]
    fn ecs_metadata_registry_preserves_safe_errors() {
        for probe_error in [
            ProbeError::AccessDenied,
            ProbeError::InvalidResponse,
            ProbeError::Unreachable,
            ProbeError::Timeout,
            ProbeError::Http,
        ] {
            let OutputMessage::RegistryResponse {
                request_id,
                success,
                data,
                error,
            } = ecs_probe_response("probe-error-id", Err(probe_error))
            else {
                panic!("expected registry response")
            };
            assert_eq!(request_id, "probe-error-id");
            assert!(!success);
            assert_eq!(
                data,
                Some(serde_json::json!({"error_code": probe_error.code()}))
            );
            assert_eq!(error, Some(probe_error.to_string()));
        }
    }
}
