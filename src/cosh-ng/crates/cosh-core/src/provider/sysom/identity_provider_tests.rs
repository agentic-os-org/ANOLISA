use super::super::{DispatchState, GenerateEvent, ProviderFailureKind};
use super::identity_test_support::Fixture;
use super::*;

async fn generate(provider: &SysomProvider) -> Vec<GenerateEvent> {
    provider
        .generate(&[Message::user("hello")], &[], &GenerateConfig::default())
        .await
        .unwrap()
        .filter(|event| futures::future::ready(!matches!(event, GenerateEvent::RequestAccepted)))
        .collect()
        .await
}

#[tokio::test]
async fn identity_provider_signs_each_model_request_with_ensured_lease_without_role_fallback() {
    let fixture = Fixture::start("Success", false).await;
    fixture
        .model(
            200,
            "text/event-stream",
            "event: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"hello\"}}]}\n\n",
        )
        .await;
    let provider = fixture.provider().await;
    for _ in 0..2 {
        let events = generate(&provider).await;
        assert!(matches!(
            events.as_slice(),
            [GenerateEvent::TextDelta(_), GenerateEvent::MessageEnd]
        ));
    }
    let requests = fixture.requests.lock().await;
    assert_eq!(requests.len(), 7);
    for request in &requests[5..] {
        assert!(request.contains("Credential=STS.test,"));
        assert!(request.contains("x-acs-security-token: token-canary"));
        assert!(!request.contains("imds-token-canary"));
    }
    assert!(!requests
        .iter()
        .any(|r| r.contains("ram/security-credentials")));
}

#[tokio::test]
async fn exact_business_json_rejections_are_failure_only_for_both_http_statuses() {
    for status in [200, 403] {
        for (code, kind) in [
            (
                "Sysom.ServiceNotActivated",
                ProviderFailureKind::ActivationRequired,
            ),
            (
                "Sysom.CreditsPreparing",
                ProviderFailureKind::CreditsPreparing,
            ),
            (
                "Sysom.CreditsExhausted",
                ProviderFailureKind::QuotaExhausted,
            ),
        ] {
            let fixture = Fixture::start("Success", false).await;
            fixture
                .model(
                    status,
                    "application/json",
                    &serde_json::json!({"code":code,"message":"secret-canary"}).to_string(),
                )
                .await;
            let provider = fixture.provider().await;
            let events = generate(&provider).await;
            match events.as_slice() {
                [GenerateEvent::ProviderFailure(failure)] => {
                    assert_eq!(failure.code, code);
                    assert_eq!(failure.kind, kind);
                    assert_eq!(failure.dispatch, DispatchState::NotDispatched);
                }
                other => panic!("unexpected events {other:?}"),
            }
            assert_eq!(fixture.requests.lock().await.len(), 6);
        }
    }
}

#[tokio::test]
async fn identity_permission_denial_never_reaches_model_or_activation_flow() {
    let fixture = Fixture::start("Sysom.Identity.PermissionDenied", false).await;
    let provider = fixture.provider().await;
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.kind==ProviderFailureKind::PermissionDenied && failure.code=="Sysom.Identity.PermissionDenied")
    );
    assert_eq!(fixture.requests.lock().await.len(), 5);
}

#[tokio::test]
async fn unknown_transport_outcome_never_replays_model_request() {
    let fixture = Fixture::start("Success", false).await;
    *fixture.model_response.lock().await = String::new();
    let provider = fixture.provider().await;
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.kind==ProviderFailureKind::Transport && failure.dispatch==DispatchState::Unknown)
    );
    assert_eq!(fixture.requests.lock().await.len(), 6);
}

#[tokio::test]
async fn identity_model_redirect_never_forwards_signed_credentials() {
    let fixture = Fixture::start("Success", false).await;
    *fixture.model_response.lock().await=format!("HTTP/1.1 307 Redirect\r\nLocation: {}/api/v1/identity/ecs-model/challenge\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",fixture.endpoint);
    let provider = fixture.provider().await;
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.dispatch==DispatchState::Unknown)
    );
    assert_eq!(fixture.requests.lock().await.len(), 6);
}

#[tokio::test]
async fn expired_ticket_stops_current_http_or_sse_attempt_and_refreshes_only_next_call() {
    for content_type in ["application/json", "text/event-stream"] {
        let fixture = Fixture::start("Success", false).await;
        let payload = r#"{"code":"SecurityTokenExpired","message":"secret-canary"}"#;
        let body = if content_type == "application/json" {
            payload.into()
        } else {
            format!("event: Failed\ndata: {payload}\n\n")
        };
        fixture.model(200, content_type, &body).await;
        let provider = fixture.provider().await;
        let events = generate(&provider).await;
        assert!(
            matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.kind==ProviderFailureKind::CredentialsExpired && failure.dispatch==DispatchState::Unknown)
        );
        assert_eq!(fixture.requests.lock().await.len(), 6);
        fixture.model(200, "text/event-stream", "").await;
        assert!(matches!(
            generate(&provider).await.as_slice(),
            [GenerateEvent::MessageEnd]
        ));
        assert_eq!(fixture.requests.lock().await.len(), 12);
    }
}

#[tokio::test]
async fn truncated_json_body_is_transport_unknown_and_never_replayed() {
    let fixture = Fixture::start("Success", false).await;
    *fixture.model_response.lock().await="HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{".into();
    let provider = fixture.provider().await;
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.kind==ProviderFailureKind::Transport && failure.dispatch==DispatchState::Unknown)
    );
    assert_eq!(fixture.requests.lock().await.len(), 6);
}

#[tokio::test]
async fn successful_http_json_without_sse_content_type_cannot_be_empty_success() {
    let fixture = Fixture::start("Success", false).await;
    fixture
        .model(
            200,
            "text/plain",
            r#"{"code":"Sysom.ServiceNotActivated","success":false}"#,
        )
        .await;
    let provider = fixture.provider().await;
    assert!(
        matches!(generate(&provider).await.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.kind==ProviderFailureKind::ActivationRequired && failure.dispatch==DispatchState::NotDispatched)
    );
}

#[tokio::test]
async fn admission_closed_during_exchange_never_submits_model() {
    let fixture = Fixture::start("Success", true).await;
    let allowed = Arc::new(AtomicBool::new(true));
    let check = {
        let allowed = allowed.clone();
        Arc::new(move || allowed.load(Ordering::SeqCst))
    };
    let provider = Arc::new(fixture.provider_with_admission(check).await);
    let pending = tokio::spawn({
        let provider = provider.clone();
        async move { generate(&provider).await }
    });
    fixture.wait_for_exchange().await;
    allowed.store(false, Ordering::SeqCst);
    fixture.exchange_gate.add_permits(1);
    let events = pending.await.unwrap();
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="EcsTrialDisabled" && failure.dispatch==DispatchState::NotDispatched)
    );
    assert_eq!(fixture.requests.lock().await.len(), 5);
}

#[tokio::test]
async fn admission_closed_during_challenge_never_submits_metadata_or_redeem() {
    let fixture = Fixture::start("Success", false).await;
    fixture
        .challenge_gate
        .acquire_many(100)
        .await
        .unwrap()
        .forget();
    let allowed = Arc::new(AtomicBool::new(true));
    let check = {
        let allowed = allowed.clone();
        Arc::new(move || allowed.load(Ordering::SeqCst))
    };
    let provider = Arc::new(fixture.provider_with_admission(check).await);
    let pending = tokio::spawn({
        let provider = provider.clone();
        async move { generate(&provider).await }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.requests.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    allowed.store(false, Ordering::SeqCst);
    fixture.challenge_gate.add_permits(1);
    let events = pending.await.unwrap();
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="EcsTrialDisabled" && failure.kind==ProviderFailureKind::Identity)
    );
    assert_eq!(fixture.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn admission_closed_before_initial_or_cached_request_submits_nothing_new() {
    let fixture = Fixture::start("Success", false).await;
    let allowed = Arc::new(AtomicBool::new(false));
    let check = {
        let allowed = allowed.clone();
        Arc::new(move || allowed.load(Ordering::SeqCst))
    };
    let provider = fixture.provider_with_admission(check).await;
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="EcsTrialDisabled")
    );
    assert!(fixture.requests.lock().await.is_empty());
    allowed.store(true, Ordering::SeqCst);
    assert!(matches!(
        generate(&provider).await.as_slice(),
        [GenerateEvent::MessageEnd]
    ));
    assert_eq!(fixture.requests.lock().await.len(), 6);
    allowed.store(false, Ordering::SeqCst);
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="EcsTrialDisabled")
    );
    assert_eq!(fixture.requests.lock().await.len(), 6);
}

#[tokio::test]
async fn admission_model_send_guard_rechecks_after_credential_ensure() {
    let fixture = Fixture::start("Success", false).await;
    let allowed = Arc::new(AtomicBool::new(true));
    let check = {
        let allowed = allowed.clone();
        Arc::new(move || allowed.load(Ordering::SeqCst))
    };
    let provider = fixture.provider_with_admission(check).await;
    let CredentialSource::EcsIdentity(manager) = &provider.source else {
        panic!("expected identity source")
    };
    let lease = manager.ensure().await.unwrap();
    assert_eq!(fixture.requests.lock().await.len(), 5);
    allowed.store(false, Ordering::SeqCst);
    let events: Vec<_> = provider
        .send_streaming_request(
            b"{}",
            lease.credentials.clone(),
            Some(IdentityRequest::new(
                manager.clone(),
                &lease,
                tokio::time::Instant::now() + ecs_identity::PREPARATION_TIMEOUT,
            )),
        )
        .await
        .unwrap()
        .collect()
        .await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="EcsTrialDisabled" && failure.dispatch==DispatchState::NotDispatched)
    );
    assert_eq!(fixture.requests.lock().await.len(), 5);
}

#[tokio::test]
async fn admission_closed_during_proof_never_redeems_challenge() {
    let fixture = Fixture::start("Success", false).await;
    fixture.proof_gate.acquire_many(100).await.unwrap().forget();
    let allowed = Arc::new(AtomicBool::new(true));
    let check = {
        let allowed = allowed.clone();
        Arc::new(move || allowed.load(Ordering::SeqCst))
    };
    let provider = Arc::new(fixture.provider_with_admission(check).await);
    let pending = tokio::spawn({
        let provider = provider.clone();
        async move { generate(&provider).await }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.requests.lock().await.len() < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    allowed.store(false, Ordering::SeqCst);
    fixture.proof_gate.add_permits(1);
    let events = pending.await.unwrap();
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="EcsTrialDisabled")
    );
    assert_eq!(fixture.requests.lock().await.len(), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn admission_check_runs_off_the_async_runtime_thread() {
    let fixture = Fixture::start("Success", false).await;
    let runtime_thread = std::thread::current().id();
    let observed = Arc::new(AtomicBool::new(false));
    let check = {
        let observed = observed.clone();
        Arc::new(move || {
            assert_ne!(std::thread::current().id(), runtime_thread);
            observed.store(true, Ordering::SeqCst);
            true
        })
    };
    let provider = fixture.provider_with_admission(check).await;
    assert!(matches!(
        generate(&provider).await.as_slice(),
        [GenerateEvent::MessageEnd]
    ));
    assert!(observed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn request_accepted_arrives_before_the_first_sse_body_bytes() {
    let fixture = Fixture::start("Success", false).await;
    fixture.model_gate.acquire_many(100).await.unwrap().forget();
    fixture
        .model(
            200,
            "text/event-stream",
            "event: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"hello\"}}]}\n\n",
        )
        .await;
    let provider = fixture.provider().await;
    let mut stream = tokio::time::timeout(
        Duration::from_secs(3),
        provider.generate(&[Message::user("hello")], &[], &GenerateConfig::default()),
    )
    .await
    .unwrap()
    .unwrap();
    let first = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(matches!(first, Ok(Some(GenerateEvent::RequestAccepted))));
    fixture.model_gate.add_permits(1);
    assert!(matches!(
        stream.next().await,
        Some(GenerateEvent::TextDelta(_))
    ));
}

#[tokio::test]
async fn business_json_rejections_never_emit_request_accepted() {
    for status in [200, 403] {
        let fixture = Fixture::start("Success", false).await;
        fixture
            .model(
                status,
                "application/json",
                r#"{"code":"Sysom.ServiceNotActivated","success":false}"#,
            )
            .await;
        let provider = fixture.provider().await;
        let events: Vec<_> = provider
            .generate(&[Message::user("hello")], &[], &GenerateConfig::default())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(matches!(
            events.as_slice(),
            [GenerateEvent::ProviderFailure(_)]
        ));
    }
}

#[tokio::test]
async fn legacy_streaming_client_keeps_its_default_redirect_behavior() {
    let fixture = Fixture::start("Success", false).await;
    *fixture.model_response.lock().await=format!("HTTP/1.1 307 Redirect\r\nLocation: {}/latest/dynamic/instance-identity/document\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",fixture.endpoint);
    let endpoint = endpoint::ResolvedEndpoint {
        host: fixture.endpoint.trim_start_matches("http://").into(),
        scheme: "http".into(),
        origin: endpoint::EndpointOrigin::Configured,
    };
    let client =
        build_streaming_client(&endpoint, Duration::from_secs(1), Duration::from_secs(1)).unwrap();
    let response = client
        .post(format!("{}{API_PATH}", fixture.endpoint))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("i-test"));
}

#[tokio::test]
async fn old_sources_do_not_emit_the_new_admission_marker() {
    for role in [false, true] {
        let fixture = Fixture::start("Success", false).await;
        let mut provider = SysomProvider::new(
            "legacy-ak",
            "legacy-secret",
            Some("legacy-token"),
            &fixture.endpoint,
        );
        if role {
            provider.source = CredentialSource::EcsRamRole;
        }
        provider.instance_id.set(Some("i-test".into())).unwrap();
        let events: Vec<_> = provider
            .generate(&[Message::user("hello")], &[], &GenerateConfig::default())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(matches!(events.as_slice(), [GenerateEvent::MessageEnd]));
    }
}

#[tokio::test(start_paused = true)]
async fn slow_challenge_and_exchange_share_a_45_second_preparation_budget() {
    let fixture = Fixture::start("Success", true).await;
    fixture
        .challenge_gate
        .acquire_many(100)
        .await
        .unwrap()
        .forget();
    let client = fixture.client().await;
    let exchange = tokio::spawn(async move { client.exchange(None).await });
    while fixture.requests.lock().await.is_empty() && !exchange.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(!exchange.is_finished());
    tokio::time::advance(Duration::from_secs(8)).await;
    fixture.challenge_gate.add_permits(1);
    while fixture.requests.lock().await.len() < 5 && !exchange.is_finished() {
        tokio::task::yield_now().await;
    }
    assert_eq!(fixture.requests.lock().await.len(), 5);
    tokio::time::advance(Duration::from_secs(9)).await;
    fixture.exchange_gate.add_permits(1);
    while !exchange.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(exchange.await.unwrap().is_ok());
    assert_eq!(fixture.requests.lock().await.len(), 5);
}

#[tokio::test]
async fn locally_expired_lease_after_ensure_is_refused_before_model_send() {
    let fixture = Fixture::start("Success", false).await;
    let provider = fixture.provider().await;
    let CredentialSource::EcsIdentity(manager) = &provider.source else {
        panic!("identity source")
    };
    let lease = manager.ensure().await.unwrap();
    tokio::time::pause();
    let context = IdentityRequest::new(
        manager.clone(),
        &lease,
        tokio::time::Instant::now() + Duration::from_secs(45),
    );
    tokio::time::advance(Duration::from_secs(901)).await;
    let result = provider
        .send_streaming_request(b"{}", lease.credentials, Some(context))
        .await;
    assert!(matches!(result, Err(SysomRequestError::LeaseExpired)));
    assert_eq!(fixture.requests.lock().await.len(), 5);
}

struct BlockingAdmission {
    armed: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl BlockingAdmission {
    fn new() -> Self {
        Self {
            armed: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(AtomicBool::new(false)),
            release: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
        }
    }
    fn check(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let armed = self.armed.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        Arc::new(move || {
            if armed.swap(false, Ordering::SeqCst) {
                entered.store(true, Ordering::SeqCst);
                let (lock, notify) = &*release;
                let mut ready = lock.lock().unwrap();
                while !*ready {
                    ready = notify.wait(ready).unwrap();
                }
            }
            true
        })
    }
    fn unblock(&self) {
        let (lock, notify) = &*self.release;
        *lock.lock().unwrap() = true;
        notify.notify_all();
    }
}

#[tokio::test]
async fn final_model_guard_wait_rechecks_five_seconds_of_lease_safety() {
    let fixture = Fixture::start("Success", false).await;
    fixture.credential_ttl.store(6, Ordering::SeqCst);
    let admission = BlockingAdmission::new();
    let provider = Arc::new(fixture.provider_with_admission(admission.check()).await);
    let CredentialSource::EcsIdentity(manager) = &provider.source else {
        panic!("identity source")
    };
    let lease = manager.ensure().await.unwrap();
    let context = IdentityRequest::new(
        manager.clone(),
        &lease,
        tokio::time::Instant::now() + Duration::from_secs(45),
    );
    admission.armed.store(true, Ordering::SeqCst);
    tokio::time::pause();
    let send = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .send_streaming_request(b"{}", lease.credentials, Some(context))
                .await
        }
    });
    while !admission.entered.load(Ordering::SeqCst) && !send.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(!send.is_finished());
    tokio::time::advance(Duration::from_secs(2)).await;
    admission.unblock();
    while !send.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        send.await.unwrap(),
        Err(SysomRequestError::LeaseExpired)
    ));
    assert_eq!(fixture.requests.lock().await.len(), 5);
}

#[tokio::test]
async fn lease_expiring_during_final_manager_guard_refreshes_before_one_model_submission() {
    let fixture = Fixture::start("Success", true).await;
    fixture.credential_ttl.store(6, Ordering::SeqCst);
    let admission = BlockingAdmission::new();
    let provider = Arc::new(fixture.provider_with_admission(admission.check()).await);
    let pending = tokio::spawn({
        let provider = provider.clone();
        async move { generate(&provider).await }
    });
    fixture.wait_for_exchange().await;
    admission.armed.store(true, Ordering::SeqCst);
    fixture.exchange_gate.add_permits(1);
    while !admission.entered.load(Ordering::SeqCst) && !pending.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(!pending.is_finished());
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    fixture.credential_ttl.store(900, Ordering::SeqCst);
    fixture.exchange_gate.add_permits(1);
    admission.unblock();
    while !pending.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        pending.await.unwrap().as_slice(),
        [GenerateEvent::MessageEnd]
    ));
    let requests = fixture.requests.lock().await;
    assert_eq!(requests.len(), 11);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with(&format!("POST {API_PATH}")))
            .count(),
        1
    );
}

#[tokio::test]
async fn insufficient_local_ttl_is_reacquired_at_most_three_times_without_model_dispatch() {
    let fixture = Fixture::start("Success", false).await;
    fixture.credential_ttl.store(4, Ordering::SeqCst);
    let provider = fixture.provider().await;
    let events = generate(&provider).await;
    assert!(
        matches!(events.as_slice(),[GenerateEvent::ProviderFailure(failure)] if failure.code=="Sysom.Identity.LeaseExpired" && failure.dispatch==DispatchState::NotDispatched)
    );
    let requests = fixture.requests.lock().await;
    assert_eq!(requests.len(), 15);
    assert!(!requests
        .iter()
        .any(|request| request.starts_with(&format!("POST {API_PATH}"))));
}

#[tokio::test]
async fn kongche_legacy_auth_rejections_keep_the_reauthentication_error_path() {
    for role in [false, true] {
        for status in [401, 403] {
            for code in ["AccessDenied", "Forbidden"] {
                let fixture = Fixture::start("Success", false).await;
                fixture
                    .model(
                        status,
                        "application/json",
                        &serde_json::json!({"code":code}).to_string(),
                    )
                    .await;
                let mut provider = SysomProvider::new(
                    "legacy-ak",
                    "legacy-secret",
                    Some("legacy-token"),
                    &fixture.endpoint,
                );
                if role {
                    provider.source = CredentialSource::EcsRamRole;
                }
                provider.instance_id.set(Some("i-test".into())).unwrap();
                let result = provider
                    .generate(&[Message::user("hello")], &[], &GenerateConfig::default())
                    .await;
                let Err(error) = result else {
                    panic!("legacy auth rejection became an event stream")
                };
                assert!(error.contains(&status.to_string()), "{error}");
                assert_eq!(fixture.requests.lock().await.len(), 1);
            }
        }
    }
}
