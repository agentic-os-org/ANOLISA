//! Effective AI credential state resolved through the cosh-core registry.

use super::CoshCoreAdapter;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StartupAuthHint {
    Ecs,
    Configure,
    #[default]
    Unknown,
}

impl CoshCoreAdapter {
    pub(crate) fn startup_auth_state(&self) -> Result<(bool, StartupAuthHint), String> {
        let value = self.registry_query("auth", "state", serde_json::Value::Null)?;
        let configured = auth_state_is_configured(&value)?;
        let auto_use = value["trial_preferences"]["auto_use"].as_bool();
        let non_ecs = cfg!(target_os = "macos").then_some(false);
        Ok((configured, startup_auth_hint(auto_use, non_ecs)))
    }

    pub(crate) fn startup_ecs_hint(&self) -> StartupAuthHint {
        self.registry_query(
            "auth",
            "prepare",
            serde_json::json!({"provider_type": "aliyun"}),
        )
        .ok()
        .map(|value| ecs_hint_from_metadata(&value))
        .unwrap_or_default()
    }

    /// Returns whether the active provider has usable credentials.
    ///
    /// cosh-core owns the per-provider semantics (env vars, user/system
    /// config layering, ECS RAM role exemption); this only transports the
    /// resolved boolean, so provider additions never touch the shell side.
    pub(crate) fn ai_configured(&self) -> Result<bool, String> {
        let value = self.registry_query("auth", "state", serde_json::Value::Null)?;
        auth_state_is_configured(&value)
    }

    /// Read-only AI service summary for the guide wizard.
    ///
    /// Uses the same `auth state` query as the startup probe without the ECS
    /// trial probe, so opening the wizard never triggers an ECS network call.
    pub(crate) fn guide_auth_summary(&self) -> Result<GuideAuthSummary, String> {
        let value = self.registry_query("auth", "state", serde_json::Value::Null)?;
        guide_auth_summary_from_value(&value)
    }
}

fn guide_auth_summary_from_value(value: &serde_json::Value) -> Result<GuideAuthSummary, String> {
    let configured = auth_state_is_configured(value)?;
    let preference_failed = value["trial_preferences"]["error_code"].as_str().is_some();
    let active = value
        .get("active_provider")
        .and_then(serde_json::Value::as_str);
    let provider = active.and_then(|id| {
        value
            .get("saved_providers")
            .and_then(serde_json::Value::as_array)
            .and_then(|providers| {
                providers.iter().find(|provider| {
                    provider
                        .get("provider_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(id)
                })
            })
    });
    Ok(GuideAuthSummary {
        configured,
        service: active.map(str::to_string),
        source: provider
            .and_then(|provider| provider.get("source"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        auto_use: (!preference_failed)
            .then(|| {
                value
                    .get("trial_preferences")
                    .and_then(|prefs| prefs.get("auto_use"))
                    .and_then(serde_json::Value::as_bool)
            })
            .flatten(),
        preference_failed,
        legacy_ecs_identity: value["ecs_trial"]["route"].is_null()
            && provider.is_some_and(|provider| provider["auth_source"] == "ecs_identity"),
        route: value["ecs_trial"]["route"].as_str().map(str::to_string),
        identity_status: value["ecs_trial"]["identity_status"]
            .as_str()
            .map(str::to_string),
    })
}

/// Current AI service facts shown by the guide; never carries credentials.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GuideAuthSummary {
    pub(crate) configured: bool,
    pub(crate) service: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) auto_use: Option<bool>,
    pub(crate) preference_failed: bool,
    pub(crate) legacy_ecs_identity: bool,
    pub(crate) route: Option<String>,
    pub(crate) identity_status: Option<String>,
}

fn startup_auth_hint(auto_use: Option<bool>, ecs: Option<bool>) -> StartupAuthHint {
    match (auto_use, ecs) {
        (Some(false), _) | (_, Some(false)) => StartupAuthHint::Configure,
        (Some(true), Some(true)) => StartupAuthHint::Ecs,
        _ => StartupAuthHint::Unknown,
    }
}

fn ecs_hint_from_metadata(value: &serde_json::Value) -> StartupAuthHint {
    match value["mode"].as_str() {
        Some("ecs_ram_role")
            if ["instance_id", "console_url"].iter().all(|key| {
                value[*key]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty())
            }) =>
        {
            StartupAuthHint::Ecs
        }
        _ => StartupAuthHint::Unknown,
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
    fn guide_legacy_identity_fact_uses_auth_source_not_provider_name() {
        for route in [serde_json::Value::Null, serde_json::json!("ecs_identity")] {
            let summary = guide_auth_summary_from_value(&serde_json::json!({
                "effective_auth_required": false,
                "active_provider": "custom-trial-42",
                "saved_providers": [{"provider_id": "custom-trial-42", "auth_source": "ecs_identity"}],
                "ecs_trial": {"route": route},
                "trial_preferences": {"auto_use": false, "error_code": "trial_preferences_invalid"}
            })).unwrap();
            assert!(summary.configured);
            assert_eq!(summary.legacy_ecs_identity, route.is_null());
            assert!(summary.preference_failed);
            assert_eq!(summary.auto_use, None);
        }
    }

    #[test]
    fn guide_preference_error_does_not_mean_user_disabled_auto_use() {
        let summary = guide_auth_summary_from_value(&serde_json::json!({
            "effective_auth_required": true,
            "trial_preferences": {"auto_use": false, "error_code": "trial_preferences_invalid"}
        }))
        .unwrap();
        assert_eq!(summary.auto_use, None);
    }

    #[test]
    fn startup_auth_hint_distinguishes_environment_and_auto_access() {
        for (auto_use, ecs, expected) in [
            (Some(true), Some(true), StartupAuthHint::Ecs),
            (Some(true), Some(false), StartupAuthHint::Configure),
            (Some(false), Some(true), StartupAuthHint::Configure),
            (Some(false), None, StartupAuthHint::Configure),
            (Some(true), None, StartupAuthHint::Unknown),
            (None, Some(true), StartupAuthHint::Unknown),
            (None, None, StartupAuthHint::Unknown),
        ] {
            assert_eq!(startup_auth_hint(auto_use, ecs), expected);
        }
    }

    #[test]
    fn startup_environment_uses_metadata_and_preserves_unknown() {
        for (value, expected) in [
            (
                serde_json::json!({"mode":"ecs_ram_role","instance_id":"i-test","console_url":"https://example.test"}),
                StartupAuthHint::Ecs,
            ),
            (
                serde_json::json!({"mode":"manual"}),
                StartupAuthHint::Unknown,
            ),
            (
                serde_json::json!({"mode":"ecs_ram_role"}),
                StartupAuthHint::Unknown,
            ),
            (
                serde_json::json!({"mode":"unknown"}),
                StartupAuthHint::Unknown,
            ),
            (serde_json::json!({}), StartupAuthHint::Unknown),
        ] {
            assert_eq!(ecs_hint_from_metadata(&value), expected);
        }
    }

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

    #[test]
    fn guide_summary_reports_service_source_and_auto_use_without_credentials() {
        let value = serde_json::json!({
            "active_provider": "prod",
            "saved_providers": [{
                "provider_id": "prod",
                "provider_type": "openai_compat",
                "has_api_key": true,
                "source": "user"
            }],
            "trial_preferences": {"auto_use": true}
        });
        let summary = guide_auth_summary_from_value(&value).unwrap();
        assert!(summary.configured);
        assert_eq!(summary.service.as_deref(), Some("prod"));
        assert_eq!(summary.source.as_deref(), Some("user"));
        assert_eq!(summary.auto_use, Some(true));
    }

    #[test]
    fn guide_summary_unconfigured_state_has_no_service() {
        let value = serde_json::json!({
            "active_provider": "dashscope",
            "saved_providers": [],
            "effective_auth_required": true
        });
        let summary = guide_auth_summary_from_value(&value).unwrap();
        assert!(!summary.configured);
        assert_eq!(summary.service.as_deref(), Some("dashscope"));
        assert_eq!(summary.source, None);
        assert_eq!(summary.auto_use, None);
    }
}
