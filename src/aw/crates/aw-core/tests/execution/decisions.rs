//! Ordered decisions and cancellation preserve host evidence.

use super::*;

#[test]
fn complete_plan_is_serial_and_candidate_is_not_adoption() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut journal = MemoryJournal::default();
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(result.record()["decision"], "proceed");
    assert_eq!(result.calls().len(), 2);
    assert_eq!(host.invoked[0]["plan_ref"]["step_id"], "step-0");
    assert_eq!(host.invoked[1]["plan_ref"]["step_id"], "step-1");
    assert!(
        result.record()["steps"][0]["settled_sequence"]
            .as_u64()
            .unwrap()
            < result.record()["steps"][1]["started_sequence"]
                .as_u64()
                .unwrap()
    );
    assert!(result.calls()[1]
        .result()
        .output
        .as_ref()
        .unwrap()
        .get("candidate")
        .is_some());
    assert!(result.record().get("adoption").is_none());
    assert!(journal.records.iter().all(|r| r.get("adoption").is_none()));
    assert_eq!(journal.records.last().unwrap()["kind"], "execution_settled");
    assert_eq!(
        result.journal_ack()["digest"],
        canonical::document_digest(journal.records.last().unwrap()).unwrap()
    );
}

#[test]
fn all_distinct_providers_are_called_and_references_are_complete() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut req = request(false);
    second_provider(&mut req, &mut host);
    let prepared = core.prepare(req, &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(result.calls().len(), 3);
    assert_eq!(
        result.record()["steps"][0]["invocations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_ne!(
        host.invoked[0]["invocation_id"],
        host.invoked[1]["invocation_id"]
    );
    assert_eq!(host.invoked[1]["provider_id"], "second-provider");
}

#[test]
fn command_deny_or_warn_survives_later_allow_and_skips_next_step() {
    for verdict in [Reply::Deny, Reply::Warn] {
        let core = Core::new().unwrap();
        let mut host = Host::new();
        host.replies = vec![verdict, Reply::Allow];
        let mut req = request(true);
        second_provider(&mut req, &mut host);
        let prepared = core.prepare(req, &host, 1000).unwrap();
        let result = core
            .execute(
                prepared,
                &mut host,
                &mut MemoryJournal::default(),
                &FixedClock(1100),
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(result.record()["decision"], "deny");
        assert_eq!(result.calls().len(), 2);
        assert_eq!(result.record()["steps"][1]["outcome"], "skipped");
        assert_eq!(
            result.calls()[1].result().output.as_ref().unwrap()["decision"]["verdict"],
            "allow"
        );
    }
}

#[test]
fn empty_optional_route_records_gap_but_required_route_preserves() {
    for required in [false, true] {
        let core = Core::new().unwrap();
        let mut host = Host::new();
        let mut req = request(false);
        req.plan["steps"][0]["providers"] = json!([]);
        req.plan["steps"][0]["required"] = json!(required);
        req.plan["steps"][0]["on_failure"] = json!(if required {
            "reject_plan"
        } else {
            "record_gap_and_continue"
        });
        let prepared = core.prepare(req, &host, 1000).unwrap();
        let result = core
            .execute(
                prepared,
                &mut host,
                &mut MemoryJournal::default(),
                &FixedClock(1100),
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(result.record()["steps"][0]["outcome"], "gap");
        assert_eq!(result.record()["steps"][0]["invocations"], json!([]));
        assert_eq!(
            result.record()["decision"],
            if required { "preserve" } else { "proceed" }
        );
        assert_eq!(host.invoked.len(), usize::from(!required));
    }
}

#[test]
fn cancellation_before_first_call_has_no_receipts() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &CancelFlag(Rc::new(Cell::new(true))),
        )
        .unwrap();
    assert_eq!(result.record()["decision"], "cancelled");
    assert_eq!(result.record()["steps"][1]["outcome"], "skipped");
    assert!(result.calls().is_empty());
}

#[test]
fn cancellation_inside_multi_provider_step_preserves_partial_evidence() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let flag = Rc::new(Cell::new(false));
    host.cancel_after_call = Some(flag.clone());
    let mut req = request(false);
    second_provider(&mut req, &mut host);
    let prepared = core.prepare(req, &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &CancelFlag(flag),
        )
        .unwrap();
    assert_eq!(result.record()["decision"], "cancelled");
    assert_eq!(result.record()["steps"][0]["outcome"], "cancelled");
    assert_eq!(
        result.record()["steps"][0]["invocations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(result.calls().len(), 1);
    assert_eq!(host.invoked.len(), 1);
}

#[test]
fn failed_receipt_is_a_visible_gap_and_transport_error_is_interrupted() {
    for reply in [Reply::Failed, Reply::Transport] {
        let core = Core::new().unwrap();
        let mut host = Host::new();
        host.replies = vec![reply];
        let mut journal = MemoryJournal::default();
        let prepared = core.prepare(request(false), &host, 1000).unwrap();
        let result = core.execute(
            prepared,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel,
        );
        if matches!(reply, Reply::Failed) {
            let result = result.unwrap();
            assert_eq!(result.record()["decision"], "preserve");
            assert_eq!(result.record()["steps"][0]["outcome"], "gap");
            assert!(result.calls()[0].result().output.is_none());
        } else {
            assert!(matches!(result, Err(Error::Host(_))));
            assert!(journal
                .records
                .iter()
                .all(|r| r["kind"] != "execution_settled"));
        }
        assert_eq!(host.invoked.len(), 1);
    }
}

#[test]
fn cancellation_during_final_host_call_cannot_return_proceed() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let flag = Rc::new(Cell::new(false));
    host.cancel_after_call = Some(flag.clone());
    let mut req = request(false);
    req.plan["steps"].as_array_mut().unwrap().remove(0);
    req.inputs.remove("step-0");
    let prepared = core.prepare(req, &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &CancelFlag(flag),
        )
        .unwrap();
    assert_eq!(result.record()["decision"], "cancelled");
    assert_eq!(result.record()["steps"][0]["outcome"], "cancelled");
    assert_eq!(result.calls().len(), 1);
    assert!(result.calls()[0]
        .result()
        .output
        .as_ref()
        .unwrap()
        .get("candidate")
        .is_some());
}
