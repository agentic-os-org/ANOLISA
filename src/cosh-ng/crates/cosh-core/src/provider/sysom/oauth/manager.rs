//! On-demand OAuth STS refresh with login-generation isolation.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};
use tokio::time::Instant;

use super::super::SysomCredentials;
use super::client::OAuthClient;
use super::contract::{OAuthCredentials, OAuthError, REFRESH_MARGIN};
use super::store::OAuthStore;

const SAFE_FALLBACK_MARGIN: Duration = Duration::from_secs(30);
const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

/// In-memory STS lease bound to one login generation.
#[derive(Clone)]
pub struct OAuthLease {
    /// Short-lived model credentials.
    pub credentials: SysomCredentials,
    /// Login generation this lease belongs to.
    pub generation: u64,
    /// Monotonic expiry.
    pub expires_at: Instant,
    refresh_at: Instant,
}

impl std::fmt::Debug for OAuthLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthLease")
            .field("generation", &self.generation)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

type RefreshResult = Option<Result<OAuthLease, OAuthError>>;

struct State {
    login_generation: u64,
    retired: bool,
    deactivated: bool,
    cached: Option<OAuthLease>,
    inflight: Option<watch::Receiver<RefreshResult>>,
    failure: Option<(OAuthError, Instant)>,
}

/// Single-flight on-demand refresh with generation fencing.
pub struct OAuthCredentialManager {
    client: Arc<OAuthClient>,
    store: Arc<OAuthStore>,
    state: Arc<Mutex<State>>,
    login_generation: u64,
}

impl OAuthCredentialManager {
    /// Serves exactly one login generation for this provider instance.
    pub fn new(client: Arc<OAuthClient>, store: Arc<OAuthStore>, login_generation: u64) -> Self {
        Self {
            client,
            store,
            state: Arc::new(Mutex::new(State {
                login_generation,
                retired: false,
                deactivated: false,
                cached: None,
                inflight: None,
                failure: None,
            })),
            login_generation,
        }
    }

    /// Generation this manager was created for.
    pub fn login_generation(&self) -> u64 {
        self.login_generation
    }

    /// Returns a usable lease, refreshing once per generation when due.
    pub async fn ensure(&self) -> Result<OAuthLease, OAuthError> {
        let mut state = self.state.lock().await;
        if state.retired {
            return Err(OAuthError::NotLoggedIn);
        }
        if state.deactivated {
            return Err(OAuthError::PermissionDenied);
        }
        let now = Instant::now();
        if let Some(lease) = &state.cached {
            if now < lease.refresh_at && now < lease.expires_at {
                return Ok(lease.clone());
            }
        }
        if let Some((error, retry_at)) = state.failure {
            if now < retry_at {
                return fallback(&state, error, now);
            }
        }
        let mut receiver = if let Some(receiver) = &state.inflight {
            receiver.clone()
        } else {
            let (sender, receiver) = watch::channel(None);
            state.inflight = Some(receiver.clone());
            let shared = self.state.clone();
            let client = self.client.clone();
            let store = self.store.clone();
            let generation = state.login_generation;
            tokio::spawn(async move {
                let issued = refresh_once(client, store.clone(), generation).await;
                let mut state = shared.lock().await;
                let now = Instant::now();
                let result = if state.retired || state.login_generation != generation {
                    Err(OAuthError::GenerationChanged)
                } else {
                    match issued {
                        Ok(issued) => {
                            let lease = OAuthLease {
                                credentials: issued.credentials,
                                generation,
                                expires_at: issued.expires_at,
                                refresh_at: issued
                                    .expires_at
                                    .checked_sub(REFRESH_MARGIN)
                                    .unwrap_or(now),
                            };
                            state.cached = Some(lease.clone());
                            state.failure = None;
                            Ok(lease)
                        }
                        Err(OAuthError::PermissionDenied) => {
                            state.deactivated = true;
                            state.cached = None;
                            state.failure = None;
                            let _ = tokio::task::spawn_blocking(move || {
                                store.deactivate_generation(generation)
                            })
                            .await;
                            Err(OAuthError::PermissionDenied)
                        }
                        Err(error) => {
                            if !error.is_transient() {
                                state.cached = None;
                            }
                            state.failure = Some((error, now + FAILURE_BACKOFF));
                            fallback(&state, error, now)
                        }
                    }
                };
                state.inflight = None;
                sender.send_replace(Some(result));
            });
            receiver
        };
        drop(state);
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result.and_then(|lease| {
                    if lease.expires_at > Instant::now() {
                        Ok(lease)
                    } else {
                        Err(OAuthError::LeaseExpired)
                    }
                });
            }
            if receiver.changed().await.is_err() {
                return Err(OAuthError::Unavailable);
            }
        }
    }

    /// Business STS-expired errors invalidate only the matching generation.
    pub async fn invalidate(&self, generation: u64) {
        let mut state = self.state.lock().await;
        if state
            .cached
            .as_ref()
            .is_some_and(|lease| lease.generation == generation)
        {
            state.cached = None;
            state.failure = None;
        }
    }

    /// Logout or account switch: late refresh results must not resurrect login.
    pub async fn retire(&self) {
        let mut state = self.state.lock().await;
        state.retired = true;
        state.cached = None;
        state.failure = None;
    }
}

async fn refresh_once(
    client: Arc<OAuthClient>,
    store: Arc<OAuthStore>,
    generation: u64,
) -> Result<OAuthCredentials, OAuthError> {
    let token = tokio::task::spawn_blocking(move || store.load_refresh(generation))
        .await
        .map_err(|_| OAuthError::Unavailable)??;
    let Some(token) = token else {
        return Err(OAuthError::NotLoggedIn);
    };
    client.refresh(&token).await
}

fn fallback(state: &State, error: OAuthError, now: Instant) -> Result<OAuthLease, OAuthError> {
    if error.is_transient() {
        if let Some(lease) = &state.cached {
            if lease.expires_at.saturating_duration_since(now) > SAFE_FALLBACK_MARGIN {
                return Ok(lease.clone());
            }
        }
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::super::client::TEST_IDENTITY_ENDPOINT;
    use super::super::contract::OAuthError;
    use super::super::store::{OAuthSecretBackend, OAuthStore};
    use super::super::test_support::{error_envelope, http_response, OAuthFixture};
    use super::*;
    use std::sync::Mutex as StdMutex;

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
        manager: Arc<OAuthCredentialManager>,
    }

    async fn harness(fixture: &OAuthFixture, generation: u64) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryBackend::default());
        let store = Arc::new(OAuthStore::new(dir.path(), backend));
        for _ in 0..generation {
            let starting = store.read_state().unwrap();
            store
                .commit_login(&starting, "client-1", "refresh-canary", None)
                .unwrap();
        }
        let client = TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
            Arc::new(OAuthClient::new("https://unused.invalid".into()))
        });
        let manager = Arc::new(OAuthCredentialManager::new(
            client,
            store.clone(),
            generation,
        ));
        Harness {
            _dir: dir,
            store,
            manager,
        }
    }

    fn lease(generation: u64, ttl: u64) -> OAuthLease {
        OAuthLease {
            credentials: SysomCredentials {
                access_key_id: "STS.old".into(),
                access_key_secret: "old-secret".into(),
                security_token: Some("old-token".into()),
            },
            generation,
            expires_at: Instant::now() + Duration::from_secs(ttl),
            refresh_at: Instant::now() + Duration::from_secs(ttl.saturating_sub(60)),
        }
    }

    #[tokio::test]
    async fn concurrent_requests_share_one_refresh_per_generation() {
        let fixture = OAuthFixture::start().await;
        fixture
            .credentials_gate
            .acquire_many(100)
            .await
            .unwrap()
            .forget();
        let harness = harness(&fixture, 7).await;
        let mut waiters = Vec::new();
        for _ in 0..16 {
            let manager = harness.manager.clone();
            waiters.push(tokio::spawn(async move { manager.ensure().await }));
        }
        fixture.wait_for_credentials().await;
        fixture.credentials_gate.add_permits(1);
        for waiter in waiters {
            let lease = waiter.await.unwrap().unwrap();
            assert_eq!(lease.generation, 7);
            assert_eq!(lease.credentials.access_key_id, "STS.test");
        }
        assert_eq!(fixture.requests.lock().await.len(), 1);
        let bodies = fixture.bodies().await;
        assert_eq!(
            bodies[0],
            serde_json::json!({"grantType":"refresh_token","refreshToken":"refresh-canary"})
        );
        assert!(harness.manager.ensure().await.unwrap().expires_at > Instant::now());
        assert_eq!(
            fixture.requests.lock().await.len(),
            1,
            "fresh cache must stay idle"
        );
    }

    #[tokio::test]
    async fn refresh_margin_is_sixty_seconds_and_lease_is_server_relative() {
        let fixture = OAuthFixture::start().await;
        let harness = harness(&fixture, 1).await;
        let started = Instant::now();
        let lease = harness.manager.ensure().await.unwrap();
        let ttl = lease.expires_at.saturating_duration_since(started);
        assert!(ttl >= Duration::from_secs(899) && ttl <= Duration::from_secs(901));
        let margin = lease.expires_at.saturating_duration_since(lease.refresh_at);
        assert_eq!(margin, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn retire_during_inflight_refresh_discards_the_late_result() {
        let fixture = OAuthFixture::start().await;
        fixture
            .credentials_gate
            .acquire_many(100)
            .await
            .unwrap()
            .forget();
        let harness = harness(&fixture, 3).await;
        let waiter = tokio::spawn({
            let manager = harness.manager.clone();
            async move { manager.ensure().await }
        });
        fixture.wait_for_credentials().await;
        harness.manager.retire().await;
        fixture.credentials_gate.add_permits(1);
        assert!(matches!(
            waiter.await.unwrap(),
            Err(OAuthError::GenerationChanged)
        ));
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::NotLoggedIn)
        ));
        assert_eq!(
            harness
                .store
                .read_state()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .generation,
            3,
            "retire is manager-local; logout owns the state commit"
        );
        assert_eq!(
            harness.store.load_refresh(3).unwrap().as_deref(),
            Some("refresh-canary"),
            "retire must not delete the slot; logout owns slot cleanup"
        );
    }

    #[tokio::test]
    async fn permission_denied_deactivates_the_generation_and_stops_network() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await =
            http_response("200 OK", &error_envelope("Sysom.Identity.PermissionDenied"));
        let harness = harness(&fixture, 5).await;
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::PermissionDenied)
        ));
        assert!(harness.store.read_state().unwrap().active.is_none());
        assert!(harness.store.load_refresh(5).unwrap().is_none());
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::PermissionDenied)
        ));
        assert_eq!(
            fixture.requests.lock().await.len(),
            1,
            "deactivated generation must not retry"
        );
    }

    #[tokio::test]
    async fn transient_failure_keeps_login_and_falls_back_only_with_safe_margin() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await =
            http_response("200 OK", &error_envelope("Sysom.Identity.StsUnavailable"));
        let harness = harness(&fixture, 2).await;
        let mut old = lease(2, 100);
        old.refresh_at = Instant::now() - Duration::from_secs(1);
        harness.manager.state.lock().await.cached = Some(old);
        assert_eq!(harness.manager.ensure().await.unwrap().generation, 2);
        assert_eq!(harness.manager.ensure().await.unwrap().generation, 2);
        assert_eq!(
            fixture.requests.lock().await.len(),
            1,
            "backoff must be shared"
        );
        assert_eq!(
            harness.store.load_refresh(2).unwrap().as_deref(),
            Some("refresh-canary")
        );
        harness
            .manager
            .state
            .lock()
            .await
            .cached
            .as_mut()
            .unwrap()
            .expires_at = Instant::now();
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::StsUnavailable)
        ));
    }

    #[tokio::test]
    async fn not_activated_pauses_requests_but_preserves_the_login_slot() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await = http_response(
            "200 OK",
            &error_envelope("Sysom.Identity.SysomNotActivated"),
        );
        let harness = harness(&fixture, 4).await;
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::SysomNotActivated)
        ));
        assert_eq!(
            harness.store.load_refresh(4).unwrap().as_deref(),
            Some("refresh-canary")
        );
    }

    #[tokio::test]
    async fn missing_slot_is_not_logged_in_without_network() {
        let fixture = OAuthFixture::start().await;
        let dir = tempfile::tempdir().unwrap();
        let empty = Arc::new(OAuthStore::new(
            dir.path(),
            Arc::new(MemoryBackend::default()),
        ));
        let client = TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
            Arc::new(OAuthClient::new("https://unused.invalid".into()))
        });
        let manager = OAuthCredentialManager::new(client, empty, 9);
        assert!(matches!(
            manager.ensure().await,
            Err(OAuthError::NotLoggedIn)
        ));
        assert!(fixture.requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn refresh_success_only_updates_sts_and_never_rotates_the_slot() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await = http_response(
            "200 OK",
            &serde_json::json!({"code":"Success","success":true,"data":{
                "accessKeyId":"STS.new","accessKeySecret":"new-secret","securityToken":"new-token",
                "serverTime":"2026-01-01T00:00:00Z","expiration":"2026-01-01T00:15:00Z",
                "refreshToken":"rotated-canary"}})
            .to_string(),
        );
        let harness = harness(&fixture, 8).await;
        let lease = harness.manager.ensure().await.unwrap();
        assert_eq!(lease.credentials.access_key_id, "STS.new");
        assert_eq!(
            harness.store.load_refresh(8).unwrap().as_deref(),
            Some("refresh-canary"),
            "first version must not consume a rotated refresh token"
        );
    }

    #[tokio::test]
    async fn invalidate_only_clears_the_matching_generation() {
        let fixture = OAuthFixture::start().await;
        let harness = harness(&fixture, 6).await;
        harness.manager.state.lock().await.cached = Some(lease(6, 900));
        harness.manager.invalidate(5).await;
        assert!(harness.manager.state.lock().await.cached.is_some());
        harness.manager.invalidate(6).await;
        assert!(harness.manager.state.lock().await.cached.is_none());
    }

    #[tokio::test]
    async fn gateway_403_and_unknown_5xx_preserve_login_without_deactivation() {
        for response in [
            http_response("403 Forbidden", "generic gateway denial"),
            http_response("500 Internal Server Error", ""),
            http_response("200 OK", &error_envelope("Sysom.Identity.ProviderNotReady")),
            http_response("200 OK", &error_envelope("Sysom.Identity.BrandNewCode")),
        ] {
            let fixture = OAuthFixture::start().await;
            *fixture.credentials_response.lock().await = response;
            let harness = harness(&fixture, 2).await;
            let mut old = lease(2, 100);
            old.refresh_at = Instant::now() - Duration::from_secs(1);
            harness.manager.state.lock().await.cached = Some(old);
            assert_eq!(harness.manager.ensure().await.unwrap().generation, 2);
            assert_eq!(
                harness
                    .store
                    .read_state()
                    .unwrap()
                    .active
                    .as_ref()
                    .unwrap()
                    .generation,
                2,
                "only explicit envelope PermissionDenied may deactivate"
            );
            assert_eq!(
                harness.store.load_refresh(2).unwrap().as_deref(),
                Some("refresh-canary")
            );
        }
    }

    #[tokio::test]
    async fn late_permission_denied_after_retire_cannot_deactivate_state() {
        let fixture = OAuthFixture::start().await;
        fixture
            .credentials_gate
            .acquire_many(100)
            .await
            .unwrap()
            .forget();
        *fixture.credentials_response.lock().await =
            http_response("200 OK", &error_envelope("Sysom.Identity.PermissionDenied"));
        let harness = harness(&fixture, 4).await;
        let waiter = tokio::spawn({
            let manager = harness.manager.clone();
            async move { manager.ensure().await }
        });
        fixture.wait_for_credentials().await;
        harness.manager.retire().await;
        fixture.credentials_gate.add_permits(1);
        assert!(matches!(
            waiter.await.unwrap(),
            Err(OAuthError::GenerationChanged)
        ));
        assert_eq!(
            harness
                .store
                .read_state()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .generation,
            4,
            "a retired generation's late denial must not touch current state"
        );
    }

    #[tokio::test]
    async fn throttled_backoff_is_bounded_and_preserves_login() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await = http_response("429 Too Many Requests", "");
        let harness = harness(&fixture, 3).await;
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::Throttled)
        ));
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::Throttled)
        ));
        assert_eq!(
            fixture.requests.lock().await.len(),
            1,
            "backoff must suppress retries"
        );
        assert_eq!(
            harness.store.load_refresh(3).unwrap().as_deref(),
            Some("refresh-canary")
        );
    }

    /// Identity maps upstream invalid_grant to PermissionDenied (definitive)
    /// but invalid_client / unclassifiable / dependency faults to ProviderNotReady.
    #[tokio::test]
    async fn permission_denied_and_provider_not_ready_contrast() {
        for (code, deactivates) in [
            ("Sysom.Identity.PermissionDenied", true),
            ("Sysom.Identity.ProviderNotReady", false),
        ] {
            let fixture = OAuthFixture::start().await;
            *fixture.credentials_response.lock().await =
                http_response("200 OK", &error_envelope(code));
            let harness = harness(&fixture, 2).await;
            let error = harness.manager.ensure().await.unwrap_err();
            assert_eq!(error.code(), code);
            let state = harness.store.read_state().unwrap();
            assert!(state.selected, "selection survives both classifications");
            if deactivates {
                assert!(state.active.is_none());
                assert!(harness.store.load_refresh(2).unwrap().is_none());
            } else {
                assert_eq!(state.active.as_ref().unwrap().generation, 2);
                assert_eq!(
                    harness.store.load_refresh(2).unwrap().as_deref(),
                    Some("refresh-canary")
                );
            }
            let second = harness.manager.ensure().await.unwrap_err();
            assert_eq!(second.code(), code);
            assert_eq!(
                fixture.requests.lock().await.len(),
                1,
                "neither classification may retry immediately"
            );
        }
    }

    /// S2 delivery also classifies HTTP 200 success envelopes missing token
    /// fields as ProviderNotReady server-side; a raw invalid envelope must
    /// never adopt a ticket nor drop the local login or selection.
    #[tokio::test]
    async fn invalid_success_envelope_keeps_login_and_selection() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await = http_response(
            "200 OK",
            &serde_json::json!({"code":"Success","success":true,"data":{
                "accessKeyId":"STS.test","accessKeySecret":"secret-canary",
                "serverTime":"2026-01-01T00:00:00Z","expiration":"2026-01-01T00:15:00Z",
                "refreshToken":"refresh-canary"}})
            .to_string(),
        );
        let harness = harness(&fixture, 2).await;
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::InvalidResponse)
        ));
        let state = harness.store.read_state().unwrap();
        assert!(state.selected);
        assert_eq!(state.active.as_ref().unwrap().generation, 2);
        assert_eq!(
            harness.store.load_refresh(2).unwrap().as_deref(),
            Some("refresh-canary")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_lease_is_never_returned_to_a_delayed_waiter() {
        let fixture = OAuthFixture::start().await;
        let harness = harness(&fixture, 1).await;
        let (sender, receiver) = watch::channel(Some(Ok(lease(1, 0))));
        harness.manager.state.lock().await.inflight = Some(receiver);
        assert!(matches!(
            harness.manager.ensure().await,
            Err(OAuthError::LeaseExpired)
        ));
        drop(sender);
    }
}
