//! Effective AI credential state resolved through the cosh-core registry.

use super::CoshCoreAdapter;

impl CoshCoreAdapter {
    /// Returns whether the active provider has usable credentials.
    ///
    /// cosh-core owns the per-provider semantics (env vars, user/system
    /// config layering, ECS RAM role exemption); this only transports the
    /// resolved boolean, so provider additions never touch the shell side.
    pub(crate) fn ai_configured(&self) -> Result<bool, String> {
        let value = self.registry_query("auth", "state", serde_json::Value::Null)?;
        auth_state_is_configured(&value)
    }
}

fn auth_state_is_configured(value: &serde_json::Value) -> Result<bool, String> {
    if let Some(required) = value
        .get("effective_auth_required")
        .and_then(serde_json::Value::as_bool)
    {
        return Ok(!required);
    }
    // Legacy fallback for cores that predate `effective_auth_required`:
    // mirrors the builtin provider shapes and is intentionally frozen.
    let active = value
        .get("active_provider")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing active provider".to_string())?;
    let provider = value
        .get("saved_providers")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(active)
            })
        })
        .ok_or_else(|| "active provider is unavailable".to_string())?;
    let bool_field = |name: &str| {
        provider
            .get(name)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let provider_type = provider
        .get("provider_type")
        .and_then(serde_json::Value::as_str);
    if provider_type == Some("mock") {
        return Ok(true);
    }
    if provider_type == Some("aliyun") {
        return Ok(provider
            .get("auth_source")
            .and_then(serde_json::Value::as_str)
            == Some("ecs_ram_role")
            || (bool_field("has_access_key_id") && bool_field("has_access_key_secret")));
    }
    Ok(bool_field("has_api_key"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_active_openai_and_aliyun_credentials() {
        let openai = serde_json::json!({
            "active_provider": "prod",
            "saved_providers": [{
                "provider_id": "prod",
                "provider_type": "openai_compat",
                "has_api_key": true
            }]
        });
        let aliyun = serde_json::json!({
            "active_provider": "ecs",
            "saved_providers": [{
                "provider_id": "ecs",
                "provider_type": "aliyun",
                "auth_source": "ecs_ram_role"
            }]
        });

        assert_eq!(auth_state_is_configured(&openai), Ok(true));
        assert_eq!(auth_state_is_configured(&aliyun), Ok(true));
    }

    #[test]
    fn rejects_missing_or_incomplete_active_credentials() {
        let incomplete = serde_json::json!({
            "active_provider": "prod",
            "saved_providers": [{
                "provider_id": "prod",
                "provider_type": "openai_compat",
                "has_api_key": false
            }]
        });

        assert_eq!(auth_state_is_configured(&incomplete), Ok(false));
        assert!(auth_state_is_configured(&serde_json::json!({})).is_err());
    }

    #[test]
    fn recognizes_effective_env_only_auth_state() {
        let env_only = serde_json::json!({
            "active_provider": "gate4",
            "saved_providers": [],
            "effective_auth_required": false
        });

        assert_eq!(auth_state_is_configured(&env_only), Ok(true));
    }

    #[test]
    fn recognizes_effective_auth_required_as_unconfigured() {
        let unconfigured = serde_json::json!({
            "active_provider": "dashscope",
            "saved_providers": [],
            "effective_auth_required": true
        });

        assert_eq!(auth_state_is_configured(&unconfigured), Ok(false));
    }

    #[test]
    fn recognizes_mock_provider_from_legacy_auth_state() {
        let mock = serde_json::json!({
            "active_provider": "test",
            "saved_providers": [{
                "provider_id": "test",
                "provider_type": "mock",
                "has_api_key": false
            }]
        });

        assert_eq!(auth_state_is_configured(&mock), Ok(true));
    }
}
