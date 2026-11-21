//! Local logout commit plus separately reported remote revoke.

use std::sync::Arc;

use super::client::OAuthClient;
use super::contract::OAuthError;
use super::manager::OAuthCredentialManager;
use super::store::OAuthStore;

/// Login removed by a local logout commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedLogin {
    /// Removed generation.
    pub generation: u64,
    /// Application id of the removed login.
    pub client_id: String,
}

/// Local and remote logout results, reported separately.
#[derive(Clone, Debug, Default)]
pub struct LogoutReport {
    /// Login removed locally, if any.
    pub removed: Option<RemovedLogin>,
    /// Whether the remote revoke succeeded.
    pub remote_revoked: bool,
    /// Non-sensitive remote failure category.
    pub remote_error: Option<OAuthError>,
    /// A late logout whose generation no longer matches commits nothing.
    pub superseded: bool,
}

/// Commits local invalidation first, then attempts remote revoke without
/// ever revoking a token a winning session still uses.
pub async fn run_logout(
    client: Arc<OAuthClient>,
    store: Arc<OAuthStore>,
    manager: Option<Arc<OAuthCredentialManager>>,
    expected_generation: Option<u64>,
) -> Result<LogoutReport, OAuthError> {
    if let Some(manager) = &manager {
        manager.retire().await;
    }
    let store_read = store.clone();
    let previous = tokio::task::spawn_blocking(move || -> Result<_, OAuthError> {
        let state = store_read.read_state()?;
        let token = match &state.active {
            Some(login) => store_read.load_refresh(login.generation)?,
            None => None,
        };
        Ok((state.active, token))
    })
    .await
    .map_err(|_| OAuthError::Unavailable)??;
    if let Some(expected) = expected_generation {
        if previous.0.as_ref().map(|login| login.generation) != Some(expected) {
            return Ok(LogoutReport {
                superseded: true,
                ..Default::default()
            });
        }
    }
    let store_commit = store.clone();
    let removed = tokio::task::spawn_blocking(move || store_commit.commit_logout())
        .await
        .map_err(|_| OAuthError::Unavailable)??;
    let mut report = LogoutReport {
        removed: removed.as_ref().map(|login| RemovedLogin {
            generation: login.generation,
            client_id: login.client_id.clone(),
        }),
        ..Default::default()
    };
    let (Some(login), Some(token)) = (&removed, previous.1) else {
        return Ok(report);
    };
    let store_guard = store.clone();
    let current_token = tokio::task::spawn_blocking(move || -> Result<_, OAuthError> {
        let state = store_guard.read_state()?;
        match &state.active {
            Some(active) => store_guard.load_refresh(active.generation),
            None => Ok(None),
        }
    })
    .await
    .map_err(|_| OAuthError::Unavailable)??;
    if !should_revoke(&token, current_token.as_deref()) {
        report.remote_error = Some(OAuthError::GenerationChanged);
        return Ok(report);
    }
    match client.revoke(&login.client_id, &token).await {
        Ok(()) => report.remote_revoked = true,
        Err(error) => report.remote_error = Some(error),
    }
    Ok(report)
}

/// Never revoke a token that a winning session still uses.
fn should_revoke(old_token: &str, current_token: Option<&str>) -> bool {
    current_token != Some(old_token)
}

#[cfg(test)]
mod tests {
    use super::super::client::{TEST_IDENTITY_ENDPOINT, TEST_REVOKE_ENDPOINT};
    use super::super::store::{OAuthSecretBackend, OAuthStore};
    use super::super::test_support::{http_response, OAuthFixture};
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MemoryBackend {
        slots: StdMutex<std::collections::HashMap<String, String>>,
    }
    impl OAuthSecretBackend for MemoryBackend {
        fn set(&self, slot: &str, token: &str) -> Result<(), OAuthError> {
            self.slots.lock().unwrap().insert(slot.into(), token.into());
            Ok(())
        }
        fn get(&self, slot: &str) -> Result<Option<String>, OAuthError> {
            Ok(self.slots.lock().unwrap().get(slot).cloned())
        }
        fn delete(&self, slot: &str) -> Result<(), OAuthError> {
            self.slots.lock().unwrap().remove(slot);
            Ok(())
        }
    }

    async fn logged_in(
        fixture: &OAuthFixture,
    ) -> (tempfile::TempDir, Arc<OAuthStore>, Arc<OAuthClient>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(OAuthStore::new(
            dir.path(),
            Arc::new(MemoryBackend::default()),
        ));
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-canary", None)
            .unwrap();
        let client = TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
            TEST_REVOKE_ENDPOINT.sync_scope(format!("{}/v1/revoke", fixture.endpoint), || {
                Arc::new(OAuthClient::new("https://unused.invalid".into()))
            })
        });
        (dir, store, client)
    }

    #[tokio::test]
    async fn logout_commits_locally_and_revokes_remotely() {
        let fixture = OAuthFixture::start().await;
        let (_dir, store, client) = logged_in(&fixture).await;
        let report = run_logout(client, store.clone(), None, Some(1))
            .await
            .unwrap();
        assert_eq!(report.removed.as_ref().unwrap().generation, 1);
        assert!(report.remote_revoked);
        assert!(report.remote_error.is_none());
        let state = store.read_state().unwrap();
        assert!(!state.selected);
        assert!(state.active.is_none());
        assert!(store.load_refresh(1).unwrap().is_none());
        let requests = fixture.requests.lock().await;
        let revoke = requests
            .iter()
            .find(|request| request.contains("/v1/revoke"))
            .unwrap();
        assert!(revoke.contains("client_id=client-1"));
        assert!(revoke.contains("token=refresh-canary"));
    }

    #[tokio::test]
    async fn revoke_failure_keeps_the_local_logout() {
        let fixture = OAuthFixture::start().await;
        *fixture.revoke_response.lock().await = http_response("500 Internal Server Error", "");
        let (_dir, store, client) = logged_in(&fixture).await;
        let report = run_logout(client, store.clone(), None, Some(1))
            .await
            .unwrap();
        assert!(report.removed.is_some());
        assert!(!report.remote_revoked);
        assert_eq!(report.remote_error, Some(OAuthError::UnknownServer));
        assert!(store.read_state().unwrap().active.is_none());
    }

    #[tokio::test]
    async fn logout_without_login_is_a_noop() {
        let fixture = OAuthFixture::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(OAuthStore::new(
            dir.path(),
            Arc::new(MemoryBackend::default()),
        ));
        let client = TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
            TEST_REVOKE_ENDPOINT.sync_scope(format!("{}/v1/revoke", fixture.endpoint), || {
                Arc::new(OAuthClient::new("https://unused.invalid".into()))
            })
        });
        let report = run_logout(client, store, None, None).await.unwrap();
        assert!(report.removed.is_none());
        assert!(!report.remote_revoked);
        assert!(fixture.requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn late_logout_cannot_touch_the_winning_session() {
        let fixture = OAuthFixture::start().await;
        let (_dir, store, client) = logged_in(&fixture).await;
        let current = store.read_state().unwrap();
        store
            .commit_login(&current, "client-2", "refresh-2", None)
            .unwrap();
        let report = run_logout(client, store.clone(), None, Some(1))
            .await
            .unwrap();
        assert!(report.superseded);
        assert!(report.removed.is_none());
        let state = store.read_state().unwrap();
        assert_eq!(state.active.as_ref().unwrap().generation, 2);
        assert_eq!(store.load_refresh(2).unwrap().as_deref(), Some("refresh-2"));
        assert!(
            fixture.requests.lock().await.is_empty(),
            "no revoke for a superseded logout"
        );
    }

    #[tokio::test]
    async fn logout_of_the_current_session_revokes_even_a_reused_token() {
        let fixture = OAuthFixture::start().await;
        let (_dir, store, client) = logged_in(&fixture).await;
        let current = store.read_state().unwrap();
        store
            .commit_login(&current, "client-2", "refresh-canary", None)
            .unwrap();
        let report = run_logout(client, store.clone(), None, Some(2))
            .await
            .unwrap();
        assert!(report.removed.is_some());
        assert!(
            report.remote_revoked,
            "no winning session remains, so revoke is safe"
        );
        assert!(store.read_state().unwrap().active.is_none());
    }

    #[test]
    fn revoke_guard_protects_a_winning_session_token() {
        assert!(!should_revoke("token-a", Some("token-a")));
        assert!(should_revoke("token-a", None));
        assert!(should_revoke("token-a", Some("token-b")));
    }
}
