//! OAuth source integration: ACS3 reuse, failure mapping, no AK fallback.

use super::oauth::client::TEST_IDENTITY_ENDPOINT;
use super::oauth::contract::OAuthError;
use super::oauth::manager::OAuthCredentialManager;
use super::oauth::store::{OAuthSecretBackend, OAuthStore};
use super::oauth::test_support::{error_envelope, http_response, OAuthFixture};
use super::*;
use futures::StreamExt;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Default)]
struct MemoryBackend {
    slots: StdMutex<std::collections::HashMap<String, String>>,
}
impl OAuthSecretBackend for MemoryBackend {
    fn set(&self, slot: &str, refresh_token: &str) -> Result<(), OAuthError> {
        self.slots
            .lock()
            .unwrap()
            .insert(slot.to_owned(), refresh_token.to_owned());
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

struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<OAuthStore>,
    provider: SysomProvider,
}

async fn harness(fixture: &OAuthFixture, logged_in: bool) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(OAuthStore::new(
        dir.path(),
        Arc::new(MemoryBackend::default()),
    ));
    if logged_in {
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-canary", None)
            .unwrap();
    }
    let generation = store
        .read_state()
        .unwrap()
        .active
        .map(|login| login.generation)
        .unwrap_or(1);
    let client = TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
        Arc::new(super::oauth::client::OAuthClient::new(
            "https://unused.invalid".into(),
        ))
    });
    let manager = Arc::new(OAuthCredentialManager::new(
        client,
        store.clone(),
        generation,
    ));
    let provider = TEST_COPILOT_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
        SysomProvider::from_oauth(
            manager,
            &format!("https://{}", super::oauth::contract::bound_host()),
        )
        .unwrap()
    });
    Harness {
        _dir: dir,
        store,
        provider,
    }
}

async fn events(provider: &SysomProvider) -> Vec<GenerateEvent> {
    provider
        .generate(&[Message::user("hello")], &[], &GenerateConfig::default())
        .await
        .unwrap()
        .collect()
        .await
}

#[tokio::test]
async fn streams_sse_signed_with_the_oauth_sts() {
    let fixture = OAuthFixture::start().await;
    let harness = harness(&fixture, true).await;
    let events = events(&harness.provider).await;
    assert!(
        matches!(events.as_slice(), [GenerateEvent::RequestAccepted, GenerateEvent::TextDelta(text), GenerateEvent::MessageEnd] if text == "oauth-answer"),
        "{events:?}"
    );
    let requests = fixture.requests.lock().await;
    let model = requests
        .iter()
        .find(|request| request.contains(super::API_PATH))
        .unwrap();
    assert!(model.contains("Credential=STS.test,"));
    assert!(model.contains("x-acs-security-token: token-canary"));
    assert!(model.contains("x-acs-action: GenerateCopilotStreamResponse"));
    let refreshes = requests
        .iter()
        .filter(|request| request.contains("/cosh-oauth/credentials"))
        .count();
    assert_eq!(refreshes, 1);
}

#[tokio::test]
async fn not_logged_in_never_touches_model_or_environment_ak() {
    let fixture = OAuthFixture::start().await;
    let harness = harness(&fixture, false).await;
    let events = events(&harness.provider).await;
    assert!(
        matches!(events.as_slice(), [GenerateEvent::ProviderFailure(failure)] if failure.code == "Cosh.OAuth.NotLoggedIn" && failure.kind == ProviderFailureKind::Identity && failure.dispatch == DispatchState::NotDispatched),
        "{events:?}"
    );
    assert!(fixture.requests.lock().await.is_empty());
}

#[tokio::test]
async fn permission_denied_maps_and_deactivates_but_keeps_selection() {
    let fixture = OAuthFixture::start().await;
    *fixture.credentials_response.lock().await =
        http_response("200 OK", &error_envelope("Sysom.Identity.PermissionDenied"));
    let harness = harness(&fixture, true).await;
    let events = events(&harness.provider).await;
    assert!(
        matches!(events.as_slice(), [GenerateEvent::ProviderFailure(failure)] if failure.code == "Sysom.Identity.PermissionDenied" && failure.kind == ProviderFailureKind::PermissionDenied),
        "{events:?}"
    );
    let state = harness.store.read_state().unwrap();
    assert!(state.active.is_none());
    assert!(state.selected, "deactivation must keep the OAuth selection");
    assert!(fixture
        .requests
        .lock()
        .await
        .iter()
        .all(|request| !request.contains(super::API_PATH)));
}

#[tokio::test]
async fn not_activated_pauses_with_activation_failure_and_keeps_login() {
    let fixture = OAuthFixture::start().await;
    *fixture.credentials_response.lock().await = http_response(
        "200 OK",
        &error_envelope("Sysom.Identity.SysomNotActivated"),
    );
    let harness = harness(&fixture, true).await;
    let events = events(&harness.provider).await;
    assert!(
        matches!(events.as_slice(), [GenerateEvent::ProviderFailure(failure)] if failure.kind == ProviderFailureKind::ActivationRequired),
        "{events:?}"
    );
    assert_eq!(
        harness.store.load_refresh(1).unwrap().as_deref(),
        Some("refresh-canary")
    );
}

#[tokio::test]
async fn expired_sts_invalidates_and_the_next_request_refreshes_without_replay() {
    let fixture = OAuthFixture::start().await;
    *fixture.model_response.lock().await = http_response(
        "403 Forbidden",
        "{\"code\":\"SecurityTokenExpired\",\"requestId\":\"req-1\"}",
    );
    let harness = harness(&fixture, true).await;
    let first = events(&harness.provider).await;
    assert!(
        matches!(first.as_slice(), [GenerateEvent::ProviderFailure(failure)] if failure.kind == ProviderFailureKind::CredentialsExpired),
        "{first:?}"
    );
    *fixture.model_response.lock().await =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"next-answer\"}}]}\n\n".into();
    let second = events(&harness.provider).await;
    assert!(second
        .iter()
        .any(|event| matches!(event, GenerateEvent::TextDelta(text) if text == "next-answer")));
    let requests = fixture.requests.lock().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("/cosh-oauth/credentials"))
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains(super::API_PATH))
            .count(),
        2
    );
}

#[tokio::test]
async fn transport_failure_is_not_replayed() {
    let fixture = OAuthFixture::start().await;
    fixture.model_response.lock().await.clear();
    let harness = harness(&fixture, true).await;
    let events = events(&harness.provider).await;
    assert!(
        matches!(events.as_slice(), [GenerateEvent::ProviderFailure(failure)] if failure.kind == ProviderFailureKind::Transport && failure.dispatch == DispatchState::Unknown),
        "{events:?}"
    );
    let requests = fixture.requests.lock().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains(super::API_PATH))
            .count(),
        1
    );
}

#[tokio::test]
async fn from_oauth_rejects_untrusted_copilot_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(OAuthStore::new(
        dir.path(),
        Arc::new(MemoryBackend::default()),
    ));
    let client = Arc::new(super::oauth::client::OAuthClient::new(
        "https://sysom.cn-hangzhou.aliyuncs.com".into(),
    ));
    let manager = Arc::new(OAuthCredentialManager::new(client, store, 1));
    for base in ["https://evil.example", "http://127.0.0.1:8080", "not a url"] {
        assert!(
            SysomProvider::from_oauth(manager.clone(), base).is_err(),
            "{base}"
        );
    }
    #[cfg(not(feature = "oauth-pre"))]
    assert!(SysomProvider::from_oauth(manager, "https://sysom.cn-hangzhou.aliyuncs.com").is_ok());
    #[cfg(feature = "oauth-pre")]
    {
        assert!(SysomProvider::from_oauth(
            manager.clone(),
            "https://sysom-pre.cn-hangzhou.aliyuncs.com"
        )
        .is_ok());
        assert!(
            SysomProvider::from_oauth(manager, "https://sysom.cn-hangzhou.aliyuncs.com").is_err()
        );
    }
}
