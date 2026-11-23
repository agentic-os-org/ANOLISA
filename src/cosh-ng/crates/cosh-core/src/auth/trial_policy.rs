//! Pure ECS trial selection; capability validation and metadata probing belong to the caller.

use std::ffi::OsString;

use crate::config::{CoreConfig, ProviderConfig};

pub(crate) const MODEL: &str = "auto";

/// Selects the existing configuration, an ECS trial candidate, or no trial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrialDecision {
    Existing,
    EcsIdentity,
    Disabled,
}

/// Captures environment intent without storing credentials or endpoint values.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ProviderIntent {
    pub(crate) provider_set: bool,
    pub(crate) endpoint_set: bool,
    pub(crate) credentials_set: bool,
}

impl ProviderIntent {
    /// Takes a presence-only snapshot, including empty and non-Unicode values.
    pub(crate) fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Self {
        Self {
            provider_set: lookup("COSH_AI_PROVIDER").is_some(),
            endpoint_set: [
                "OPENAI_BASE_URL",
                "COSH_SYSOM_ENDPOINT",
                "COSH_SYSOM_VPC_PROXY_HOST",
            ]
            .into_iter()
            .any(|name| lookup(name).is_some()),
            credentials_set: [
                "DASHSCOPE_API_KEY",
                "OPENAI_API_KEY",
                "ALIBABA_CLOUD_ACCESS_KEY_ID",
                "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
                "ALIBABA_CLOUD_SECURITY_TOKEN",
            ]
            .into_iter()
            .any(|name| lookup(name).is_some()),
        }
    }
}

/// Decides a candidate without environment reads, filesystem access, or network probes.
pub(crate) fn decide(
    config: &CoreConfig,
    intent: &ProviderIntent,
    user_initiated: bool,
    auto_use_allowed: bool,
    persistent_auto_use: bool,
) -> TrialDecision {
    let selected = match config.ai.active_provider.as_deref() {
        Some(name) => match config.ai.providers.get(name) {
            Some(provider) => Some((name, provider)),
            None if config.ai.providers.is_empty() && matches!(name, "default" | "generic") => None,
            None => return TrialDecision::Existing,
        },
        None => match config.ai.providers.len() {
            0 => None,
            1 => config
                .ai
                .providers
                .iter()
                .next()
                .map(|(name, provider)| (name.as_str(), provider)),
            _ => return TrialDecision::Existing,
        },
    };
    let permitted = user_initiated && auto_use_allowed && persistent_auto_use;
    if let Some((_, provider)) = selected {
        if provider.auth_source.as_deref() == Some("ecs_identity") {
            if !permitted {
                return TrialDecision::Disabled;
            }
            return if provider.provider_type.as_deref() == Some("aliyun") {
                TrialDecision::EcsIdentity
            } else {
                TrialDecision::Existing
            };
        }
    }
    if intent.provider_set
        || intent.endpoint_set
        || intent.credentials_set
        || selected.is_some_and(|(name, provider)| configured_provider(name, provider))
    {
        return TrialDecision::Existing;
    }
    if permitted {
        TrialDecision::EcsIdentity
    } else {
        TrialDecision::Disabled
    }
}

fn configured_provider(name: &str, provider: &ProviderConfig) -> bool {
    !matches!(name, "default" | "generic")
        || !matches!(provider.provider_type.as_deref(), None | Some("generic"))
        || provider.auth_source.is_some()
        || provider.base_url.is_some()
        || provider.sysom_endpoint.is_some()
        || provider.api_key.is_some()
        || provider.access_key_id.is_some()
        || provider.access_key_secret.is_some()
        || provider.security_token.is_some()
        || provider.extra_params.is_some()
        || provider.explicit_cache.is_some()
}

#[cfg(test)]
mod tests;
