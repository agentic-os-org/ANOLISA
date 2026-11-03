//! Invalid inputs and drift never reach a provider.

use super::*;

#[test]
fn later_malformed_selection_or_input_fails_before_any_call() {
    for provider in [false, true] {
        let core = Core::new().unwrap();
        let host = Host::new();
        let mut req = request(false);
        if provider {
            req.plan["steps"][1]["providers"][0]["provider_version"] = json!("unknown");
        } else {
            req.inputs.get_mut("step-1").unwrap().input["artifact"]["content"] =
                json!("changed source");
        }
        assert!(core.prepare(req, &host, 1000).is_err());
        assert!(host.invoked.is_empty());
    }
}

#[test]
fn named_unavailable_provider_is_not_an_optional_empty_route() {
    let core = Core::new().unwrap();
    let host = Host::new();
    let mut req = request(false);
    req.plan["steps"][0]["required"] = json!(false);
    req.plan["steps"][0]["on_failure"] = json!("record_gap_and_continue");
    req.plan["steps"][0]["providers"][0]["provider_id"] = json!("missing-provider");
    assert!(matches!(
        core.prepare(req, &host, 1000),
        Err(Error::ProviderUnavailable)
    ));
    assert!(host.invoked.is_empty());
}

#[test]
fn changed_descriptor_is_rejected_before_dispatch() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    host.descriptors.get_mut("fixture-provider").unwrap()["provider_version"] = json!("2");
    assert!(matches!(
        core.execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::ProviderChanged)
    ));
    assert!(host.invoked.is_empty());
}

#[test]
fn mismatched_receipt_input_or_output_stops_without_terminal_success() {
    for reply in [Reply::WrongReceipt, Reply::WrongInput, Reply::WrongOutput] {
        let core = Core::new().unwrap();
        let mut host = Host::new();
        host.replies = vec![reply];
        let mut journal = MemoryJournal::default();
        let prepared = core.prepare(request(false), &host, 1000).unwrap();
        assert!(matches!(
            core.execute(
                prepared,
                &mut host,
                &mut journal,
                &FixedClock(1100),
                &NeverCancel
            ),
            Err(Error::Contract(_))
        ));
        assert_eq!(host.invoked.len(), 1);
        assert!(journal
            .records
            .iter()
            .all(|r| r["kind"] != "execution_settled"));
        assert_eq!(journal.claims.len(), 1);
    }
}

#[test]
fn scopes_have_separate_claims_and_wrong_runtime_binding_is_rejected() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut journal = MemoryJournal::default();
    let first = core.prepare(request(false), &host, 1000).unwrap();
    let first_key = first.event_key().to_owned();
    core.execute(
        first,
        &mut host,
        &mut journal,
        &FixedClock(1100),
        &NeverCancel,
    )
    .unwrap();
    let mut req = request(false);
    req.plan["scope"]["session_id"] = json!("other-session");
    assert!(core.prepare(req, &host, 1000).is_err());
    let mut req = request(false);
    req.plan["scope"]["session_id"] = json!("other-session");
    req.runtime["session_id"] = json!("other-session");
    let second = core.prepare(req, &host, 1000).unwrap();
    assert_ne!(first_key, second.event_key());
    let result = core
        .execute(
            second,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(result.record()["scope"]["session_id"], "other-session");
    assert!(result
        .calls()
        .iter()
        .all(|call| call.result().receipt["scope"]["session_id"] == "other-session"));
    assert_eq!(journal.claims.len(), 2);
}
