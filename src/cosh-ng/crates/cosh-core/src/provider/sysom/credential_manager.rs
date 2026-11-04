//! Per-provider in-memory credential leases and refresh coordination.

use super::{
    ecs_identity::{IdentityClient, IdentityError},
    SysomCredentials,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex, Notify};
use tokio::time::Instant;

#[derive(Clone)]
pub(super) struct CredentialLease {
    pub(super) credentials: SysomCredentials,
    pub(super) generation: u64,
    pub(super) expires_at: Instant,
    pub(super) instance_id: Option<String>,
    refresh_at: Instant,
}

type RefreshResult = Option<Result<CredentialLease, IdentityError>>;
#[derive(Default)]
struct State {
    cached: Option<CredentialLease>,
    inflight: Option<watch::Receiver<RefreshResult>>,
    failure: Option<(IdentityError, Instant)>,
    generation: u64,
}

#[derive(Default)]
struct Waiters {
    count: AtomicUsize,
    changed: Notify,
}
struct WaiterGuard(Arc<Waiters>);
impl WaiterGuard {
    fn new(waiters: Arc<Waiters>) -> Self {
        waiters.count.fetch_add(1, Ordering::SeqCst);
        Self(waiters)
    }
}
impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.changed.notify_one();
        }
    }
}

pub(super) struct CredentialManager {
    waiters: Arc<Waiters>,
    client: Arc<IdentityClient>,
    state: Arc<Mutex<State>>,
}

impl CredentialManager {
    pub(super) fn new(configured_endpoint: String) -> Self {
        Self {
            waiters: Arc::default(),
            client: Arc::new(IdentityClient::new(configured_endpoint)),
            state: Arc::new(Mutex::new(State::default())),
        }
    }
    pub(super) fn with_admission(
        configured_endpoint: String,
        admission: super::ecs_identity::AdmissionCheck,
    ) -> Self {
        Self {
            waiters: Arc::default(),
            client: Arc::new(IdentityClient::with_admission(
                configured_endpoint,
                admission,
            )),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub(super) async fn check_admission(&self) -> Result<(), IdentityError> {
        self.client.check_admission().await
    }

    pub(super) async fn ensure(&self) -> Result<CredentialLease, IdentityError> {
        self.check_admission().await?;
        let _waiter = WaiterGuard::new(self.waiters.clone());
        loop {
            match self.ensure_once().await {
                Err(IdentityError::Cancelled) => continue,
                result => {
                    let lease = result?;
                    if let Err(error) = self.check_admission().await {
                        self.invalidate(lease.generation).await;
                        return Err(error);
                    }
                    if lease.expires_at.saturating_duration_since(Instant::now())
                        < super::ecs_identity::MINIMUM_SEND_TTL
                    {
                        self.invalidate(lease.generation).await;
                        return Err(IdentityError::LeaseExpired);
                    }
                    return Ok(lease);
                }
            }
        }
    }

    async fn ensure_once(&self) -> Result<CredentialLease, IdentityError> {
        let mut state = self.state.lock().await;
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
            state.generation = state
                .generation
                .max(state.cached.as_ref().map_or(0, |lease| lease.generation))
                + 1;
            let generation = state.generation;
            state.inflight = Some(receiver.clone());
            let shared = self.state.clone();
            let client = self.client.clone();
            let waiters = self.waiters.clone();
            // The refresh belongs to this provider, not to any one waiting request.
            tokio::spawn(async move {
                let exchange = client.exchange(Some(&waiters.count));
                tokio::pin!(exchange);
                let result = loop {
                    tokio::select! {
                        biased;
                        _=waiters.changed.notified()=>{if waiters.count.load(Ordering::SeqCst)==0 {break Err(IdentityError::Cancelled);}}
                        result=&mut exchange=>break result,
                    }
                };
                let mut state = shared.lock().await;
                let now = Instant::now();
                let result = match result {
                    Ok(issued) => {
                        let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 31;
                        let refresh_after = issued
                            .expires_at
                            .saturating_duration_since(now)
                            .saturating_sub(Duration::from_secs(120 + jitter));
                        let lease = CredentialLease {
                            credentials: issued.credentials,
                            generation,
                            expires_at: issued.expires_at,
                            instance_id: issued.instance_id,
                            refresh_at: now + refresh_after,
                        };
                        state.cached = Some(lease.clone());
                        state.failure = None;
                        Ok(lease)
                    }
                    Err(IdentityError::Cancelled) => Err(IdentityError::Cancelled),
                    Err(IdentityError::TrialDisabled) => {
                        state.cached = None;
                        state.failure = None;
                        Err(IdentityError::TrialDisabled)
                    }
                    Err(error) => {
                        if !error.is_transient() {
                            state.cached = None;
                        }
                        state.failure = Some((error, now + Duration::from_secs(5)));
                        fallback(&state, error, now)
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
                        Err(IdentityError::LeaseExpired)
                    }
                });
            }
            if receiver.changed().await.is_err() {
                return Err(IdentityError::Unavailable);
            }
        }
    }

    pub(super) async fn invalidate(&self, generation: u64) {
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
}

fn fallback(
    state: &State,
    error: IdentityError,
    now: Instant,
) -> Result<CredentialLease, IdentityError> {
    if error.is_transient() {
        if let Some(lease) = &state.cached {
            if lease.expires_at.saturating_duration_since(now) > Duration::from_secs(30) {
                return Ok(lease.clone());
            }
        }
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lease(generation: u64, ttl: u64) -> CredentialLease {
        CredentialLease {
            credentials: SysomCredentials {
                access_key_id: "STS.test".into(),
                access_key_secret: "secret".into(),
                security_token: Some("token".into()),
            },
            generation,
            expires_at: Instant::now() + Duration::from_secs(ttl),
            instance_id: None,
            refresh_at: Instant::now() + Duration::from_secs(ttl.saturating_sub(120)),
        }
    }
    #[tokio::test(start_paused = true)]
    async fn fresh_cache_is_idle_until_refresh_point() {
        let manager = CredentialManager::new("untrusted.invalid".into());
        manager.state.lock().await.cached = Some(lease(1, 900));
        assert_eq!(manager.ensure().await.unwrap().generation, 1);
        tokio::time::advance(Duration::from_secs(779)).await;
        assert_eq!(manager.ensure().await.unwrap().generation, 1);
    }
    #[tokio::test]
    async fn old_generation_failure_cannot_clear_new_credentials() {
        let manager = CredentialManager::new("untrusted.invalid".into());
        manager.state.lock().await.cached = Some(lease(2, 900));
        manager.invalidate(1).await;
        assert_eq!(
            manager
                .state
                .lock()
                .await
                .cached
                .as_ref()
                .unwrap()
                .generation,
            2
        );
        manager.invalidate(2).await;
        assert!(manager.state.lock().await.cached.is_none());
    }
    async fn manager(
        fixture: &super::super::identity_test_support::Fixture,
    ) -> Arc<CredentialManager> {
        Arc::new(CredentialManager {
            waiters: Arc::default(),
            client: Arc::new(fixture.client().await),
            state: Arc::new(Mutex::new(State::default())),
        })
    }
    #[tokio::test]
    async fn concurrent_waiters_share_refresh_and_cancelled_waiter_does_not_abort_it() {
        let fixture = super::super::identity_test_support::Fixture::start("Success", true).await;
        let manager = manager(&fixture).await;
        let initial = tokio::spawn({
            let manager = manager.clone();
            async move { manager.ensure().await }
        });
        fixture.wait_for_exchange().await;
        let mut waiters = Vec::new();
        for _ in 0..31 {
            let manager = manager.clone();
            waiters.push(tokio::spawn(async move { manager.ensure().await }));
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while manager.waiters.count.load(Ordering::SeqCst) < 32 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        initial.abort();
        let _ = initial.await;
        fixture.exchange_gate.add_permits(1);
        for waiter in waiters {
            let lease = waiter.await.unwrap().unwrap();
            assert_eq!(lease.generation, 1);
            assert_eq!(lease.credentials.access_key_id, "STS.test");
            assert!(lease.expires_at > Instant::now());
        }
        assert_eq!(
            fixture
                .requests
                .lock()
                .await
                .iter()
                .filter(|r| r.starts_with("POST /api/v1/identity/ecs-model/challenge"))
                .count(),
            1
        );
        assert_eq!(manager.ensure().await.unwrap().generation, 1);
    }
    #[tokio::test]
    async fn transient_failure_falls_back_only_with_safe_old_lease_and_shared_backoff() {
        let fixture = super::super::identity_test_support::Fixture::start(
            "Sysom.Identity.StsUnavailable",
            false,
        )
        .await;
        let manager = manager(&fixture).await;
        let mut old = lease(1, 100);
        old.refresh_at = Instant::now() - Duration::from_secs(1);
        manager.state.lock().await.cached = Some(old);
        assert_eq!(manager.ensure().await.unwrap().generation, 1);
        assert_eq!(manager.ensure().await.unwrap().generation, 1);
        assert_eq!(fixture.requests.lock().await.len(), 5);
        manager
            .state
            .lock()
            .await
            .cached
            .as_mut()
            .unwrap()
            .expires_at = Instant::now();
        assert!(matches!(
            manager.ensure().await,
            Err(IdentityError::StsUnavailable)
        ));
    }
    #[tokio::test]
    async fn identity_stability_metadata_timeout_preserves_safe_old_lease() {
        let fixture = super::super::identity_test_support::Fixture::start("Success", false).await;
        fixture.proof_gate.forget_permits(100);
        let manager = manager(&fixture).await;
        let mut old = lease(1, 100);
        old.refresh_at = Instant::now() - Duration::from_secs(1);
        manager.state.lock().await.cached = Some(old);
        let result = manager.ensure().await;
        assert!(
            result.is_ok(),
            "metadata timeout discarded valid lease: {:?}",
            result.err()
        );
        assert_eq!(manager.ensure().await.unwrap().generation, 1);
        assert_eq!(fixture.requests.lock().await.len(), 4);
        manager
            .state
            .lock()
            .await
            .cached
            .as_mut()
            .unwrap()
            .expires_at = Instant::now();
        assert!(
            matches!(manager.ensure().await, Err(error) if error.code() == "Sysom.Identity.MetadataUnavailable")
        );
    }

    #[tokio::test]
    async fn identity_stability_metadata_timeout_never_uses_nearly_expired_lease() {
        let fixture = super::super::identity_test_support::Fixture::start("Success", false).await;
        fixture.proof_gate.forget_permits(100);
        let manager = manager(&fixture).await;
        let mut old = lease(1, 30);
        old.refresh_at = Instant::now() - Duration::from_secs(1);
        manager.state.lock().await.cached = Some(old);
        assert!(
            matches!(manager.ensure().await, Err(error) if error.code() == "Sysom.Identity.MetadataUnavailable")
        );
        assert!(manager.ensure().await.is_err());
        assert_eq!(fixture.requests.lock().await.len(), 4);
    }

    #[tokio::test]
    async fn identity_stability_denied_or_invalid_metadata_clears_old_lease() {
        for (status, body) in [(403, "token-canary"), (200, "bad token-canary")] {
            let fixture =
                super::super::identity_test_support::Fixture::start("Success", false).await;
            *fixture.metadata_response.lock().await = Some(format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ));
            let manager = manager(&fixture).await;
            let mut old = lease(1, 100);
            old.refresh_at = Instant::now() - Duration::from_secs(1);
            manager.state.lock().await.cached = Some(old);
            assert!(matches!(
                manager.ensure().await,
                Err(IdentityError::Metadata)
            ));
            assert!(manager.state.lock().await.cached.is_none());
            let requests = fixture.requests.lock().await;
            assert_eq!(requests.len(), 2);
            assert!(!requests.iter().any(|r| r.contains("/credentials")));
        }
    }

    #[tokio::test]
    async fn permission_denial_does_not_fall_back_to_old_lease() {
        let fixture = super::super::identity_test_support::Fixture::start(
            "Sysom.Identity.PermissionDenied",
            false,
        )
        .await;
        let manager = manager(&fixture).await;
        let mut old = lease(1, 100);
        old.refresh_at = Instant::now() - Duration::from_secs(1);
        manager.state.lock().await.cached = Some(old);
        assert!(matches!(
            manager.ensure().await,
            Err(IdentityError::PermissionDenied)
        ));
        assert!(manager.state.lock().await.cached.is_none());
    }
    #[tokio::test]
    async fn invalidating_old_ticket_during_refresh_keeps_new_generation() {
        let fixture = super::super::identity_test_support::Fixture::start("Success", true).await;
        let manager = manager(&fixture).await;
        let mut old = lease(1, 100);
        old.refresh_at = Instant::now() - Duration::from_secs(1);
        manager.state.lock().await.cached = Some(old);
        let waiter = tokio::spawn({
            let manager = manager.clone();
            async move { manager.ensure().await }
        });
        fixture.wait_for_exchange().await;
        manager.invalidate(1).await;
        fixture.exchange_gate.add_permits(1);
        let new = waiter.await.unwrap().unwrap();
        assert_eq!(new.generation, 2);
        manager.invalidate(1).await;
        assert_eq!(manager.ensure().await.unwrap().generation, 2);
        let early = new.expires_at - Duration::from_secs(150);
        let late = new.expires_at - Duration::from_secs(120);
        assert!(new.refresh_at >= early && new.refresh_at <= late);
    }
    #[tokio::test]
    async fn delayed_waiter_never_receives_an_expired_lease() {
        let manager = CredentialManager::new("untrusted.invalid".into());
        let (_sender, receiver) = watch::channel(Some(Ok(lease(1, 0))));
        manager.state.lock().await.inflight = Some(receiver);
        assert!(matches!(
            manager.ensure().await,
            Err(IdentityError::LeaseExpired)
        ));
    }
    #[tokio::test]
    async fn cancelling_last_waiter_stops_before_next_identity_stage() {
        let fixture = super::super::identity_test_support::Fixture::start("Success", false).await;
        fixture
            .challenge_gate
            .acquire_many(100)
            .await
            .unwrap()
            .forget();
        let manager = manager(&fixture).await;
        let waiter = tokio::spawn({
            let manager = manager.clone();
            async move { manager.ensure().await }
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.requests.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        waiter.abort();
        let _ = waiter.await;
        fixture.challenge_gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(3), async {
            while manager.state.lock().await.inflight.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(fixture.requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn identity_combinations_cancel_during_metadata_retry_stops_all_requests() {
        use std::sync::atomic::AtomicBool;
        use tracing_subscriber::prelude::*;

        struct RetryObserver(Arc<AtomicBool>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RetryObserver {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                struct Retry(bool);
                impl tracing::field::Visit for Retry {
                    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
                        if field.name() == "retry" {
                            self.0 = value;
                        }
                    }
                    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {
                    }
                }
                let mut retry = Retry(false);
                event.record(&mut retry);
                if retry.0 {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
        }

        let retry_seen = Arc::new(AtomicBool::new(false));
        let subscriber = tracing_subscriber::registry().with(RetryObserver(retry_seen.clone()));
        // Aggregate live subscribers when parallel tests register the retry callsite.
        let _interest_guard = tracing::Dispatch::new(tracing_subscriber::registry());
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let fixture = super::super::identity_test_support::Fixture::start("Success", false).await;
        *fixture.metadata_response.lock().await =
            Some("HTTP/1.1 503 Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let manager = manager(&fixture).await;
        tokio::time::pause();
        // Keep network readiness from auto-advancing the paused retry clock.
        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let waiter = tokio::spawn({
            let manager = manager.clone();
            async move { manager.ensure().await }
        });
        let started = std::time::Instant::now();
        while !retry_seen.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "retry not reached"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.requests.lock().await.len(), 2);
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        assert_eq!(manager.waiters.count.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        while manager.state.lock().await.inflight.is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "refresh did not stop"
            );
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(5)).await;
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), 2, "cancelled refresh sent another request");
        assert!(requests[0].starts_with("POST /api/v1/identity/ecs-model/challenge "));
        assert!(requests[1].starts_with("PUT /latest/api/token "));
        let state = manager.state.lock().await;
        assert!(state.cached.is_none());
        assert!(state.failure.is_none());
        clock_guard.abort();
        assert!(clock_guard.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn identity_combinations_safe_fallback_signs_once_without_replaying_model() {
        use crate::provider::{
            ContentGenerator, DispatchState, GenerateConfig, GenerateEvent, Message,
            ProviderFailureKind,
        };
        use futures::StreamExt;

        for (status, content_type, body, expected) in [
            (200, "text/event-stream", "event: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"fallback-answer\"}}]}\n\n", None),
            (0, "", "", Some(ProviderFailureKind::Transport)),
            (403, "application/json", "{\"code\":\"SecurityTokenExpired\"}", Some(ProviderFailureKind::CredentialsExpired)),
            (200, "text/event-stream", "event: Failed\ndata: {\"code\":\"SecurityTokenExpired\"}\n\n", Some(ProviderFailureKind::CredentialsExpired)),
        ] {
            let fixture = super::super::identity_test_support::Fixture::start("Success", false).await;
            *fixture.metadata_response.lock().await =
                Some("HTTP/1.1 503 Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
            if status == 0 {
                fixture.model_response.lock().await.clear();
            } else {
                fixture.model(status, content_type, body).await;
            }
            let provider = fixture.provider().await;
            let super::super::CredentialSource::EcsIdentity(manager) = &provider.source else {
                panic!("identity source required");
            };
            let mut old = lease(17, 100);
            old.credentials.access_key_id = "STS.old".into();
            old.credentials.security_token = Some("old-token-canary".into());
            old.refresh_at = Instant::now() - Duration::from_secs(1);
            manager.state.lock().await.cached = Some(old);
            let events: Vec<_> = provider
                .generate(&[Message::user("hello")], &[], &GenerateConfig::default())
                .await
                .unwrap()
                .filter(|event| futures::future::ready(!matches!(event, GenerateEvent::RequestAccepted)))
                .collect()
                .await;
            if let Some(kind) = expected {
                assert!(matches!(events.as_slice(), [GenerateEvent::ProviderFailure(failure)] if failure.kind == kind && failure.dispatch == DispatchState::Unknown), "{events:?}");
            } else {
                assert!(matches!(events.as_slice(), [GenerateEvent::TextDelta(text), GenerateEvent::MessageEnd] if text == "fallback-answer"), "{events:?}");
            }
            let requests = fixture.requests.lock().await;
            assert_eq!(requests.len(), 4, "unexpected refresh or model replay");
            assert!(requests[0].contains("/ecs-model/challenge "));
            assert!(requests[1..3].iter().all(|request| request.starts_with("PUT /latest/api/token ")));
            assert!(requests[3].starts_with(&format!("POST {} ", super::super::API_PATH)));
            assert!(requests[3].contains("Credential=STS.old,"));
            assert!(requests[3].contains("x-acs-security-token: old-token-canary"));
            assert!(!requests.iter().any(|request| request.contains("/ecs-model/credentials ")));
            drop(requests);
            if expected != Some(ProviderFailureKind::CredentialsExpired) {
                fixture.model(200, "text/event-stream", "event: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"next-answer\"}}]}\n\n").await;
                let events: Vec<_> = provider.generate(&[Message::user("next")], &[], &GenerateConfig::default()).await.unwrap().collect().await;
                assert!(events.iter().any(|event| matches!(event, GenerateEvent::TextDelta(text) if text == "next-answer")));
                assert_eq!(fixture.requests.lock().await.len(), 5, "backoff retried metadata or duplicated the next model call");
            } else {
                assert!(manager.state.lock().await.cached.is_none());
            }
        }
    }
}
