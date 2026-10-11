//! Shared budgets, caller scheduling, cancellation and once-only claims.

use super::*;

#[test]
fn serial_steps_share_one_absolute_event_deadline_and_remaining_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    add_second_step(&mut document);
    let mut third = document["spec"]["events"]["tool.before"]["steps"][1].clone();
    third["id"] = json!("after-deadline");
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(third);
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(2000);
    document["spec"]["providers"]["policy"]["config"]["delays_ms"] =
        json!({"check": 300, "second": 4000});
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            event("tool.before", "observe"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let deadline = selected.deadline();
    let first = selected.invoke("check").unwrap();
    assert!(first.result.is_ok());
    let second = selected.invoke("second").unwrap();
    assert!(matches!(
        second.result,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
    ));
    assert!(Instant::now() < deadline + CLEANUP_ALLOWANCE);
    assert_eq!(first.record.event_id, second.record.event_id);
    assert_ne!(first.record.request_id, second.record.request_id);
    let first_budget = fixture.call(&first.record.request_id)["request"]["budget_ms"]
        .as_u64()
        .unwrap();
    let second_budget = fixture.call(&second.record.request_id)["request"]["budget_ms"]
        .as_u64()
        .unwrap();
    assert!(first_budget <= 2000 && second_budget < first_budget);
    let calls_before_expired_attempt = fixture.calls("invoke").len();
    let expired = selected.invoke("after-deadline").unwrap();
    assert!(matches!(
        expired.result,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
    ));
    assert!(expired.record.process.is_none());
    assert_eq!(fixture.calls("invoke").len(), calls_before_expired_attempt);
    fixture.assert_reaped();
}

#[test]
fn concurrently_scheduled_steps_retain_the_same_event_deadline() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    add_second_step(&mut document);
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(1500);
    document["spec"]["providers"]["policy"]["config"]["delays_ms"] =
        json!({"check": 4000, "second": 4000});
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            event("tool.before", "observe"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let deadline = selected.deadline();
    let (ready, first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| selected.invoke("check").unwrap());
        let ready = fixture.wait_for_calls("invoke", 1, deadline);
        let second = scope.spawn(|| selected.invoke("second").unwrap());
        (ready, first.join().unwrap(), second.join().unwrap())
    });
    assert!(ready);
    assert!(Instant::now() < deadline + CLEANUP_ALLOWANCE);
    for invocation in [&first, &second] {
        assert!(matches!(
            invocation.result,
            Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
        ));
        assert_eq!(invocation.record.event_id.as_deref(), Some(selected.id()));
    }
    assert_eq!(fixture.calls("invoke").len(), 2);
    assert_ne!(first.record.request_id, second.record.request_id);
    fixture.assert_reaped();
}

#[test]
fn cancellation_of_one_concurrent_event_does_not_cancel_its_neighbor() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&fixture.document());
    let cancelled = AtomicBool::new(false);
    let survivor_cancelled = AtomicBool::new(false);
    let cancelled_event = host
        .event(
            event("tool.before", "wait"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let mut survivor_value = event("tool.before", "wait");
    survivor_value["native"]["release"] = json!("survivor");
    survivor_value["tool"]["input"] = json!({"isolated": "survivor"});
    let survivor_event = host
        .event(
            survivor_value.clone(),
            Instant::now() + TIMEOUT,
            &survivor_cancelled,
        )
        .unwrap();
    let (ready, failed, survived) = thread::scope(|scope| {
        let failed = scope.spawn(|| cancelled_event.invoke("check").unwrap());
        let survived = scope.spawn(|| survivor_event.invoke("check").unwrap());
        let ready = fixture.wait_for_calls("invoke", 2, Instant::now() + Duration::from_secs(3));
        cancelled.store(true, Ordering::Release);
        let failed = failed.join().unwrap();
        fs::write(fixture.0.join("release-survivor"), b"go").unwrap();
        (ready, failed, survived.join().unwrap())
    });
    assert!(ready);
    assert!(matches!(
        failed.result,
        Err(Failure::Transport(aw_exec::Error::Cancelled))
    ));
    assert!(survived.result.is_ok());
    assert_ne!(failed.record.event_id, survived.record.event_id);
    assert_ne!(failed.record.request_id, survived.record.request_id);
    assert_eq!(
        fixture.call(&survived.record.request_id)["request"]["event"],
        survivor_value
    );
    fixture.assert_reaped();
}

#[test]
fn cancellation_before_preparation_event_or_invocation_never_spawns_a_child() {
    let fixture = Fixture::new();
    let document = fixture.document();
    let cancelled = AtomicBool::new(true);
    let result = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &cancelled,
    );
    assert!(matches!(
        result,
        Err(Error::Execution(aw_exec::Error::Cancelled))
    ));
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    let host = fixture.prepare(&document);
    assert!(matches!(
        host.event(
            event("tool.before", "allow"),
            Instant::now() + TIMEOUT,
            &cancelled
        ),
        Err(Error::Execution(aw_exec::Error::Cancelled))
    ));
    cancelled.store(false, Ordering::Release);
    let selected = host
        .event(
            event("tool.before", "allow"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    cancelled.store(true, Ordering::Release);
    let invocation = selected.invoke("check").unwrap();
    assert!(matches!(
        invocation.result,
        Err(Failure::Transport(aw_exec::Error::Cancelled))
    ));
    assert!(invocation.record.process.is_none());
    assert!(fixture.calls("invoke").is_empty());
    fixture.assert_reaped();
}

#[test]
fn preparation_has_its_own_shared_deadline_instead_of_spending_event_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(1);
    let host = fixture.prepare(&document);
    assert_eq!(host.preparation().len(), 2);
    document["spec"]["providers"]["policy"]["transport"]["argv"][4] = json!("slow-preparation");
    let deadline = Instant::now() + Duration::from_secs(2);
    let error = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities(),
        fixture.context(),
        deadline,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    let Error::Preparation(preparation) = error else {
        panic!("expected preparation history")
    };
    assert_eq!(preparation.completed.len(), 1);
    let Error::Call(call) = preparation.cause else {
        panic!("expected preparation deadline failure")
    };
    assert_eq!(call.record.method, Method::ValidateConfig);
    assert!(matches!(
        call.failure,
        Failure::Transport(aw_exec::Error::DeadlineExceeded)
    ));
    assert!(Instant::now() < deadline + CLEANUP_ALLOWANCE);
    fixture.assert_reaped();
}

#[test]
fn only_one_concurrent_claim_runs_a_step_even_when_that_attempt_fails() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&fixture.document());
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            event("tool.before", "invalid-json"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    assert!(matches!(selected.invoke("missing"), Err(Error::Invalid(_))));
    let results = thread::scope(|scope| {
        let first = scope.spawn(|| selected.invoke("check"));
        let second = scope.spawn(|| selected.invoke("check"));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in results {
        match result {
            Ok(invocation) => assert!(matches!(invocation.result, Err(Failure::Protocol(_)))),
            Err(error) => assert!(matches!(
                error,
                Error::Invalid("step was already claimed for this event")
            )),
        }
    }
    assert!(matches!(selected.invoke("check"), Err(Error::Invalid(_))));
    assert_eq!(fixture.calls("invoke").len(), 1);
    fixture.assert_reaped();
}

#[test]
fn provider_timeout_and_caller_deadline_can_each_tighten_the_event_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"]["policy"]["timeout_ms"] = json!(1000);
    document["spec"]["providers"]["policy"]["config"]["delays_ms"] = json!({"check": 4000});
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            event("tool.before", "allow"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("check").unwrap();
    assert!(matches!(
        invocation.result,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
    ));
    assert!(Instant::now() < selected.deadline());
    assert!(
        fixture.call(&invocation.record.request_id)["request"]["budget_ms"]
            .as_u64()
            .unwrap()
            <= 1000
    );
    let caller_deadline = Instant::now() + Duration::from_millis(500);
    let selected = host
        .event(event("tool.before", "allow"), caller_deadline, &cancelled)
        .unwrap();
    assert_eq!(selected.deadline(), caller_deadline);
    let invocation = selected.invoke("check").unwrap();
    assert!(matches!(
        invocation.result,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
    ));
    assert!(
        fixture.call(&invocation.record.request_id)["request"]["budget_ms"]
            .as_u64()
            .unwrap()
            <= 500
    );
    assert!(Instant::now() < caller_deadline + CLEANUP_ALLOWANCE);
    fixture.assert_reaped();
}
