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

    /// Read-only summary with the automatic-access identity probe.
    ///
    /// Only an explicit user action may run this: it performs the same
    /// read-only IMDS check as a real request but never activates a provider.
    pub(crate) fn guide_auth_probe(&self) -> Result<GuideAuthSummary, String> {
        let value = self.registry_query(
            "auth",
            "state",
            serde_json::json!({"probe_ecs_trial": true}),
        )?;
        guide_auth_summary_from_value(&value)
    }
}

fn guide_auth_summary_from_value(value: &serde_json::Value) -> Result<GuideAuthSummary, String> {
    let configured = auth_state_is_configured(value)?;
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
    let preferences = value.get("trial_preferences");
    let auto_use_unreadable = preferences.is_some_and(|prefs| prefs.get("error_code").is_some());
    let access = value.get("access");
    Ok(GuideAuthSummary {
        configured,
        service: active.map(str::to_string),
        source: provider
            .and_then(|provider| provider.get("source"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        // An unreadable preference is not the user turning automatic use off.
        auto_use: preferences
            .and_then(|prefs| prefs.get("auto_use"))
            .and_then(serde_json::Value::as_bool)
            .filter(|_| !auto_use_unreadable),
        auto_use_unreadable,
        access: access_path(access),
        probe: probe_state(access),
        probe_error_code: access
            .and_then(|access| access.get("probe_error_code"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

fn access_path(access: Option<&serde_json::Value>) -> AccessPath {
    match access
        .and_then(|access| access.get("path"))
        .and_then(serde_json::Value::as_str)
    {
        Some("explicit") => AccessPath::Explicit,
        Some("ecs_trial") => AccessPath::EcsTrial,
        Some("none") => AccessPath::None,
        _ => AccessPath::Unknown,
    }
}

fn probe_state(access: Option<&serde_json::Value>) -> ProbeState {
    match access
        .and_then(|access| access.get("probe"))
        .and_then(serde_json::Value::as_str)
    {
        Some("confirmed_supported") => ProbeState::ConfirmedSupported,
        Some("confirmed_unsupported") => ProbeState::ConfirmedUnsupported,
        Some("failed") => ProbeState::Failed,
        _ => ProbeState::NotChecked,
    }
}

/// Route a user-initiated question would take for credentials.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AccessPath {
    /// Core predates the `access` summary or reported an unknown route.
    #[default]
    Unknown,
    Explicit,
    EcsTrial,
    None,
}

/// How far the automatic-access identity probe got, if it ran at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProbeState {
    #[default]
    NotChecked,
    ConfirmedSupported,
    ConfirmedUnsupported,
    Failed,
}

/// Current AI service facts shown by the guide; never carries credentials.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GuideAuthSummary {
    pub(crate) configured: bool,
    pub(crate) service: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) auto_use: Option<bool>,
    pub(crate) auto_use_unreadable: bool,
    pub(crate) access: AccessPath,
    pub(crate) probe: ProbeState,
    pub(crate) probe_error_code: Option<String>,
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
            "trial_preferences": {"auto_use": true},
            "access": {"path": "explicit", "probe": "not_checked", "probe_error_code": null}
        });
        let summary = guide_auth_summary_from_value(&value).unwrap();
        assert!(summary.configured);
        assert_eq!(summary.service.as_deref(), Some("prod"));
        assert_eq!(summary.source.as_deref(), Some("user"));
        assert_eq!(summary.auto_use, Some(true));
        assert!(!summary.auto_use_unreadable);
        assert_eq!(summary.access, AccessPath::Explicit);
        assert_eq!(summary.probe, ProbeState::NotChecked);
        assert_eq!(summary.probe_error_code, None);
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
        // An old core without the access summary degrades to unknown facts.
        assert_eq!(summary.access, AccessPath::Unknown);
        assert_eq!(summary.probe, ProbeState::NotChecked);
    }

    #[test]
    fn guide_summary_parses_probe_states_and_unknown_paths() {
        for (probe, expected) in [
            ("not_checked", ProbeState::NotChecked),
            ("confirmed_supported", ProbeState::ConfirmedSupported),
            ("confirmed_unsupported", ProbeState::ConfirmedUnsupported),
            ("failed", ProbeState::Failed),
            ("unexpected", ProbeState::NotChecked),
        ] {
            let value = serde_json::json!({
                "active_provider": "dashscope",
                "saved_providers": [],
                "effective_auth_required": true,
                "access": {"path": "ecs_trial", "probe": probe}
            });
            let summary = guide_auth_summary_from_value(&value).unwrap();
            assert_eq!(summary.access, AccessPath::EcsTrial, "{probe}");
            assert_eq!(summary.probe, expected, "{probe}");
        }

        for (path, expected) in [
            ("explicit", AccessPath::Explicit),
            ("ecs_trial", AccessPath::EcsTrial),
            ("none", AccessPath::None),
            ("unknown", AccessPath::Unknown),
            ("unexpected", AccessPath::Unknown),
        ] {
            let value = serde_json::json!({
                "active_provider": "dashscope",
                "saved_providers": [],
                "effective_auth_required": true,
                "access": {"path": path, "probe": "not_checked"}
            });
            let summary = guide_auth_summary_from_value(&value).unwrap();
            assert_eq!(summary.access, expected, "{path}");
        }
    }

    #[test]
    fn guide_summary_reports_unreadable_preference_as_unknown_not_off() {
        let value = serde_json::json!({
            "active_provider": "dashscope",
            "saved_providers": [],
            "effective_auth_required": true,
            "trial_preferences": {"auto_use": false, "error_code": "trial_preferences_invalid"},
            "access": {"path": "unknown", "probe": "not_checked"}
        });
        let summary = guide_auth_summary_from_value(&value).unwrap();
        assert!(summary.auto_use_unreadable);
        assert_eq!(summary.auto_use, None);
        assert_eq!(summary.access, AccessPath::Unknown);
    }

    #[test]
    fn guide_summary_saved_trial_with_unreadable_preference_has_no_access_path() {
        // Core cannot decide the route without the preference, so a saved trial
        // provider comes back with an unknown path and the read error; the page
        // must show unknown rather than a working trial.
        let value = serde_json::json!({
            "active_provider": "ecs-free-trial",
            "saved_providers": [{
                "provider_id": "ecs-free-trial",
                "provider_type": "aliyun",
                "auth_source": "ecs_identity",
                "source": "user"
            }],
            "effective_auth_required": true,
            "trial_preferences": {"auto_use": false, "error_code": "trial_preferences_invalid"},
            "access": {"path": "unknown", "probe": "not_checked"}
        });
        let summary = guide_auth_summary_from_value(&value).unwrap();
        assert!(!summary.configured);
        assert_eq!(summary.service.as_deref(), Some("ecs-free-trial"));
        assert!(summary.auto_use_unreadable);
        assert_eq!(summary.auto_use, None);
        assert_eq!(summary.access, AccessPath::Unknown);
        assert_eq!(summary.probe, ProbeState::NotChecked);
    }

    #[test]
    fn guide_summary_preserves_runtime_and_system_sources() {
        for source in ["runtime", "system"] {
            let value = serde_json::json!({
                "active_provider": "managed",
                "saved_providers": [{
                    "provider_id": "managed",
                    "provider_type": "openai_compat",
                    "has_api_key": true,
                    "source": source
                }],
                "trial_preferences": {"auto_use": true},
                "access": {"path": "explicit", "probe": "not_checked"}
            });
            let summary = guide_auth_summary_from_value(&value).unwrap();
            assert_eq!(summary.source.as_deref(), Some(source));
        }
    }
}
