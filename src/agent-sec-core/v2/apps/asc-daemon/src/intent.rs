//! Lazy composition of deployment-owned cloud configuration, isolated from other daemon features.

use asc_daemon_core::intent::IntentService;
use asc_model_client::provider::{ModelProvider, OpenAiCompatibleProvider, ProviderError};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Captures process configuration, deferring provider validation until the first intent request.
/// Configuration changes require a restart.
pub fn intent_from_environment() -> Arc<IntentService> {
    configured(|key| match std::env::var(key) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => Some(String::new()),
    })
}

fn configured(get: impl Fn(&str) -> Option<String>) -> Arc<IntentService> {
    let enabled = get("ASC_INTENT_ENABLED").as_deref() == Some("true");
    let model = get("ASC_INTENT_MODEL").unwrap_or_else(|| "deepseek-v4.1-flash".into());
    let base = get("ASC_INTENT_BASE_URL")
        .unwrap_or_else(|| "https://dashscope.aliyuncs.com/compatible-mode/v1".into());
    let key = get("DASHSCOPE_API_KEY").unwrap_or_default();
    // The default model reasons until max_tokens on some tasks and then returns no draft.
    let thinking = match get("ASC_INTENT_THINKING").as_deref() {
        None | Some("disabled") => Some(Thinking::Disabled),
        Some("provider") => Some(Thinking::ProviderDefault),
        Some(_) => None,
    };
    Arc::new(IntentService::new(
        Arc::new(LazyProvider {
            configuration: enabled
                .then_some(thinking)
                .flatten()
                .map(|thinking| (base, key, thinking)),
            provider: OnceLock::new(),
        }),
        model,
        || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                .unwrap_or(0)
        },
    ))
}

/// `ASC_INTENT_THINKING`: `disabled` (default) or `provider` for services without the extension.
#[derive(Clone, Copy)]
enum Thinking {
    Disabled,
    ProviderDefault,
}

struct LazyProvider {
    configuration: Option<(String, String, Thinking)>,
    provider: OnceLock<Result<OpenAiCompatibleProvider, ProviderError>>,
}

impl ModelProvider for LazyProvider {
    fn complete(
        &self,
        model: &str,
        system: &str,
        input: &str,
        deadline: Instant,
    ) -> Result<String, ProviderError> {
        let provider = self.provider.get_or_init(|| {
            let (base, key, thinking) = self
                .configuration
                .as_ref()
                .ok_or(ProviderError::InvalidConfiguration)?;
            let provider = OpenAiCompatibleProvider::new(base, key.clone())?;
            Ok(match thinking {
                Thinking::Disabled => provider.without_thinking(),
                Thinking::ProviderDefault => provider,
            })
        });
        match provider {
            Ok(provider) => provider.complete(model, system, input, deadline),
            Err(_) => Err(ProviderError::InvalidConfiguration),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_daemon_core::{
        PeerCredentials,
        intent::{IntentInput, IntentServiceError},
    };
    use std::time::Duration;

    #[test]
    fn disabled_missing_key_invalid_endpoint_and_thinking_fail_without_network() {
        for (enabled, base, key, thinking) in [
            ("false", "https://example.com", "key", None),
            ("invalid", "https://example.com", "key", None),
            ("true", "https://example.com", "", None),
            ("true", "http://example.com", "key", None),
            ("true", "https://example.com", "key", Some("off")),
        ] {
            let service = configured(|name| match name {
                "ASC_INTENT_ENABLED" => Some(enabled.into()),
                "ASC_INTENT_BASE_URL" => Some(base.into()),
                "DASHSCOPE_API_KEY" => Some(key.into()),
                "ASC_INTENT_THINKING" => thinking.map(Into::into),
                _ => None,
            });
            let result = service.propose(
                PeerCredentials::new(0, 0, 1),
                "request",
                IntentInput {
                    task: "Write a poem".into(),
                    ttl: 1800,
                },
                Instant::now() + Duration::from_secs(1),
                || false,
            );
            assert!(matches!(result, Err(IntentServiceError::Unavailable)));
        }
    }
}
