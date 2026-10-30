//! Submission policy uses the shared deadline and single-claim mechanism.

use super::*;

#[test]
fn input_submission_preserves_identity_and_shares_claims_and_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    let mut submit = document["spec"]["events"]["tool.before"].clone();
    submit["steps"][0]["operation"] = json!("submit");
    document["spec"]["events"]["input.submit"] = submit;
    let mut capabilities = capabilities();
    capabilities.events.insert(
        "input.submit".into(),
        vec!["observe".into(), "block".into()],
    );
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities,
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .unwrap();
    let cancelled = AtomicBool::new(false);
    let value = json!({"name":"input.submit", "agent":{"adapter":"qoder","binding_id":"target","instance_id":"instance"},
        "session_id":"session", "turn_id":"turn", "input":{"text":"user input"}, "native":{"scenario":"block"}});
    let event = host
        .event(value.clone(), Instant::now() + TIMEOUT, &cancelled)
        .unwrap();
    assert!(
        matches!(event.invoke("check").unwrap().result, Ok(aw_host::StepOutput::Provider(outcome)) if outcome.requests_block())
    );
    assert!(event.invoke("check").is_err());
    let requests = fixture.calls("invoke");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["request"]["event"], value);
    assert!(requests[0]["request"]["event"].get("tool").is_none());
    let cancelled_event = host
        .event(value.clone(), Instant::now() + TIMEOUT, &cancelled)
        .unwrap();
    cancelled.store(true, Ordering::Release);
    let attempt = cancelled_event.invoke("check").unwrap();
    assert!(attempt.result.is_err());
    assert_eq!(attempt.failure_action, Some(FailureAction::Block));
    cancelled.store(false, Ordering::Release);
    let expired = host
        .event(value, Instant::now() + Duration::from_millis(5), &cancelled)
        .unwrap();
    thread::sleep(Duration::from_millis(10));
    assert!(expired.invoke("check").unwrap().result.is_err());
    assert_eq!(fixture.calls("invoke").len(), 1);
    fixture.assert_reaped();
}
