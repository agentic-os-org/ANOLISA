//! Shared OAuth source resolution and release-environment endpoint binding.

use std::path::Path;
use std::sync::Arc;

use crate::config::CoreConfig;
use crate::provider::sysom::oauth::resolve_source;
use crate::provider::sysom::oauth::store::{KeyringOAuthBackend, OAuthSecretBackend, OAuthStore};
pub(crate) use crate::provider::sysom::oauth::SourceState;

#[cfg(test)]
tokio::task_local! { pub(crate) static TEST_BACKEND: Arc<dyn OAuthSecretBackend>; }

/// Secret backend for the current build profile (test-injectable).
pub(crate) fn backend() -> Arc<dyn OAuthSecretBackend> {
    #[cfg(test)]
    if let Ok(backend) = TEST_BACKEND.try_with(Clone::clone) {
        return backend;
    }
    Arc::new(KeyringOAuthBackend)
}

/// Effective OAuth source for the live configuration directory.
pub(crate) fn effective(config: &CoreConfig) -> SourceState {
    effective_in(
        &crate::config::config_dir(),
        config.ai.active_provider.as_deref(),
    )
}

/// Resolution with an explicit directory; unreadable state keeps legacy sources.
pub(crate) fn effective_in(dir: &Path, active_provider: Option<&str>) -> SourceState {
    match resolve_source(dir, active_provider, backend()) {
        Ok(SourceState::Active {
            generation,
            client_id,
        }) => match crate::provider::sysom::oauth::contract::expected_client_id() {
            Some(expected) if client_id != expected => SourceState::NeedsLogin,
            _ => SourceState::Active {
                generation,
                client_id,
            },
        },
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "oauth state unreadable; keeping configured sources"
            );
            SourceState::NotSelected
        }
    }
}

/// Release-environment Identity/Copilot base for this build profile.
pub(crate) fn sysom_base() -> String {
    format!(
        "https://{}",
        crate::provider::sysom::oauth::contract::bound_host()
    )
}

/// Login-state store rooted at the live configuration directory.
pub(crate) fn store() -> Arc<OAuthStore> {
    Arc::new(OAuthStore::new(&crate::config::config_dir(), backend()))
}

/// Active login generation, if the OAuth source is effective.
pub(crate) fn current_generation(config: &CoreConfig) -> Option<u64> {
    match effective(config) {
        SourceState::Active { generation, .. } => Some(generation),
        _ => None,
    }
}

/// Send admission: an unbound session may bind the current generation once;
/// afterwards any change (logout, switch, re-login) rejects new admissions.
pub(crate) fn admission(
    bound: Option<u64>,
    current: Option<u64>,
) -> Result<(), crate::provider::sysom::oauth::contract::OAuthError> {
    use crate::provider::sysom::oauth::contract::OAuthError;
    match bound {
        None => Ok(()),
        Some(bound) if current == Some(bound) => Ok(()),
        _ => Err(OAuthError::GenerationChanged),
    }
}

/// Resume isolation: history may only load under the exact same generation.
/// Legacy unbound history never flows into a newly bound OAuth source.
pub(crate) fn resume_allowed(record: Option<u64>, current: Option<u64>) -> bool {
    record == current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_binds_once_then_rejects_every_change() {
        assert!(admission(None, None).is_ok());
        assert!(admission(None, Some(1)).is_ok());
        assert!(admission(Some(1), Some(1)).is_ok());
        assert!(admission(Some(1), Some(2)).is_err());
        assert!(admission(Some(1), None).is_err());
    }

    #[test]
    fn resume_requires_the_exact_generation() {
        assert!(resume_allowed(None, None));
        assert!(resume_allowed(Some(3), Some(3)));
        assert!(!resume_allowed(None, Some(1)));
        assert!(!resume_allowed(Some(1), None));
        assert!(!resume_allowed(Some(1), Some(2)));
    }
}
